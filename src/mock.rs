//! Deterministic demo collector. It speaks the same [`RawSample`] as Windows.
//!
//! The UI does not branch on this type. A build without the `mock` feature
//! cannot construct it, so a normal Windows release has no demo path.
//! Numbers are scripted. They are not measurements.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::collector::SystemProbe;
use crate::gpuagg::fold_engine_percents;
use crate::metric::MetricValue;
use crate::model::{
    CpuDetailSnapshot, CpuDetailState, GpuAdapter, GpuPresence, LogicalCpuId, LogicalCpuProcess,
    LogicalCpuSample, NamedRate, ProcessGpu, ProcessKey, ProcessNetMode, RawProcess, RawSample,
};

const STEP: Duration = Duration::from_millis(500);
const PROCESSORS: u32 = 8;
const TICKS_100NS: u64 = 5_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Normal,
    Many,
    Gaps,
    Special,
    /// Scripted 60-second story for history selection screenshots.
    History,
    /// Eight logical processors with different rankings.
    Cpus,
    /// Thirty-two logical processors, for the grid layout.
    Cpus32,
    /// Detailed analysis is refused. Overall CPU still runs.
    CpusDenied,
}

impl Scenario {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "normal" => Some(Self::Normal),
            "many" => Some(Self::Many),
            "gaps" => Some(Self::Gaps),
            "special" => Some(Self::Special),
            "history" => Some(Self::History),
            "cpus" => Some(Self::Cpus),
            "cpus32" => Some(Self::Cpus32),
            "cpus-denied" => Some(Self::CpusDenied),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    Live,
    Unavailable,
    Unsupported,
    /// A counter that stays put, so the rate is a real zero after the first interval.
    Zero,
}

#[derive(Clone, Debug)]
struct Actor {
    pid: u32,
    creation: u64,
    name: String,
    weight: f64,
    memory: u64,
    cpu: Reading,
    disk: Reading,
    net: Reading,
    gpu_engines: &'static [f64],
    born_at: u64,
    dies_at: Option<u64>,
}

#[derive(Clone, Debug)]
struct Accumulators {
    cpu: u64,
    disk_read: u64,
    disk_write: u64,
    net_send: u64,
    net_recv: u64,
}

pub struct MockProbe {
    scenario: Scenario,
    origin: Instant,
    step: u64,
    actors: Vec<Actor>,
    totals: HashMap<(u32, u64), Accumulators>,
    system_total: u64,
    system_busy: u64,
    cpu_detail: Arc<AtomicBool>,
}

impl MockProbe {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(scenario: Scenario) -> Self {
        let on = matches!(scenario, Scenario::Cpus | Scenario::Cpus32);
        Self::with_detail(scenario, Arc::new(AtomicBool::new(on)))
    }

    pub fn with_detail(scenario: Scenario, cpu_detail: Arc<AtomicBool>) -> Self {
        Self {
            scenario,
            origin: Instant::now(),
            step: 0,
            actors: actors(scenario),
            totals: HashMap::new(),
            system_total: 0,
            system_busy: 0,
            cpu_detail,
        }
    }
}

