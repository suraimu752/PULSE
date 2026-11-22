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
use crate::platform;
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
        }
    }

    pub fn tick(&mut self) -> PublishedSample {
        let mut raw = self.probe.probe();
        for enricher in &mut self.enrichers {
            let id = enricher.id();
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                enricher.enrich(&mut raw);
            }));
            if caught.is_err() {
                let note = format!("{id} failed");
                raw.warning = Some(match raw.warning.take() {
                    Some(existing) => format!("{existing}; {note}"),
                    None => note,
                });
            }
        }
        let mut snapshot = aggregate(self.previous.as_ref(), &raw);
        self.sequence = self.sequence.saturating_add(1);
        snapshot.sequence = self.sequence;
        let updated_text = platform::format_local_hms(snapshot.taken_at);
        let snapshot = Arc::new(snapshot);
        self.history.push(Arc::clone(&snapshot));
        let published = publish(
            &self.history,
            Arc::clone(&snapshot),
            updated_text,
            self.demo,
        );
        self.previous = Some(raw);
        published
    }
}

pub struct SamplerHandle {
    shared: SharedLatest,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SamplerHandle {
    pub fn start(demo: Option<&str>) -> Result<Self, String> {
        let shared = SharedLatest::new();
        let stop = Arc::new(AtomicBool::new(false));
        let (mut core, warmup) = build_core(demo)?;
        for _ in 0..warmup {
            shared.publish(core.tick());
        }
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
        })
    }

    pub fn latest(&self) -> SharedLatest {
        self.shared.clone()
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

fn build_core(demo: Option<&str>) -> Result<(SamplerCore, u32), String> {
    #[cfg(feature = "mock")]
    if let Some(name) = demo {
        let scenario = crate::mock::Scenario::parse(name)
            .ok_or_else(|| format!("未知のデモシナリオです: {name}"))?;
        let mut core =
            SamplerCore::new(Box::new(crate::mock::MockProbe::new(scenario)), Vec::new());
        core.demo = true;
        let warmup = u32::try_from(history_capacity()).unwrap_or(120);
        return Ok((core, warmup));
    }
    #[cfg(not(feature = "mock"))]
    if demo.is_some() {
        return Err(
            "このビルドに DEMO DATA は入っていません。--features mock で起動してください。"
                .to_string(),
        );
    }
    Ok((
        SamplerCore::new(platform::make_probe(), platform::make_enrichers()),
        0,
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
}
