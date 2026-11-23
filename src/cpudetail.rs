#![cfg_attr(not(windows), allow(dead_code))]
//! Per-logical-processor CPU time from context-switch events.
//!
//! The Windows collector feeds this ledger. Tests feed it directly. It does
//! not read GetSystemTimes and must not be used to replace that overall CPU
//! percent. A percent here is time on that one logical processor divided by
//! the interval length.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use crate::model::{
    CpuDetailSnapshot, CpuDetailState, LogicalCpuId, LogicalCpuProcess, LogicalCpuSample,
    ProcessKey,
};

/// One ETW fact, already decoded. Times are on one monotonic clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CpuEvent {
    /// Process or thread identity. `create_time_100ns == 0` is a weaker key.
    ProcessStart {
        pid: u32,
        create_time_100ns: u64,
        name: String,
    },
    ThreadStart {
        tid: u32,
        pid: u32,
        create_time_100ns: u64,
    },
    ThreadEnd {
        tid: u32,
    },
    /// `old_tid` left `cpu` and `new_tid` entered it at `time`.
    Switch {
        time: u64,
        cpu: LogicalCpuId,
        old_tid: u32,
        new_tid: u32,
    },
    /// DPC or ISR ending at `time`. Not charged to the thread that was on CPU.
    Unattributed {
        time: u64,
        cpu: LogicalCpuId,
        duration: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Idle,
    Unknown,
    Process { pid: u32, create_time_100ns: u64 },
}

#[derive(Clone, Debug)]
struct ThreadRec {
    pid: u32,
    create_time_100ns: u64,
}

#[derive(Clone, Debug)]
struct Running {
    since: u64,
    kind: Kind,
    /// DPC/ISR already removed from the open slice.
    withheld: u64,
}

#[derive(Clone, Debug, Default)]
struct CpuBuckets {
    idle: u64,
    unknown: u64,
    unattributed: u64,
    processes: HashMap<(u32, u64), u64>,
}

/// Folds switch events into interval snapshots.
#[derive(Debug)]
pub struct SwitchLedger {
    enabled: bool,
    threads: HashMap<u32, ThreadRec>,
    /// TIDs that have ended. A later switch without a new start stays unknown.
    ended: HashSet<u32>,
    names: HashMap<(u32, u64), String>,
    running: HashMap<LogicalCpuId, Running>,
    buckets: HashMap<LogicalCpuId, CpuBuckets>,
    expected: Vec<LogicalCpuId>,
    interval_start: Option<u64>,
    lost: bool,
    lost_total: u64,
    events: u64,
    warmup_done: bool,
    seen_switch: bool,
}

impl Default for SwitchLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchLedger {
    pub fn new() -> Self {
        Self {
            enabled: false,
            threads: HashMap::new(),
            ended: HashSet::new(),
            names: HashMap::new(),
            running: HashMap::new(),
            buckets: HashMap::new(),
            expected: Vec::new(),
            interval_start: None,
            lost: false,
            lost_total: 0,
            events: 0,
            warmup_done: false,
            seen_switch: false,
        }
    }