impl SystemProbe for MockProbe {
    fn probe(&mut self) -> RawSample {
        let step = self.step;
        self.step = self.step.saturating_add(1);
        let elapsed = STEP.saturating_mul(u32::try_from(step).unwrap_or(u32::MAX));
        let taken_at = self.origin + elapsed;
        let mut sample = RawSample::empty(taken_at, SystemTime::UNIX_EPOCH + elapsed);
        sample.logical_processors = MetricValue::Value(PROCESSORS);
        sample.gpu_presence = GpuPresence::Present;
        sample.process_net = ProcessNetMode::Attributed;

        let percent = system_cpu_percent(self.scenario, step);
        let total_delta = TICKS_100NS.saturating_mul(u64::from(PROCESSORS));
        let busy_delta = ((total_delta as f64) * percent / 100.0).round() as u64;
        self.system_total = self.system_total.saturating_add(total_delta);
        self.system_busy = self.system_busy.saturating_add(busy_delta);
        sample.system_total_100ns = MetricValue::Value(self.system_total);
        sample.system_busy_100ns = MetricValue::Value(self.system_busy);

        let memory_total = 16 * 1024 * 1024 * 1024;
        let used = system_memory_used(step, memory_total);
        sample.memory_total_bytes = MetricValue::Value(memory_total);
        sample.memory_available_bytes = MetricValue::Value(memory_total.saturating_sub(used));

        let live: Vec<&Actor> = self
            .actors
            .iter()
            .filter(|actor| actor.born_at <= step && actor.dies_at.is_none_or(|dies| step < dies))
            .collect();
        let weight_sum = live
            .iter()
            .filter(|actor| actor.cpu == Reading::Live)
            .map(|actor| actor.weight)
            .sum::<f64>()
            .max(1.0);

        for actor in &live {
            let mut process = RawProcess::new(actor.pid, 1, actor.name.clone());
            process.creation_time_100ns = Some(actor.creation);
            let key = (actor.pid, actor.creation);
            let slot = self.totals.entry(key).or_insert(Accumulators {
                cpu: 0,
                disk_read: 0,
                disk_write: 0,
                net_send: 0,
                net_recv: 0,
            });
            process.cpu_time_100ns = match actor.cpu {
                Reading::Live => {
                    let share = actor.weight / weight_sum;
                    slot.cpu = slot
                        .cpu
                        .saturating_add(((busy_delta as f64) * share).round() as u64);
                    MetricValue::Value(slot.cpu)
                }
                Reading::Zero => MetricValue::Value(slot.cpu),
                Reading::Unavailable => MetricValue::Unavailable,
                Reading::Unsupported => MetricValue::Unsupported,
            };
            let disk_rate = disk_bytes_per_step(step, actor.weight);
            process.disk_read_bytes = advance(&mut slot.disk_read, disk_rate, actor.disk);
            process.disk_write_bytes = advance(&mut slot.disk_write, disk_rate / 4, actor.disk);
            let net_rate = net_bytes_per_step(step, actor.weight);
            process.net_send_bytes = advance(&mut slot.net_send, net_rate / 3, actor.net);
            process.net_recv_bytes = advance(&mut slot.net_recv, net_rate, actor.net);

            let gpu = fold_engine_percents(actor.gpu_engines.iter().copied().map(|base| {
                (base * (0.65 + 0.35 * ((step as f64) / 9.0).sin().abs())).clamp(0.0, 100.0)
            }));
            process.gpu_3d_percent = gpu;
            process.gpu_encode_percent =
                fold_engine_percents([(actor.weight * 1.5 + (step as f64 % 7.0)).clamp(0.0, 40.0)]);
            process.gpu_decode_percent = MetricValue::Value(0.0);
            let memory = if self.scenario == Scenario::History && actor.pid == 2008 {
                8 * 1024 * 1024 + step.saturating_mul(512 * 1024)
            } else {
                actor.memory
            };
            if memory == u64::MAX {
                process.memory_bytes = MetricValue::Unavailable;
                process.vram_dedicated_bytes = MetricValue::Unavailable;
                process.vram_shared_bytes = MetricValue::Unavailable;
            } else {
                process.memory_bytes = MetricValue::Value(memory);
                process.vram_dedicated_bytes = MetricValue::Value(memory / 8);
                process.vram_shared_bytes = MetricValue::Value(memory / 20);
            }
            let gpu3d = process.gpu_3d_percent;
            process.gpus = adapter_rows(
                self.scenario,
                actor.pid,
                gpu3d,
                process.gpu_encode_percent,
                process.gpu_decode_percent,
                process.vram_dedicated_bytes,
                process.vram_shared_bytes,
            );
            if let Some(max) = process
                .gpus
                .iter()
                .filter_map(|gpu| gpu.gpu_3d_percent.as_option())
                .reduce(f64::max)
            {
                process.gpu_3d_percent = MetricValue::Value(max);
            }
            sample.processes.push(process);
        }

        fill_devices(&mut sample, self.scenario, step);
        if let Some(count) = cpu_count(self.scenario) {
            sample.logical_processors = MetricValue::Value(u32::from(count));
            sample.cpu_detail = script_cpus(
                step,
                count,
                self.scenario == Scenario::CpusDenied,
                self.cpu_detail.load(Ordering::SeqCst),
            );
        }
        sample
    }
}

