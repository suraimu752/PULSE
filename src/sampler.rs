//! Sampler thread. It owns collectors, the previous counters, and the 60-second ring.
//! The UI only observes [`PublishedSample`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::aggregate::aggregate;
use crate::collector::{MetricEnricher, SystemProbe};
use crate::config::{history_capacity, CPU_SAMPLE_INTERVAL, SHUTDOWN_POLL};
use crate::history::align_timeline;
use crate::history::RingBuffer;
use crate::metric::MetricValue;
use crate::model::Snapshot;
use crate::names::NameTable;
use crate::platform;
use crate::selfmeter::{estimate_ring_bytes, SelfDiagnostics};
use crate::series::{chart_history, ChartHistory};

/// What the UI thread is allowed to read.
#[derive(Clone, Debug)]
pub struct PublishedSample {
    pub snapshot: Arc<Snapshot>,
    /// Oldest to newest, right-aligned to the 60-second window. `None` is a gap, not zero.
    pub cpu_history: Vec<Option<f64>>,
    pub memory_history: Vec<Option<f64>>,
    pub charts: ChartHistory,
    /// Right-aligned 60-second window. `None` is a slot with no sample.
    /// Each entry is the same `Arc` the ring holds, not a copy of the processes.
    pub timeline: Vec<Option<Arc<Snapshot>>>,
    pub self_cpu: MetricValue<f64>,
    pub self_memory: MetricValue<u64>,
    pub updated_text: String,
    pub demo: bool,
    /// This process and the sampler's own costs. Not a stand-in for other processes.
    pub diag: SelfDiagnostics,
}

#[derive(Clone)]
pub struct SharedLatest {
    inner: Arc<Mutex<Option<Arc<PublishedSample>>>>,
}

impl SharedLatest {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    fn publish(&self, sample: PublishedSample) {
        *self.lock() = Some(Arc::new(sample));
    }

    pub fn load(&self) -> Option<Arc<PublishedSample>> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<PublishedSample>>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Step-wise sampler used by the background thread and by tests.
pub struct SamplerCore {
    probe: Box<dyn SystemProbe>,
    enrichers: Vec<Box<dyn MetricEnricher>>,
    previous: Option<crate::model::RawSample>,
    history: RingBuffer<Arc<Snapshot>>,
    sequence: u64,
    demo: bool,
    names: NameTable,
}

impl SamplerCore {
    pub fn new(probe: Box<dyn SystemProbe>, enrichers: Vec<Box<dyn MetricEnricher>>) -> Self {
        Self {
            probe,
            enrichers,
            previous: None,
            history: RingBuffer::with_capacity(history_capacity()),
            sequence: 0,
            demo: false,
            names: NameTable::new(),
        }
    }

    fn aligned_at(&self, index: usize) -> Option<Arc<Snapshot>> {
        let aligned = align_timeline(self.history.iter().cloned(), self.history.capacity());
        aligned.get(index).and_then(|slot| slot.clone())
    }

    pub fn tick(&mut self) -> PublishedSample {
        let step_started = Instant::now();
        let probe_started = Instant::now();
        let mut raw = self.probe.probe();
        let probe_nanos = nanos(probe_started.elapsed());
        let threads = self.probe.self_threads();
        let mut enrich_nanos = 0u64;
        for enricher in &mut self.enrichers {
            let id = enricher.id();
            let enrich_started = Instant::now();
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                enricher.enrich(&mut raw);
            }));
            enrich_nanos = enrich_nanos.saturating_add(nanos(enrich_started.elapsed()));
            if caught.is_err() {
                let note = format!("{id} failed");
                raw.warning = Some(match raw.warning.take() {
                    Some(existing) => format!("{existing}; {note}"),
                    None => note,
                });
            }
        }
        self.names.begin();
        let mut snapshot = aggregate(self.previous.as_ref(), &raw, &mut self.names);
        self.names.end();
        self.sequence = self.sequence.saturating_add(1);
        snapshot.sequence = self.sequence;
        let updated_text = platform::format_local_hms(snapshot.taken_at);
        let snapshot = Arc::new(snapshot);
        self.history.push(Arc::clone(&snapshot));
        let mut diag = platform::read_self_usage();
        diag.threads = threads;
        diag.sample_nanos = nanos(step_started.elapsed());
        diag.probe_nanos = probe_nanos;
        diag.enrich_nanos = enrich_nanos;
        diag.pdh_reopens = platform::pdh_reopen_count();
        diag.ring_slots = u32::try_from(self.history.len()).unwrap_or(u32::MAX);
        diag.ring_bytes = estimate_ring_bytes(&snapshot, self.history.len());
        diag.etw_buffer_ceiling = platform::etw_buffer_ceiling(&snapshot);
        let published = publish(
            &self.history,
            Arc::clone(&snapshot),
            updated_text,
            self.demo,
            diag,
        );
        self.previous = Some(raw);
        published
    }
}