    pub fn set_enabled(&mut self, on: bool) {
        if on == self.enabled {
            return;
        }
        self.enabled = on;
        self.running.clear();
        self.buckets.clear();
        self.interval_start = None;
        self.lost = false;
        self.events = 0;
        self.warmup_done = false;
        self.seen_switch = false;
        if !on {
            self.threads.clear();
            self.ended.clear();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// True after the first switch or DPC on the event clock.
    /// A close before that must not stamp a different clock into the ledger.
    pub fn has_timestamp(&self) -> bool {
        self.interval_start.is_some()
    }

    /// Logical processors the OS says exist. Ones not yet switched stay unavailable.
    pub fn expect(&mut self, cpus: impl IntoIterator<Item = LogicalCpuId>) {
        self.expected = cpus.into_iter().collect();
        self.expected.sort();
        self.expected.dedup();
    }

    /// Name and creation time from the normal process snapshot. Fills only
    /// threads whose creation time is still unknown, so a reused PID does not
    /// overwrite a thread that already recorded the old creation time.
    pub fn note_live_process(&mut self, pid: u32, create_time_100ns: u64, name: &str) {
        if !self.enabled {
            return;
        }
        self.note_process(pid, create_time_100ns, name);
        for thread in self.threads.values_mut() {
            if thread.pid == pid && thread.create_time_100ns == 0 {
                thread.create_time_100ns = create_time_100ns;
            }
        }
    }

    /// Identity observed outside ETW (Toolhelp at session start). Not a guess
    /// for a TID that has already ended.
    pub fn seed_thread(&mut self, tid: u32, pid: u32, create_time_100ns: u64, name: &str) {
        if self.ended.contains(&tid) {
            return;
        }
        self.note_process(pid, create_time_100ns, name);
        self.threads.insert(
            tid,
            ThreadRec {
                pid,
                create_time_100ns,
            },
        );
    }

    pub fn mark_lost(&mut self, count: u64) {
        if count == 0 {
            return;
        }
        self.lost = true;
        self.lost_total = self.lost_total.saturating_add(count);
    }

    pub fn push(&mut self, event: CpuEvent) {
        if !self.enabled {
            return;
        }
        self.events = self.events.saturating_add(1);
        match event {
            CpuEvent::ProcessStart {
                pid,
                create_time_100ns,
                name,
            } => self.note_process(pid, create_time_100ns, &name),
            CpuEvent::ThreadStart {
                tid,
                pid,
                create_time_100ns,
            } => {
                self.ended.remove(&tid);
                self.threads.insert(
                    tid,
                    ThreadRec {
                        pid,
                        create_time_100ns,
                    },
                );
            }
            CpuEvent::ThreadEnd { tid } => {
                self.threads.remove(&tid);
                self.ended.insert(tid);
            }
            CpuEvent::Switch {
                time,
                cpu,
                old_tid: _,
                new_tid,
            } => {
                self.seen_switch = true;
                self.ensure_start(time);
                self.credit_until(cpu, time);
                let kind = self.kind_of(new_tid);
                self.running.insert(
                    cpu,
                    Running {
                        since: time,
                        kind,
                        withheld: 0,
                    },
                );
            }
            CpuEvent::Unattributed {
                time,
                cpu,
                duration,
            } => {
                self.ensure_start(time);
                let bucket = self.buckets.entry(cpu).or_default();
                bucket.unattributed = bucket.unattributed.saturating_add(duration);
                if let Some(running) = self.running.get_mut(&cpu) {
                    if time > running.since {
                        let open = time.saturating_sub(running.since);
                        let take = duration.min(open.saturating_sub(running.withheld));
                        running.withheld = running.withheld.saturating_add(take);
                    }
                }
            }
        }
    }

    /// Close the interval at `end` and start the next one there.
    ///
    /// A thread still running is split: time up to `end` stays in this
    /// interval, and the remainder begins at `end`.
    pub fn close(&mut self, end: u64) -> CpuDetailSnapshot {
        if !self.enabled {
            return CpuDetailSnapshot::off();
        }
        let started = Instant::now();
        let start = self.interval_start.unwrap_or(end);
        let span = end.saturating_sub(start).max(1);
        let cpus: Vec<LogicalCpuId> = self.running.keys().copied().collect();
        for cpu in cpus {
            self.credit_until(cpu, end);
            if let Some(running) = self.running.get_mut(&cpu) {
                running.since = end;
                running.withheld = 0;
            }
        }
        let lost = self.lost;
        let events = self.events;
        let lost_total = self.lost_total;
        self.events = 0;
        self.lost = false;
        self.interval_start = Some(end);

        let warmup = !self.warmup_done || !self.seen_switch;
        self.warmup_done = true;

        let mut ids = self.expected.clone();
        for cpu in self.buckets.keys() {
            if !ids.contains(cpu) {
                ids.push(*cpu);
            }
        }
        ids.sort();
        ids.dedup();

        let mut processors = Vec::with_capacity(ids.len());
        let mut busy_sum = 0.0;
        let mut busy_count = 0;
        for id in ids {
            let bucket = self.buckets.remove(&id).unwrap_or_default();
            let established = self.running.contains_key(&id);
            if lost || warmup || !established {
                processors.push(LogicalCpuSample {
                    id,
                    usage_percent: crate::metric::MetricValue::Unavailable,
                    processes: Vec::new(),
                });
                continue;
            }
            let idle = bucket.idle.min(span);
            let busy = span.saturating_sub(idle);
            let usage = percent(busy, span);
            busy_sum += usage;
            busy_count += 1;
            let mut processes = Vec::new();
            for ((pid, create), time) in bucket.processes {
                if time == 0 || pid == 0 {
                    continue;
                }
                let key = ProcessKey {
                    pid,
                    creation_time_100ns: create,
                };
                let name = self
                    .names
                    .get(&(pid, create))
                    .cloned()
                    .unwrap_or_else(|| format!("pid {pid}"));
                processes.push(LogicalCpuProcess {
                    key,
                    name,
                    cpu_time_100ns: time,
                    percent: percent(time, span),
                });
            }
            // Unknown and unattributed stay out of `processes`.
            let _ = (bucket.unknown, bucket.unattributed);
            processors.push(LogicalCpuSample {
                id,
                usage_percent: crate::metric::MetricValue::Value(usage),
                processes,
            });
        }

        let nanos = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if lost || warmup || busy_count == 0 {
            let message = if lost {
                "ETW のイベントが欠測したため、この区間の論理プロセッサ内訳は N/A です"
            } else {
                "論理プロセッサの内訳は、最初の区間が確定するまで N/A です"
            };
            return CpuDetailSnapshot {
                state: CpuDetailState::Unavailable,
                message: message.to_string(),
                lost_events: lost_total,
                events_in_interval: events,
                aggregate_nanos: nanos,
                mean_busy_percent: None,
                processors,
            };
        }
        CpuDetailSnapshot {
            state: CpuDetailState::Ready,
            message: String::new(),
            lost_events: lost_total,
            events_in_interval: events,
            aggregate_nanos: nanos,
            mean_busy_percent: Some(busy_sum / busy_count as f64),
            processors,
        }
    }

    fn note_process(&mut self, pid: u32, create_time_100ns: u64, name: &str) {
        if name.is_empty() {
            return;
        }
        self.names
            .insert((pid, create_time_100ns), name.to_string());
    }

    fn ensure_start(&mut self, time: u64) {
        if self.interval_start.is_none() {
            self.interval_start = Some(time);
        }
    }

    fn kind_of(&self, tid: u32) -> Kind {
        if tid == 0 {
            return Kind::Idle;
        }
        if self.ended.contains(&tid) && !self.threads.contains_key(&tid) {
            return Kind::Unknown;
        }
        match self.threads.get(&tid) {
            Some(thread) if thread.pid == 0 => Kind::Idle,
            Some(thread) => Kind::Process {
                pid: thread.pid,
                create_time_100ns: thread.create_time_100ns,
            },
            None => Kind::Unknown,
        }
    }

    fn credit_until(&mut self, cpu: LogicalCpuId, time: u64) {
        let Some(running) = self.running.get(&cpu) else {
            return;
        };
        if time <= running.since {
            return;
        }
        let raw = time.saturating_sub(running.since);
        let credited = raw.saturating_sub(running.withheld);
        let kind = running.kind;
        let bucket = self.buckets.entry(cpu).or_default();
        match kind {
            Kind::Idle => bucket.idle = bucket.idle.saturating_add(credited),
            Kind::Unknown => bucket.unknown = bucket.unknown.saturating_add(credited),
            Kind::Process {
                pid,
                create_time_100ns,
            } => {
                let slot = bucket
                    .processes
                    .entry((pid, create_time_100ns))
                    .or_insert(0);
                *slot = slot.saturating_add(credited);
            }
        }
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let capped = part.min(whole);
    (capped as f64) * 100.0 / (whole as f64)
}

/// Which clock a live ETW session is using.
///
/// ProcessTrace converts QPC stamps into 100 ns since 1601 unless the consumer
/// sets `PROCESS_TRACE_MODE_RAW_TIMESTAMP`. Closing that converted stamp with
/// `QueryPerformanceCounter` mixes epochs, and a logical CPU lands on 0% or 100%.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockMode {
    /// Same counter as `QueryPerformanceCounter`.
    Qpc,
    /// 100 ns since 1601, same epoch as `GetSystemTimePreciseAsFileTime`.
    FileTime,
}

/// Event stamps and the interval close, on one scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionClock {
    mode: ClockMode,
    /// Frequency of raw event stamps. `TRACE_LOGFILE_HEADER.PerfFreq`.
    event_hz: u64,
    /// `QueryPerformanceFrequency`. The QPC close is already in these ticks.
    qpc_hz: u64,
}

impl SessionClock {
    /// QPC session. `perf_freq == 0` means the header did not publish a frequency,
    /// so event stamps are treated as already being `qpc_hz` ticks.
    pub fn qpc(perf_freq: u64, qpc_hz: u64) -> Self {
        let qpc_hz = qpc_hz.max(1);
        Self {
            mode: ClockMode::Qpc,
            event_hz: if perf_freq == 0 { qpc_hz } else { perf_freq },
            qpc_hz,
        }
    }