fn advance(slot: &mut u64, delta: u64, reading: Reading) -> MetricValue<u64> {
    match reading {
        Reading::Live => {
            *slot = slot.saturating_add(delta);
            MetricValue::Value(*slot)
        }
        Reading::Zero => MetricValue::Value(*slot),
        Reading::Unavailable => MetricValue::Unavailable,
        Reading::Unsupported => MetricValue::Unsupported,
    }
}

fn fill_devices(sample: &mut RawSample, scenario: Scenario, step: u64) {
    let physical_read = physical_disk_read(step);
    let physical_write = physical_read * 0.35;
    let (disks, adapters, gpus) = match scenario {
        Scenario::Special | Scenario::History => (
            vec![
                named("ディスク 0", physical_read * 0.7, physical_write * 0.7),
                named("ディスク 1", physical_read * 0.3, physical_write * 0.3),
            ],
            vec![
                named(
                    "イーサネット",
                    adapter_recv(step) * 0.8,
                    adapter_send(step) * 0.8,
                ),
                named("Wi-Fi", adapter_recv(step) * 0.2, adapter_send(step) * 0.2),
            ],
            vec![
                adapter_gpu(0x11, "GPU 0", 0.7, step),
                adapter_gpu(0x22, "GPU 1", 0.4, step),
            ],
        ),
        _ => (
            vec![named("ディスク 0", physical_read, physical_write)],
            vec![named(
                "イーサネット",
                adapter_recv(step),
                adapter_send(step),
            )],
            vec![adapter_gpu(0x11, "GPU 0", 1.0, step)],
        ),
    };
    sample.system_disk_read_bps = MetricValue::Value(physical_read);
    sample.system_disk_write_bps = MetricValue::Value(physical_write);
    sample.system_net_recv_bps = MetricValue::Value(adapter_recv(step));
    sample.system_net_send_bps = MetricValue::Value(adapter_send(step));
    sample.disks = disks;
    sample.adapters = adapters;
    sample.gpus = gpus;
    sample.system_gpu_3d_percent =
        fold_engine_percents(sample.gpus.iter().filter_map(|gpu| gpu.util_3d.as_option()));
    sample.system_gpu_encode_percent = fold_engine_percents(
        sample
            .gpus
            .iter()
            .filter_map(|gpu| gpu.util_encode.as_option()),
    );
    sample.system_gpu_decode_percent = MetricValue::Value(0.0);
    sample.system_vram_dedicated_bytes =
        sum_bytes(sample.gpus.iter().map(|gpu| gpu.dedicated_bytes));
    sample.system_vram_shared_bytes = sum_bytes(sample.gpus.iter().map(|gpu| gpu.shared_bytes));
}

fn named(name: &str, read: f64, write: f64) -> NamedRate {
    NamedRate {
        name: name.to_string(),
        read_bps: MetricValue::Value(read),
        write_bps: MetricValue::Value(write),
    }
}

fn adapter_gpu(luid: u64, name: &str, scale: f64, step: u64) -> GpuAdapter {
    let wave = 0.5 + 0.5 * ((step as f64) / 10.0).sin();
    GpuAdapter {
        luid,
        name: name.to_string(),
        util_3d: MetricValue::Value((28.0 + 50.0 * wave) * scale),
        util_encode: MetricValue::Value((6.0 + 10.0 * wave) * scale),
        util_decode: MetricValue::Value(0.0),
        dedicated_bytes: MetricValue::Value((512 * 1024 * 1024) as f64 as u64),
        shared_bytes: MetricValue::Value((128 * 1024 * 1024) as f64 as u64),
    }
}

fn sum_bytes(values: impl Iterator<Item = MetricValue<u64>>) -> MetricValue<u64> {
    let mut total = 0u64;
    let mut any = false;
    for value in values {
        if let MetricValue::Value(bytes) = value {
            total = total.saturating_add(bytes);
            any = true;
        }
    }
    if any {
        MetricValue::Value(total)
    } else {
        MetricValue::Unavailable
    }
}