fn nanos(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
}

pub struct SamplerHandle {
    shared: SharedLatest,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    /// One snapshot kept for a screenshot of selection outside the ring.
    retained: Option<Arc<Snapshot>>,
    /// Detailed per-CPU analysis. Off leaves GetSystemTimes running.
    cpu_detail: Arc<AtomicBool>,
}

/// Pin this aligned slot, then publish `extra_steps` newer samples.
/// The mock clock moves with those steps. Wall time is not waited on.
#[derive(Clone, Copy, Debug)]
pub struct RetainRequest {
    pub index: usize,
    pub extra_steps: u32,
}

impl SamplerHandle {
    pub fn start(demo: Option<&str>, retain: Option<RetainRequest>) -> Result<Self, String> {
        let shared = SharedLatest::new();
        let stop = Arc::new(AtomicBool::new(false));
        let (mut core, warmup, cpu_detail) = build_core(demo)?;
        for _ in 0..warmup {
            shared.publish(core.tick());
        }
        let retained = if let Some(request) = retain {
            let pinned = core.aligned_at(request.index).ok_or_else(|| {
                "指定した履歴の位置に計測がありません。60 秒分のデモを温めてから指定してください。"
                    .to_string()
            })?;
            for _ in 0..request.extra_steps {
                shared.publish(core.tick());
            }
            Some(pinned)
        } else {
            None
        };
        let shared_thread = shared.clone();
        let stop_thread = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("pulse-sampler".to_string())
            .spawn(move || sampler_loop(core, shared_thread, stop_thread))
            .map_err(|err| format!("計測スレッドを開始できませんでした: {err}"))?;
        Ok(Self {
            shared,
            stop,
            join: Some(join),
            retained,
            cpu_detail,
        })
    }

    /// Turn detailed per-CPU analysis on or off. Overall CPU is unchanged.
    pub fn set_cpu_detail(&self, on: bool) {
        self.cpu_detail.store(on, Ordering::SeqCst);
    }

    pub fn cpu_detail_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cpu_detail)
    }

    pub fn latest(&self) -> SharedLatest {
        self.shared.clone()
    }

    pub fn take_retained(&mut self) -> Option<Arc<Snapshot>> {
        self.retained.take()
    }
}

impl Drop for SamplerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn build_core(demo: Option<&str>) -> Result<(SamplerCore, u32, Arc<AtomicBool>), String> {
    let cpu_detail = Arc::new(AtomicBool::new(false));
    #[cfg(feature = "mock")]
    if let Some(name) = demo {
        let scenario = crate::mock::Scenario::parse(name)
            .ok_or_else(|| format!("未知のデモシナリオです: {name}"))?;
        let on = matches!(
            scenario,
            crate::mock::Scenario::Cpus
                | crate::mock::Scenario::Cpus32
                | crate::mock::Scenario::CpusDenied
        );
        cpu_detail.store(on, Ordering::SeqCst);
        let mut core = SamplerCore::new(
            Box::new(crate::mock::MockProbe::with_detail(
                scenario,
                Arc::clone(&cpu_detail),
            )),
            Vec::new(),
        );
        core.demo = true;
        let warmup = u32::try_from(history_capacity()).unwrap_or(120);
        return Ok((core, warmup, cpu_detail));
    }
    #[cfg(not(feature = "mock"))]
    if demo.is_some() {
        return Err(
            "このビルドに DEMO DATA は入っていません。--features mock で起動してください。"
                .to_string(),
        );
    }
    Ok((
        SamplerCore::new(
            platform::make_probe(),
            platform::make_enrichers(Arc::clone(&cpu_detail)),
        ),
        0,
        cpu_detail,
    ))
}