    pub fn filetime() -> Self {
        Self {
            mode: ClockMode::FileTime,
            event_hz: 10_000_000,
            qpc_hz: 10_000_000,
        }
    }

    /// Raw `EVENT_HEADER.TimeStamp` onto the close clock.
    pub fn event_ticks(self, raw: u64) -> u64 {
        match self.mode {
            ClockMode::FileTime => raw,
            ClockMode::Qpc => scale_freq(raw, self.event_hz, self.qpc_hz),
        }
    }

    /// Interval end on the same clock as [`Self::event_ticks`].
    pub fn close_ticks(self, qpc: u64, filetime: u64) -> u64 {
        match self.mode {
            ClockMode::FileTime => filetime,
            ClockMode::Qpc => qpc,
        }
    }
}

/// Decide which live clock a raw stamp belongs to.
///
/// `qpc_hz` is `QueryPerformanceFrequency`. Five seconds of slack covers a
/// real-time buffer that flushes once a second.
pub fn classify_timestamp(raw: u64, qpc: u64, filetime: u64, qpc_hz: u64) -> ClockMode {
    let qpc_slop = qpc_hz.max(1).saturating_mul(5);
    let file_slop = 10_000_000u64.saturating_mul(5);
    let near_qpc = raw.abs_diff(qpc) <= qpc_slop;
    let near_file = raw.abs_diff(filetime) <= file_slop;
    if near_qpc {
        ClockMode::Qpc
    } else if near_file || raw.abs_diff(filetime) < raw.abs_diff(qpc) {
        ClockMode::FileTime
    } else {
        ClockMode::Qpc
    }
}

/// `ticks` counted at `from_hz`, expressed as ticks at `to_hz`.
pub fn scale_freq(ticks: u64, from_hz: u64, to_hz: u64) -> u64 {
    if from_hz == 0 || to_hz == 0 || from_hz == to_hz {
        return ticks;
    }
    let whole = ticks / from_hz;
    let rem = ticks % from_hz;
    whole
        .saturating_mul(to_hz)
        .saturating_add(rem.saturating_mul(to_hz) / from_hz)
}

/// First two `u32`s of a CSwitch user payload: new TID, old TID.
pub fn parse_switch_tids(user: &[u8]) -> Option<(u32, u32)> {
    if user.len() < 8 {
        return None;
    }
    let new_tid = u32::from_ne_bytes(user[0..4].try_into().ok()?);
    let old_tid = u32::from_ne_bytes(user[4..8].try_into().ok()?);
    Some((new_tid, old_tid))
}

/// Rank processes on one logical CPU. Idle is already absent.
/// Descending time, then PID, then creation time. Not a top-N filter of storage.
#[cfg(any(test, feature = "ui"))]
pub fn rank_logical_cpu(sample: &LogicalCpuSample, limit: usize) -> Vec<LogicalCpuProcess> {
    let mut rows = sample.processes.clone();
    rows.sort_by(|left, right| {
        right
            .cpu_time_100ns
            .cmp(&left.cpu_time_100ns)
            .then(left.key.pid.cmp(&right.key.pid))
            .then(
                left.key
                    .creation_time_100ns
                    .cmp(&right.key.creation_time_100ns),
            )
    });
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(number: u8) -> LogicalCpuId {
        LogicalCpuId { group: 0, number }
    }

    fn ledger() -> SwitchLedger {
        let mut ledger = SwitchLedger::new();
        ledger.set_enabled(true);
        ledger
    }

    fn switch(ledger: &mut SwitchLedger, time: u64, number: u8, new_tid: u32) {
        ledger.push(CpuEvent::Switch {
            time,
            cpu: cpu(number),
            old_tid: 0,
            new_tid,
        });
    }

    #[test]
    fn processor_index_splits_groups_of_64() {
        assert_eq!(
            LogicalCpuId::from_processor_index(0),
            LogicalCpuId {
                group: 0,
                number: 0
            }
        );
        assert_eq!(
            LogicalCpuId::from_processor_index(63),
            LogicalCpuId {
                group: 0,
                number: 63
            }
        );
        assert_eq!(
            LogicalCpuId::from_processor_index(64),
            LogicalCpuId {
                group: 1,
                number: 0
            }
        );
        assert_eq!(
            LogicalCpuId::from_processor_index(65),
            LogicalCpuId {
                group: 1,
                number: 1
            }
        );
    }

    #[test]
    fn interval_time_is_percent_of_that_cpu_only() {
        let mut ledger = ledger();
        ledger.seed_thread(10, 100, 5, "app.exe");
        switch(&mut ledger, 0, 3, 0);
        let _ = ledger.close(1_000);
        switch(&mut ledger, 1_000, 3, 10);
        switch(&mut ledger, 5_000, 3, 0);
        let snap = ledger.close(11_000);
        assert_eq!(snap.state, CpuDetailState::Ready);
        let cpu3 = snap
            .processors
            .iter()
            .find(|cpu| cpu.id.number == 3)
            .expect("cpu 3");
        let app = cpu3
            .processes
            .iter()
            .find(|row| row.name == "app.exe")
            .expect("app");
        assert_eq!(app.cpu_time_100ns, 4_000);
        assert!((app.percent - 40.0).abs() < 0.01, "{}", app.percent);
        assert!(cpu3.processes.iter().all(|row| row.key.pid != 0));
    }

    #[test]
    fn a_running_thread_is_split_at_the_interval_boundary() {
        let mut ledger = ledger();
        ledger.seed_thread(10, 100, 5, "app.exe");
        switch(&mut ledger, 0, 0, 0);
        let _ = ledger.close(100);
        switch(&mut ledger, 100, 0, 10);
        let first = ledger.close(1_000);
        let second = ledger.close(1_500);
        let time = |snap: &CpuDetailSnapshot| {
            snap.processors
                .iter()
                .find_map(|cpu| cpu.processes.first().map(|row| row.cpu_time_100ns))
                .unwrap_or(0)
        };
        assert_eq!(first.state, CpuDetailState::Ready);
        assert_eq!(time(&first), 900);
        assert_eq!(time(&second), 500);
    }

    #[test]
    fn migration_credits_each_logical_cpu_separately() {
        let mut ledger = ledger();
        ledger.seed_thread(10, 100, 5, "mover.exe");
        switch(&mut ledger, 0, 0, 0);
        switch(&mut ledger, 0, 1, 0);
        let _ = ledger.close(0);
        switch(&mut ledger, 100, 0, 10);
        switch(&mut ledger, 400, 0, 0);
        switch(&mut ledger, 400, 1, 10);
        switch(&mut ledger, 700, 1, 0);
        let snap = ledger.close(1_000);
        let on = |number: u8| {
            snap.processors
                .iter()
                .find(|cpu| cpu.id.number == number)
                .and_then(|cpu| cpu.processes.first().map(|row| row.cpu_time_100ns))
                .unwrap_or(0)
        };
        assert_eq!(on(0), 300);
        assert_eq!(on(1), 300);
    }

    #[test]
    fn pid_reuse_keeps_creation_time() {
        let mut ledger = ledger();
        ledger.push(CpuEvent::ProcessStart {
            pid: 50,
            create_time_100ns: 1,
            name: "old.exe".into(),
        });
        ledger.push(CpuEvent::ThreadStart {
            tid: 7,
            pid: 50,
            create_time_100ns: 1,
        });
        switch(&mut ledger, 0, 0, 0);
        let _ = ledger.close(0);
        switch(&mut ledger, 10, 0, 7);
        switch(&mut ledger, 110, 0, 0);
        ledger.push(CpuEvent::ThreadEnd { tid: 7 });
        ledger.push(CpuEvent::ProcessStart {
            pid: 50,
            create_time_100ns: 9,
            name: "new.exe".into(),
        });
        ledger.push(CpuEvent::ThreadStart {
            tid: 8,
            pid: 50,
            create_time_100ns: 9,
        });
        switch(&mut ledger, 200, 0, 8);
        switch(&mut ledger, 250, 0, 0);
        let snap = ledger.close(1_000);
        let cpu0 = snap
            .processors
            .iter()
            .find(|cpu| cpu.id.number == 0)
            .unwrap();
        let old = cpu0
            .processes
            .iter()
            .find(|row| row.name == "old.exe")
            .unwrap();
        let new = cpu0
            .processes
            .iter()
            .find(|row| row.name == "new.exe")
            .unwrap();
        assert_eq!(old.key.creation_time_100ns, 1);
        assert_eq!(new.key.creation_time_100ns, 9);
        assert_eq!(old.cpu_time_100ns, 100);
        assert_eq!(new.cpu_time_100ns, 50);
    }

    #[test]
    fn tid_reuse_without_a_start_stays_unknown() {
        let mut ledger = ledger();
        ledger.seed_thread(4, 20, 1, "first.exe");
        switch(&mut ledger, 0, 0, 0);
        let _ = ledger.close(0);
        switch(&mut ledger, 10, 0, 4);
        switch(&mut ledger, 40, 0, 0);
        ledger.push(CpuEvent::ThreadEnd { tid: 4 });
        switch(&mut ledger, 50, 0, 4);
        switch(&mut ledger, 90, 0, 0);
        let snap = ledger.close(100);
        let cpu0 = snap
            .processors
            .iter()
            .find(|cpu| cpu.id.number == 0)
            .unwrap();
        assert!(cpu0.processes.iter().all(|row| row.name == "first.exe"));
        assert_eq!(cpu0.processes[0].cpu_time_100ns, 30);
    }

    #[test]
    fn lost_events_make_the_interval_unavailable() {
        let mut ledger = ledger();
        ledger.seed_thread(4, 20, 1, "app.exe");
        switch(&mut ledger, 0, 0, 4);
        let _ = ledger.close(10);
        ledger.mark_lost(3);
        switch(&mut ledger, 20, 0, 0);
        let snap = ledger.close(100);
        assert_eq!(snap.state, CpuDetailState::Unavailable);
        assert!(snap.processors.iter().all(|cpu| cpu.processes.is_empty()));
        assert!(snap.mean_busy_percent.is_none());
        assert_eq!(snap.lost_events, 3);
    }

    #[test]
    fn dpc_time_is_not_given_to_the_running_process() {
        let mut ledger = ledger();
        ledger.seed_thread(4, 20, 1, "app.exe");
        switch(&mut ledger, 0, 0, 0);
        let _ = ledger.close(0);
        switch(&mut ledger, 100, 0, 4);
        ledger.push(CpuEvent::Unattributed {
            time: 250,
            cpu: cpu(0),
            duration: 40,
        });
        switch(&mut ledger, 400, 0, 0);
        let snap = ledger.close(1_000);
        let app = snap.processors[0].processes.first().unwrap();
        assert_eq!(app.cpu_time_100ns, 260);
    }

    #[test]
    fn disabled_ledger_does_not_keep_time() {
        let mut ledger = SwitchLedger::new();
        assert!(!ledger.enabled());
        ledger.seed_thread(4, 20, 1, "app.exe");
        switch(&mut ledger, 0, 0, 4);
        let snap = ledger.close(1_000);
        assert_eq!(snap.state, CpuDetailState::Off);
        assert!(snap.processors.is_empty());
        ledger.set_enabled(true);
        switch(&mut ledger, 1_000, 0, 4);
        let _ = ledger.close(1_000);
        ledger.set_enabled(false);
        let off = ledger.close(2_000);
        assert_eq!(off.state, CpuDetailState::Off);
    }

    #[test]
    fn ranking_is_descending_and_ties_follow_pid() {
        let sample = LogicalCpuSample {
            id: cpu(1),
            usage_percent: crate::metric::MetricValue::Value(50.0),
            processes: vec![
                row(30, 1, "c", 10),
                row(10, 1, "a", 40),
                row(20, 2, "b2", 40),
                row(20, 1, "b1", 40),
            ],
        };
        let ranked = rank_logical_cpu(&sample, 5);
        let names: Vec<_> = ranked.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["a", "b1", "b2", "c"]);
    }