fn system_cpu_percent(scenario: Scenario, step: u64) -> f64 {
    let t = step as f64;
    if scenario == Scenario::History {
        // A clear spike 30 seconds before the end of the 120-slot warmup.
        let peak = 78.0 * (-((t - 59.0) / 3.5).powi(2)).exp();
        return (10.0 + peak).clamp(2.0, 96.0);
    }
    let base = 18.0 + 7.0 * (t / 8.0).sin();
    let peak = 70.0 * (-((t - 90.0) / 6.0).powi(2)).exp();
    (base + peak).clamp(2.0, 96.0)
}

fn system_memory_used(step: u64, total: u64) -> u64 {
    let ratio = 0.42 + 0.06 * ((step as f64) / 12.0).sin() + (step as f64) * 0.0004;
    ((total as f64) * ratio.clamp(0.2, 0.85)) as u64
}

fn physical_disk_read(step: u64) -> f64 {
    let t = step as f64;
    let base = 4_000_000.0 + 1_500_000.0 * (t / 5.0).sin();
    let peak = 30_000_000.0 * (-((t - 40.0) / 4.0).powi(2)).exp();
    (base + peak).max(0.0)
}

fn adapter_recv(step: u64) -> f64 {
    let t = step as f64;
    1_200_000.0 + 6_000_000.0 * (-((t - 70.0) / 8.0).powi(2)).exp()
}

fn adapter_send(step: u64) -> f64 {
    adapter_recv(step) * 0.25
}

fn disk_bytes_per_step(step: u64, weight: f64) -> u64 {
    (weight * 80_000.0 * (1.0 + ((step as f64) / 6.0).sin().abs())) as u64
}

fn net_bytes_per_step(step: u64, weight: f64) -> u64 {
    (weight * 40_000.0 * (1.0 + ((step as f64) / 9.0).sin().abs())) as u64
}

fn adapter_rows(
    scenario: Scenario,
    pid: u32,
    gpu_3d: MetricValue<f64>,
    encode: MetricValue<f64>,
    decode: MetricValue<f64>,
    dedicated: MetricValue<u64>,
    shared: MetricValue<u64>,
) -> Vec<ProcessGpu> {
    let row = |luid: u64, gpu_3d: MetricValue<f64>| ProcessGpu {
        luid,
        gpu_3d_percent: gpu_3d,
        gpu_encode_percent: encode,
        gpu_decode_percent: decode,
        vram_dedicated_bytes: dedicated,
        vram_shared_bytes: shared,
    };
    match scenario {
        Scenario::Special | Scenario::History => {
            let high = gpu_3d;
            let low = match gpu_3d {
                MetricValue::Value(value) => MetricValue::Value((value * 0.1).clamp(0.0, 100.0)),
                other => other,
            };
            if pid.is_multiple_of(2) {
                vec![row(0x11, low), row(0x22, high)]
            } else {
                vec![row(0x11, high), row(0x22, low)]
            }
        }
        _ => vec![row(0x11, gpu_3d)],
    }
}

fn actors(scenario: Scenario) -> Vec<Actor> {
    match scenario {
        Scenario::Normal => crowd(100, false),
        Scenario::Many => crowd(1000, true),
        Scenario::Gaps => gaps(),
        Scenario::Special => special(),
        Scenario::History => history_cast(),
        Scenario::Cpus | Scenario::Cpus32 | Scenario::CpusDenied => crowd(24, false),
    }
}

fn cpu_count(scenario: Scenario) -> Option<u16> {
    match scenario {
        Scenario::Cpus | Scenario::CpusDenied => Some(8),
        Scenario::Cpus32 => Some(32),
        _ => None,
    }
}

fn script_cpus(step: u64, count: u16, denied: bool, enabled: bool) -> CpuDetailSnapshot {
    if !enabled {
        return CpuDetailSnapshot::off();
    }
    if denied {
        return CpuDetailSnapshot::needs_admin();
    }
    if step == 0 {
        return CpuDetailSnapshot::unavailable(
            "論理プロセッサの内訳は、最初の区間が確定するまで N/A です",
        );
    }
    let mut processors = Vec::with_capacity(usize::from(count));
    let mut busy = 0.0;
    for number in 0..count {
        let processes = script_rows(number, step);
        let usage = processes
            .iter()
            .map(|row| row.percent)
            .sum::<f64>()
            .min(100.0);
        busy += usage;
        processors.push(LogicalCpuSample {
            id: LogicalCpuId {
                group: 0,
                number: u8::try_from(number).unwrap_or(0),
            },
            usage_percent: MetricValue::Value(usage),
            processes,
        });
    }
    CpuDetailSnapshot {
        state: CpuDetailState::Ready,
        message: String::new(),
        lost_events: 0,
        events_in_interval: 800 + u64::from(count) * 20,
        aggregate_nanos: 35_000,
        mean_busy_percent: Some(busy / f64::from(count.max(1))),
        processors,
    }
}