fn sampler_loop(mut core: SamplerCore, shared: SharedLatest, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        let published = core.tick();
        shared.publish(published);
        sleep_until_next(started, &stop);
    }
}

fn sleep_until_next(started: Instant, stop: &AtomicBool) {
    let mut remaining = CPU_SAMPLE_INTERVAL.saturating_sub(started.elapsed());
    while remaining > std::time::Duration::ZERO {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let slice = remaining.min(SHUTDOWN_POLL);
        thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
}

fn publish(
    history: &RingBuffer<Arc<Snapshot>>,
    snapshot: Arc<Snapshot>,
    updated_text: String,
    demo: bool,
    diag: SelfDiagnostics,
) -> PublishedSample {
    let charts = chart_history(
        history.iter().map(|sample| sample.as_ref()),
        history.capacity(),
    );
    let (self_cpu, self_memory) = self_metrics(&snapshot);
    let timeline = align_timeline(history.iter().cloned(), history.capacity());
    PublishedSample {
        cpu_history: charts.cpu.clone(),
        memory_history: charts.memory.clone(),
        charts,
        timeline,
        self_cpu,
        self_memory,
        updated_text,
        demo,
        diag,
        snapshot,
    }
}

fn self_metrics(snapshot: &Snapshot) -> (MetricValue<f64>, MetricValue<u64>) {
    let pid = std::process::id();
    snapshot
        .processes
        .iter()
        .find(|process| process.key.pid == pid)
        .map(|process| (process.cpu_percent, process.memory_bytes))
        .unwrap_or((MetricValue::Unavailable, MetricValue::Unavailable))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::{MetricEnricher, SystemProbe};
    use crate::metric::MetricValue;
    use crate::model::{RawProcess, RawSample};
    use std::time::{Duration, SystemTime};

    struct ScriptedProbe {
        ticks: u32,
        start: Instant,
    }

    impl SystemProbe for ScriptedProbe {
        fn probe(&mut self) -> RawSample {
            self.ticks += 1;
            let taken_at = self.start + Duration::from_millis(500 * u64::from(self.ticks - 1));
            let mut process = RawProcess::new(10, 1, "app.exe");
            process.creation_time_100ns = Some(42);
            process.cpu_time_100ns = MetricValue::Value(u64::from(self.ticks) * 5_000_000);
            process.memory_bytes = MetricValue::Value(10_000_000);
            let mut sample = RawSample::empty(taken_at, SystemTime::UNIX_EPOCH);
            sample.logical_processors = MetricValue::Value(4);
            sample.system_total_100ns = MetricValue::Value(u64::from(self.ticks) * 40_000_000);
            sample.system_busy_100ns = MetricValue::Value(u64::from(self.ticks) * 10_000_000);
            sample.memory_total_bytes = MetricValue::Value(1_000_000_000);
            sample.memory_available_bytes = MetricValue::Value(400_000_000);
            sample.processes.push(process);
            sample
        }
    }

    struct NetEnricher {
        calls: u64,
    }

    impl MetricEnricher for NetEnricher {
        fn id(&self) -> &'static str {
            "net-test"
        }

        fn enrich(&mut self, sample: &mut RawSample) {
            self.calls += 1;
            let value = self.calls * 1_000;
            for process in &mut sample.processes {
                process.net_send_bytes = MetricValue::Value(value);
            }
        }
    }

    #[test]
    fn enricher_rates_do_not_turn_unsupported_disk_into_zero() {
        let mut core = SamplerCore::new(
            Box::new(ScriptedProbe {
                ticks: 0,
                start: Instant::now(),
            }),
            vec![Box::new(NetEnricher { calls: 0 })],
        );
        let first = core.tick();
        assert_eq!(first.snapshot.sequence, 1);
        assert_eq!(first.snapshot.cpu_percent, MetricValue::Unavailable);
        assert_eq!(
            first.snapshot.processes[0].disk_read_bps,
            MetricValue::Unsupported
        );
        assert_eq!(
            first.snapshot.processes[0].net_send_bps,
            MetricValue::Unavailable
        );
        assert_eq!(first.cpu_history.len(), history_capacity());
        assert!(first.cpu_history.iter().all(Option::is_none));

        let second = core.tick();
        assert_eq!(second.snapshot.cpu_percent, MetricValue::Value(25.0));
        assert_eq!(
            second.snapshot.processes[0].cpu_percent,
            MetricValue::Value(12.5)
        );
        assert_eq!(
            second.snapshot.processes[0].net_send_bps,
            MetricValue::Value(2_000.0)
        );
        assert_eq!(
            second.snapshot.processes[0].disk_read_bps,
            MetricValue::Unsupported
        );
        assert_eq!(second.memory_history.last().copied(), Some(Some(60.0)));
        assert_eq!(second.cpu_history.last().copied(), Some(Some(25.0)));
        assert!(second.cpu_history.first().copied().unwrap().is_none());
        // History keeps the full process row from the first tick.
        assert_eq!(core.history.len(), 2);
        assert_eq!(
            core.history.iter().next().unwrap().processes[0].cpu_percent,
            MetricValue::Unavailable
        );
    }

    #[test]
    fn extra_steps_evict_the_pinned_slot_without_waiting() {
        let mut core = SamplerCore::new(
            Box::new(ScriptedProbe {
                ticks: 0,
                start: Instant::now(),
            }),
            Vec::new(),
        );
        for _ in 0..history_capacity() {
            core.tick();
        }
        let pinned = core.aligned_at(59).expect("slot");
        let pinned_id = pinned.sequence;
        for _ in 0..80 {
            core.tick();
        }
        let still = align_timeline(core.history.iter().cloned(), core.history.capacity());
        assert!(still
            .iter()
            .flatten()
            .all(|snapshot| snapshot.sequence != pinned_id));
        let newest = core.history.iter().last().expect("newest");
        let age = newest.monotonic.saturating_duration_since(pinned.monotonic);
        assert_eq!(age, Duration::from_secs(70));
    }

    /// Disk, network, and GPU failing must not drop the other processes' CPU.
    #[test]
    fn device_failures_leave_non_self_cpu_ranking_intact() {
        struct ManyProbe {
            ticks: u32,
            start: Instant,
        }
        impl SystemProbe for ManyProbe {
            fn probe(&mut self) -> RawSample {
                self.ticks += 1;
                let taken_at = self.start + Duration::from_millis(500 * u64::from(self.ticks - 1));
                let mut sample = RawSample::empty(taken_at, SystemTime::UNIX_EPOCH);
                sample.logical_processors = MetricValue::Value(8);
                sample.system_total_100ns = MetricValue::Value(u64::from(self.ticks) * 80_000_000);
                sample.system_busy_100ns = MetricValue::Value(u64::from(self.ticks) * 40_000_000);
                sample.memory_total_bytes = MetricValue::Value(32 << 30);
                sample.memory_available_bytes = MetricValue::Value(8 << 30);
                for (pid, share) in [(100u32, 20u64), (200, 10), (300, 1)] {
                    let mut process = RawProcess::new(pid, 1, format!("app-{pid}.exe"));
                    process.creation_time_100ns = Some(u64::from(pid));
                    process.cpu_time_100ns =
                        MetricValue::Value(u64::from(self.ticks) * share * 1_000_000);
                    process.memory_bytes = MetricValue::Value(1_000_000);
                    sample.processes.push(process);
                }
                sample
            }
        }
        struct Boom;
        impl MetricEnricher for Boom {
            fn id(&self) -> &'static str {
                "boom"
            }
            fn enrich(&mut self, _sample: &mut RawSample) {
                panic!("disk collector exploded");
            }
        }
        struct FailingDevices;
        impl MetricEnricher for FailingDevices {
            fn id(&self) -> &'static str {
                "devices"
            }
            fn enrich(&mut self, sample: &mut RawSample) {
                sample.system_disk_read_bps = MetricValue::Unavailable;
                sample.system_disk_write_bps = MetricValue::Unavailable;
                sample.system_net_recv_bps = MetricValue::Unavailable;
                sample.system_net_send_bps = MetricValue::Unavailable;
                sample.system_gpu_3d_percent = MetricValue::Unavailable;
                sample.system_gpu_encode_percent = MetricValue::Unavailable;
                sample.system_gpu_decode_percent = MetricValue::Unavailable;
                sample.system_vram_dedicated_bytes = MetricValue::Unavailable;
                sample.system_vram_shared_bytes = MetricValue::Unavailable;
                for process in &mut sample.processes {
                    process.disk_read_bytes = MetricValue::Unavailable;
                    process.disk_write_bytes = MetricValue::Unavailable;
                    process.gpu_3d_percent = MetricValue::Unavailable;
                    process.vram_dedicated_bytes = MetricValue::Unavailable;
                    process.vram_shared_bytes = MetricValue::Unavailable;
                }
            }
        }

        let mut core = SamplerCore::new(
            Box::new(ManyProbe {
                ticks: 0,
                start: Instant::now(),
            }),
            vec![Box::new(Boom), Box::new(FailingDevices)],
        );
        core.tick();
        let mut published = core.tick();
        for _ in 0..40 {
            published = core.tick();
        }
        assert_eq!(published.snapshot.processes.len(), 3);
        assert_eq!(published.snapshot.cpu_percent, MetricValue::Value(50.0));
        assert_eq!(published.snapshot.disk_read_bps, MetricValue::Unavailable);
        assert!(published
            .snapshot
            .warning
            .as_deref()
            .is_some_and(|text| text.contains("boom failed")));
        let crate::rank::RankStatus::Ranked(rows) =
            crate::rank::rank(&published.snapshot, crate::model::ResourceId::Cpu, None, 5)
        else {
            panic!("cpu rank");
        };
        assert_eq!(
            rows.iter().map(|row| row.pid).collect::<Vec<_>>(),
            vec![100, 200, 300]
        );
        assert!(rows.iter().all(|row| row.value != "N/A"));
    }

    #[test]
    fn a_long_run_shares_names_and_does_not_grow_the_ring() {
        let mut core = SamplerCore::new(
            Box::new(ScriptedProbe {
                ticks: 0,
                start: Instant::now(),
            }),
            Vec::new(),
        );
        let mut shared = None;
        let mut published = core.tick();
        for _ in 0..150 {
            published = core.tick();
            let name = published.snapshot.processes[0].name.as_arc().clone();
            if let Some(previous) = &shared {
                assert!(std::sync::Arc::ptr_eq(previous, &name));
            } else {
                shared = Some(name);
            }
        }
        assert_eq!(core.history.len(), 120);
        assert_eq!(core.names.len(), 1);
        assert_eq!(published.diag.ring_slots, 120);
        assert!(published.diag.ring_bytes > 0);
        assert_eq!(published.diag.handles, MetricValue::Unavailable);
        assert_eq!(published.diag.private_bytes, MetricValue::Unavailable);
        assert_eq!(published.diag.etw_buffer_ceiling, MetricValue::Unavailable);
        assert_ne!(published.diag.handles, MetricValue::Value(0));
    }
}