    #[test]
    fn an_expected_cpu_without_a_switch_stays_unavailable() {
        let mut ledger = ledger();
        ledger.expect([cpu(0), cpu(1)]);
        ledger.seed_thread(4, 20, 1, "app.exe");
        switch(&mut ledger, 0, 0, 0);
        let _ = ledger.close(0);
        switch(&mut ledger, 10, 0, 4);
        let snap = ledger.close(100);
        assert_eq!(snap.state, CpuDetailState::Ready);
        let cpu1 = snap
            .processors
            .iter()
            .find(|row| row.id.number == 1)
            .unwrap();
        assert!(matches!(
            cpu1.usage_percent,
            crate::metric::MetricValue::Unavailable
        ));
        assert!(cpu1.processes.is_empty());
    }

    #[test]
    fn needs_admin_has_no_invented_rows() {
        let snap = CpuDetailSnapshot::needs_admin();
        assert_eq!(snap.state, CpuDetailState::NeedsAdmin);
        assert!(snap.processors.is_empty());
        assert!(snap.mean_busy_percent.is_none());
    }

    #[test]
    fn short_switch_payload_is_ignored() {
        assert_eq!(parse_switch_tids(&[1, 2, 3]), None);
        assert_eq!(parse_switch_tids(&7u32.to_ne_bytes()), None);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&11u32.to_ne_bytes());
        bytes.extend_from_slice(&22u32.to_ne_bytes());
        assert_eq!(parse_switch_tids(&bytes), Some((11, 22)));
    }

    #[test]
    fn filetime_events_closed_on_qpc_are_only_zero_or_one_hundred() {
        // ProcessTrace's default conversion writes 100 ns since 1601.
        // The old close() sampled QueryPerformanceCounter. Same 500 ms of
        // partial busy time then collapses: idle-at-the-boundary becomes 0%,
        // busy-at-the-boundary becomes 100%.
        let filetime = 133_000_000_000_000_000u64;
        let qpc = 80_000_000_000u64;
        let snap = run_partial(|ms| filetime + ms * 10_000, qpc, qpc + 5_000_000);
        let usage = |number: u8| usage_of(&snap, number);
        assert_eq!(usage(0), 0.0, "partial cpu collapsed");
        assert_eq!(usage(1), 0.0, "partial cpu collapsed");
        assert_eq!(usage(2), 100.0, "busy-at-boundary collapsed");
        assert_eq!(usage(3), 0.0, "idle cpu collapsed");
    }

    #[test]
    fn partial_switches_on_one_clock_stay_fractional_and_at_most_100() {
        let assert_partial = |snap: &CpuDetailSnapshot| {
            let usage = |number: u8| usage_of(snap, number);
            assert!((usage(0) - 38.0).abs() < 0.05, "cpu0 {}", usage(0));
            assert!((usage(1) - 12.0).abs() < 0.05, "cpu1 {}", usage(1));
            assert!((usage(2) - 100.0).abs() < 0.05, "cpu2 {}", usage(2));
            assert!((usage(3) - 0.0).abs() < 0.05, "cpu3 {}", usage(3));
            for cpu in &snap.processors {
                let value = cpu.usage_percent.as_option().expect("usage");
                assert!(value <= 100.0, "{value}");
                for process in &cpu.processes {
                    assert!(process.percent <= 100.0, "{}", process.percent);
                    assert!(process.percent > 0.0);
                }
            }
            let cpu0 = snap
                .processors
                .iter()
                .find(|cpu| cpu.id.number == 0)
                .unwrap();
            let app = cpu0
                .processes
                .iter()
                .find(|row| row.name == "app.exe")
                .unwrap();
            let other = cpu0
                .processes
                .iter()
                .find(|row| row.name == "other.exe")
                .unwrap();
            assert!((app.percent - 30.0).abs() < 0.05, "{}", app.percent);
            assert!((other.percent - 8.0).abs() < 0.05, "{}", other.percent);
            assert!(usage(0) > 0.0 && usage(0) < 100.0);
            assert!(usage(1) > 0.0 && usage(1) < 100.0);
        };

        let hz = 10_000_000u64;
        let origin = 9_000_000_000_000u64;
        let clock = SessionClock::qpc(hz, hz);
        let snap = run_partial(
            |ms| clock.event_ticks(origin + hz / 1000 * ms),
            clock.close_ticks(origin, 0),
            clock.close_ticks(origin + hz / 1000 * 500, 0),
        );
        assert_partial(&snap);

        // Header PerfFreq is a 3 GHz counter. The close sample is 10 MHz QPC.
        // Both sides are scaled, so 150 ms of a 500 ms interval stays 30%.
        let event_hz = 3_000_000_000u64;
        let qpc_hz = 10_000_000u64;
        let clock = SessionClock::qpc(event_hz, qpc_hz);
        let raw_end = origin + event_hz / 1000 * 500;
        let snap = run_partial(
            |ms| clock.event_ticks(origin + event_hz / 1000 * ms),
            clock.close_ticks(scale_freq(origin, event_hz, qpc_hz), 0),
            clock.close_ticks(scale_freq(raw_end, event_hz, qpc_hz), 0),
        );
        assert_partial(&snap);

        let clock = SessionClock::filetime();
        let snap = run_partial(
            |ms| clock.event_ticks(origin + 10_000 * ms),
            clock.close_ticks(0, origin),
            clock.close_ticks(0, origin + 5_000_000),
        );
        assert_partial(&snap);
    }

    #[test]
    fn classify_timestamp_follows_the_live_counter() {
        let qpc_hz = 10_000_000u64;
        let qpc = 80_000_000_000u64;
        let filetime = 133_000_000_000_000_000u64;
        assert_eq!(
            classify_timestamp(qpc + 20_000, qpc, filetime, qpc_hz),
            ClockMode::Qpc
        );
        assert_eq!(
            classify_timestamp(filetime + 50_000, qpc, filetime, qpc_hz),
            ClockMode::FileTime
        );
        assert_eq!(
            scale_freq(1_500_000_000, 3_000_000_000, 10_000_000),
            5_000_000
        );
        assert_eq!(SessionClock::qpc(0, qpc_hz).event_ticks(qpc), qpc);
        assert_eq!(
            SessionClock::filetime().close_ticks(qpc, filetime),
            filetime
        );
        assert_eq!(
            SessionClock::qpc(qpc_hz, qpc_hz).close_ticks(qpc, filetime),
            qpc
        );
    }

    /// Several switches per CPU inside one 500 ms interval.
    /// `at(ms)` is the event clock. `warm_close` / `end_close` are the interval
    /// ends the consumer actually passes in.
    fn run_partial(at: impl Fn(u64) -> u64, warm_close: u64, end_close: u64) -> CpuDetailSnapshot {
        let mut ledger = ledger();
        ledger.seed_thread(10, 100, 1, "app.exe");
        ledger.seed_thread(11, 101, 1, "other.exe");
        switch(&mut ledger, at(0), 0, 0);
        switch(&mut ledger, at(0), 1, 0);
        switch(&mut ledger, at(0), 2, 10);
        switch(&mut ledger, at(0), 3, 0);
        let _ = ledger.close(warm_close);
        // CPU 0: idle 100 ms, app 150 ms, idle 150 ms, other 40 ms, idle 60 ms → 38%.
        switch(&mut ledger, at(100), 0, 10);
        switch(&mut ledger, at(250), 0, 0);
        switch(&mut ledger, at(400), 0, 11);
        switch(&mut ledger, at(440), 0, 0);
        // CPU 1: idle 200 ms, app 60 ms, idle the rest → 12%.
        switch(&mut ledger, at(200), 1, 10);
        switch(&mut ledger, at(260), 1, 0);
        // CPU 2 stays non-idle the whole interval → 100%, which is a real full CPU.
        switch(&mut ledger, at(200), 2, 11);
        // CPU 3 stays idle → 0%.
        switch(&mut ledger, at(100), 3, 0);
        ledger.close(end_close)
    }

    fn usage_of(snap: &CpuDetailSnapshot, number: u8) -> f64 {
        snap.processors
            .iter()
            .find(|cpu| cpu.id.number == number)
            .and_then(|cpu| cpu.usage_percent.as_option())
            .unwrap_or(-1.0)
    }

    fn row(pid: u32, create: u64, name: &str, time: u64) -> LogicalCpuProcess {
        LogicalCpuProcess {
            key: ProcessKey {
                pid,
                creation_time_100ns: create,
            },
            name: name.to_string(),
            cpu_time_100ns: time,
            percent: time as f64,
        }
    }
}