fn script_rows(cpu: u16, step: u64) -> Vec<LogicalCpuProcess> {
    let mut rows = Vec::new();
    let push = |rows: &mut Vec<LogicalCpuProcess>, pid, create, name: &str, percent: f64| {
        rows.push(LogicalCpuProcess {
            key: ProcessKey {
                pid,
                creation_time_100ns: create,
            },
            name: name.to_string(),
            cpu_time_100ns: ((percent / 100.0) * 5_000_000.0).round() as u64,
            percent,
        });
    };
    match cpu {
        0 => {
            push(&mut rows, 3100, 2, "mover.exe", 22.0);
            push(&mut rows, 3000, 1, "resident.exe", 8.0);
        }
        1 if step < 40 => push(&mut rows, 4242, 100, "before.exe", 20.0),
        1 => push(&mut rows, 4242, 900, "after.exe", 12.0),
        // Wide enough that the 60-second bar nearest 30 seconds ago, and a
        // few live samples after warmup, still land inside this story.
        2 if (52..=68).contains(&step) => {
            push(&mut rows, 3301, 8, "kept-cpu2.exe", 60.0);
            push(&mut rows, 3300, 4, "gone-cpu.exe", 25.0);
        }
        2 if step < 52 => push(&mut rows, 3300, 4, "gone-cpu.exe", 30.0),
        2 => push(&mut rows, 3302, 9, "steady-cpu2.exe", 8.0),
        3 => {
            push(&mut rows, 3100, 2, "mover.exe", 14.0);
            push(&mut rows, 3003, 1, "side.exe", 6.0);
        }
        5 if (52..=68).contains(&step) => push(&mut rows, 3200, 3, "heavy-cpu5.exe", 72.0),
        5 => push(&mut rows, 3201, 3, "background.exe", 5.0),
        _ => {
            let percent =
                (3.0 + f64::from(cpu % 7) + 6.0 * ((step as f64) / 9.0 + f64::from(cpu)).sin())
                    .clamp(1.0, 90.0);
            push(
                &mut rows,
                4000 + u32::from(cpu),
                1,
                &format!("core{cpu}.exe"),
                percent,
            );
        }
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn history_actor(
    name: &str,
    pid: u32,
    creation: u64,
    weight: f64,
    memory: u64,
    cpu: Reading,
    born_at: u64,
    dies_at: Option<u64>,
) -> Actor {
    Actor {
        pid,
        creation,
        name: name.to_string(),
        weight,
        memory,
        cpu,
        disk: Reading::Live,
        net: Reading::Live,
        gpu_engines: &[40.0],
        born_at,
        dies_at,
    }
}

fn history_cast() -> Vec<Actor> {
    vec![
        history_actor(
            "spike.exe",
            2001,
            11,
            40.0,
            32 * 1024 * 1024,
            Reading::Live,
            0,
            None,
        ),
        history_actor(
            "steady.exe",
            2002,
            12,
            4.0,
            20 * 1024 * 1024,
            Reading::Live,
            0,
            None,
        ),
        history_actor(
            "gone.exe",
            2003,
            13,
            28.0,
            16 * 1024 * 1024,
            Reading::Live,
            0,
            Some(80),
        ),
        history_actor(
            "reused-before.exe",
            4242,
            100,
            6.0,
            12 * 1024 * 1024,
            Reading::Live,
            0,
            Some(40),
        ),
        history_actor(
            "reused-after.exe",
            4242,
            900,
            3.0,
            8 * 1024 * 1024,
            Reading::Live,
            50,
            None,
        ),
        history_actor(
            "unreadable.exe",
            2006,
            16,
            1.0,
            u64::MAX,
            Reading::Unavailable,
            0,
            None,
        ),
        history_actor(
            "idle.exe",
            2007,
            17,
            1.0,
            4 * 1024 * 1024,
            Reading::Zero,
            0,
            None,
        ),
        history_actor(
            "grow.exe",
            2008,
            18,
            2.0,
            8 * 1024 * 1024,
            Reading::Live,
            0,
            None,
        ),
    ]
}

fn crowd(count: u32, long_names: bool) -> Vec<Actor> {
    (0..count)
        .map(|index| {
            let pid = 1000 + index;
            let hot = index < 4;
            let name = if long_names && index == 0 {
                format!("very-long-process-name-{}-{}", "x".repeat(48), pid)
            } else if hot {
                format!("hot-{pid}.exe")
            } else {
                format!("app-{pid}.exe")
            };
            Actor {
                pid,
                creation: 10_000 + u64::from(pid),
                name,
                weight: if hot { 18.0 } else { 1.0 },
                memory: if index == 7 {
                    0
                } else {
                    20 * 1024 * 1024 + u64::from(index) * 1024 * 64
                },
                cpu: Reading::Live,
                disk: Reading::Live,
                net: Reading::Live,
                gpu_engines: if hot { &[70.0, 25.0] } else { &[4.0] },
                born_at: 0,
                dies_at: None,
            }
        })
        .collect()
}

fn gaps() -> Vec<Actor> {
    let mut actors = crowd(24, false);
    if let Some(actor) = actors.get_mut(1) {
        actor.cpu = Reading::Unavailable;
    }
    if let Some(actor) = actors.get_mut(2) {
        actor.disk = Reading::Unsupported;
        actor.net = Reading::Unsupported;
        actor.cpu = Reading::Zero;
    }
    if let Some(actor) = actors.get_mut(3) {
        actor.memory = u64::MAX;
        actor.net = Reading::Unavailable;
    }
    if let Some(actor) = actors.get_mut(4) {
        actor.disk = Reading::Zero;
        actor.gpu_engines = &[];
    }
    actors
}

fn special() -> Vec<Actor> {
    let mut actors = crowd(12, false);
    actors.push(Actor {
        pid: 4242,
        creation: 100,
        name: "reused-before.exe".to_string(),
        weight: 8.0,
        memory: 30 * 1024 * 1024,
        cpu: Reading::Live,
        disk: Reading::Live,
        net: Reading::Live,
        gpu_engines: &[55.0, 20.0],
        born_at: 0,
        dies_at: Some(40),
    });
    actors.push(Actor {
        pid: 4242,
        creation: 900,
        name: "reused-after.exe".to_string(),
        weight: 3.0,
        memory: 8 * 1024 * 1024,
        cpu: Reading::Live,
        disk: Reading::Live,
        net: Reading::Live,
        gpu_engines: &[10.0],
        born_at: 40,
        dies_at: None,
    });
    actors.push(Actor {
        pid: 7777,
        creation: 50,
        name: "comes-and-goes.exe".to_string(),
        weight: 6.0,
        memory: 12 * 1024 * 1024,
        cpu: Reading::Live,
        disk: Reading::Live,
        net: Reading::Live,
        gpu_engines: &[15.0],
        born_at: 20,
        dies_at: Some(70),
    });
    actors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::aggregate;
    use crate::model::ProcessKey;

    #[test]
    fn scenarios_are_deterministic_and_cover_the_requested_shapes() {
        let mut normal = MockProbe::new(Scenario::Normal);
        let first = normal.probe();
        assert_eq!(first.processes.len(), 100);
        let mut again = MockProbe::new(Scenario::Normal);
        assert_eq!(again.probe().processes.len(), 100);

        let mut many = MockProbe::new(Scenario::Many);
        let many_sample = many.probe();
        assert_eq!(many_sample.processes.len(), 1000);
        assert!(many_sample.processes[0].name.chars().count() > 40);
        let pids: std::collections::HashSet<_> =
            many_sample.processes.iter().map(|p| p.pid).collect();
        assert_eq!(pids.len(), 1000);

        let mut gaps = MockProbe::new(Scenario::Gaps);
        let gaps_sample = gaps.probe();
        assert!(gaps_sample
            .processes
            .iter()
            .any(|process| process.cpu_time_100ns == MetricValue::Unavailable));
        assert!(gaps_sample
            .processes
            .iter()
            .any(|process| process.disk_read_bytes == MetricValue::Unsupported));
        assert!(gaps_sample
            .processes
            .iter()
            .any(|process| process.memory_bytes == MetricValue::Value(0)));

        let mut special = MockProbe::new(Scenario::Special);
        let at_start = special.probe();
        assert_eq!(at_start.disks.len(), 2);
        assert_eq!(at_start.adapters.len(), 2);
        assert_eq!(at_start.gpus.len(), 2);
        assert!(at_start
            .processes
            .iter()
            .any(|process| process.name == "reused-before.exe"));
        assert!(at_start
            .processes
            .iter()
            .all(|process| process.name != "reused-after.exe"));
    }

    #[test]
    fn pid_reuse_does_not_share_counters_and_cpu_has_a_peak() {
        let mut probe = MockProbe::new(Scenario::Special);
        let mut previous = None;
        let mut saw_peak = false;
        let mut saw_both = false;
        for _ in 0..100 {
            let raw = probe.probe();
            let snapshot = aggregate(previous.as_ref(), &raw);
            if let MetricValue::Value(cpu) = snapshot.cpu_percent {
                if cpu > 70.0 {
                    saw_peak = true;
                }
            }
            let before = snapshot.processes.iter().any(|process| {
                process.key
                    == ProcessKey {
                        pid: 4242,
                        creation_time_100ns: 100,
                    }
            });
            let after = snapshot.processes.iter().find(|process| {
                process.key
                    == ProcessKey {
                        pid: 4242,
                        creation_time_100ns: 900,
                    }
            });
            assert!(!(before && after.is_some()));
            if let Some(row) = after {
                saw_both = true;
                assert_eq!(row.name, "reused-after.exe");
            }
            previous = Some(raw);
        }
        assert!(saw_peak);
        assert!(saw_both);
        let _ = previous;
    }

    #[test]
    fn logical_cpu_story_is_deterministic() {
        let mut probe = MockProbe::with_detail(
            Scenario::Cpus,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        );
        for _ in 0..59 {
            let _ = probe.probe();
        }
        let at_thirty = probe.probe();
        let cpu5 = at_thirty
            .cpu_detail
            .processors
            .iter()
            .find(|cpu| cpu.id.number == 5)
            .expect("cpu 5");
        assert_eq!(cpu5.processes[0].name, "heavy-cpu5.exe");
        assert!((cpu5.processes[0].percent - 72.0).abs() < 0.01);
        let cpu0 = &at_thirty.cpu_detail.processors[0];
        let cpu3 = &at_thirty.cpu_detail.processors[3];
        assert_eq!(cpu0.processes[0].name, "mover.exe");
        assert_eq!(cpu3.processes[0].name, "mover.exe");
        assert_eq!(cpu0.processes[0].key, cpu3.processes[0].key);
        assert!((cpu0.processes[0].percent - 22.0).abs() < 0.01);
        assert!((cpu3.processes[0].percent - 14.0).abs() < 0.01);
        let reused = at_thirty
            .cpu_detail
            .processors
            .iter()
            .find(|cpu| cpu.id.number == 1)
            .expect("cpu 1");
        assert_eq!(reused.processes[0].name, "after.exe");
        assert_eq!(reused.processes[0].key.pid, 4242);
        assert_eq!(reused.processes[0].key.creation_time_100ns, 900);

        let mut denied = MockProbe::with_detail(
            Scenario::CpusDenied,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        );
        let first = denied.probe();
        let denied_sample = denied.probe();
        assert_eq!(
            denied_sample.cpu_detail.state,
            crate::model::CpuDetailState::NeedsAdmin
        );
        assert!(denied_sample.cpu_detail.processors.is_empty());
        assert!(matches!(
            aggregate(Some(&first), &denied_sample).cpu_percent,
            MetricValue::Value(_)
        ));

        let mut off = MockProbe::with_detail(
            Scenario::Cpus,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(
            off.probe().cpu_detail.state,
            crate::model::CpuDetailState::Off
        );
    }
}
