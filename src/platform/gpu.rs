//! GPU engine and memory counters via PDH.
//!
//! Utilization of several engines is the max, never the sum. Dedicated and
//! shared bytes stay separate. Adapter memory is preferred for the system
//! total because summing process shared memory double-counts.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::collector::MetricEnricher;
use crate::gpuagg::{
    fold_engine_percents, parse_engine_instance, parse_luid_token, parse_process_memory_instance,
    EngineClass,
};
use crate::metric::MetricValue;
use crate::model::{GpuAdapter, GpuPresence, ProcessGpu, RawProcess, RawSample};

use super::pdh::{PdhQuery, PdhSample};

pub struct GpuEnricher {
    engines: Option<PdhQuery>,
    process_dedicated: Option<PdhQuery>,
    process_shared: Option<PdhQuery>,
    adapter_dedicated: Option<PdhQuery>,
    adapter_shared: Option<PdhQuery>,
    engine_error: u32,
    since: Option<Instant>,
    saw_engine: bool,
    confirmed_empty: bool,
}

impl GpuEnricher {
    pub fn new() -> Self {
        Self {
            engines: None,
            process_dedicated: None,
            process_shared: None,
            adapter_dedicated: None,
            adapter_shared: None,
            engine_error: 0,
            since: None,
            saw_engine: false,
            confirmed_empty: false,
        }
    }

    fn ensure_open(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
        if self.engines.is_none() {
            match PdhQuery::open(r"\GPU Engine(*)\Utilization Percentage") {
                Ok(query) => self.engines = Some(query),
                Err(code) => self.engine_error = code,
            }
        }
        open_into(
            &mut self.process_dedicated,
            r"\GPU Process Memory(*)\Dedicated Usage",
        );
        open_into(
            &mut self.process_shared,
            r"\GPU Process Memory(*)\Shared Usage",
        );
        open_into(
            &mut self.adapter_dedicated,
            r"\GPU Adapter Memory(*)\Dedicated Usage",
        );
        open_into(
            &mut self.adapter_shared,
            r"\GPU Adapter Memory(*)\Shared Usage",
        );
    }
}

fn open_into(slot: &mut Option<PdhQuery>, path: &str) {
    if slot.is_none() {
        *slot = PdhQuery::open(path).ok();
    }
}

impl MetricEnricher for GpuEnricher {
    fn id(&self) -> &'static str {
        "gpu"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        self.ensure_open();
        if self.engines.is_none() {
            sample.note(&format!("GPU PDH({:#X})", self.engine_error));
            mark_unknown(sample);
            // Engine counters can be missing while process memory counters still work.
            apply_memory(sample, self);
            return;
        }
        let stale = self
            .since
            .is_some_and(|opened| opened.elapsed() >= Duration::from_secs(2));
        let engines = self.engines.as_ref().and_then(PdhQuery::collect);
        let reopen = self.engines.as_ref().is_some_and(PdhQuery::should_reopen);
        match engines {
            Some(PdhSample::Ready(values)) if values.is_empty() && !self.saw_engine => {
                self.confirmed_empty = true;
                mark_absent(sample);
            }
            Some(PdhSample::Ready(values)) if values.is_empty() => {
                mark_gap(sample, true);
                apply_memory(sample, self);
            }
            Some(PdhSample::Ready(values)) => {
                if apply_engines(sample, &values) {
                    self.saw_engine = true;
                    self.confirmed_empty = false;
                    apply_memory(sample, self);
                } else if self.saw_engine {
                    mark_gap(sample, true);
                    apply_memory(sample, self);
                } else {
                    mark_unknown(sample);
                    apply_memory(sample, self);
                }
            }
            Some(PdhSample::NoData) => {
                if stale && !self.saw_engine {
                    sample.note("GPU PDH にデータが来ません");
                }
                if self.confirmed_empty && !self.saw_engine {
                    mark_absent(sample);
                } else {
                    mark_gap(sample, self.saw_engine);
                    apply_memory(sample, self);
                }
            }
            None => {
                if let Some(query) = self.engines.as_ref() {
                    sample.note(&format!("GPU PDH({:#X})", query.status()));
                }
                // Drop the engine query only after repeated hard failures.
                // Memory counters stay on their own queries.
                if reopen {
                    self.engines = None;
                }
                if self.confirmed_empty && !self.saw_engine {
                    mark_absent(sample);
                } else {
                    mark_gap(sample, self.saw_engine);
                    apply_memory(sample, self);
                }
            }
        }
    }
}

fn apply_engines(sample: &mut RawSample, values: &[(String, f64)]) -> bool {
    let mut by_process: HashMap<u32, ClassBuckets> = HashMap::new();
    let mut by_adapter: HashMap<u64, ClassBuckets> = HashMap::new();
    let mut by_process_adapter: HashMap<(u32, u64), ClassBuckets> = HashMap::new();
    for (name, value) in values {
        let Some(instance) = parse_engine_instance(name) else {
            continue;
        };
        if instance.class == EngineClass::Other {
            continue;
        }
        by_process
            .entry(instance.pid)
            .or_default()
            .push(instance.class, *value);
        by_adapter
            .entry(instance.luid)
            .or_default()
            .push(instance.class, *value);
        by_process_adapter
            .entry((instance.pid, instance.luid))
            .or_default()
            .push(instance.class, *value);
    }
    if by_adapter.is_empty() {
        // Instances existed but none matched the documented name. Do not invent 0%.
        return false;
    }
    sample.gpu_presence = GpuPresence::Present;
    for process in &mut sample.processes {
        let buckets = by_process.get(&process.pid);
        process.gpu_3d_percent = bucket_or_zero(buckets, EngineClass::ThreeD);
        process.gpu_encode_percent = bucket_or_zero(buckets, EngineClass::Encode);
        process.gpu_decode_percent = bucket_or_zero(buckets, EngineClass::Decode);
        process.gpus = process_gpus(&by_process_adapter, process.pid);
    }
    sample.system_gpu_3d_percent = class_or_zero(&by_adapter, EngineClass::ThreeD);
    sample.system_gpu_encode_percent = class_or_zero(&by_adapter, EngineClass::Encode);
    sample.system_gpu_decode_percent = class_or_zero(&by_adapter, EngineClass::Decode);
    sample.gpus = by_adapter
        .into_iter()
        .map(|(luid, buckets)| GpuAdapter {
            luid,
            name: format!("GPU {luid:#x}"),
            util_3d: class_metric(&buckets.three_d),
            util_encode: class_metric(&buckets.encode),
            util_decode: class_metric(&buckets.decode),
            dedicated_bytes: MetricValue::Unavailable,
            shared_bytes: MetricValue::Unavailable,
        })
        .collect();
    true
}

fn apply_memory(sample: &mut RawSample, enricher: &mut GpuEnricher) {
    let dedicated = collect_bytes(&mut enricher.process_dedicated);
    let shared = collect_bytes(&mut enricher.process_shared);
    match (&dedicated, &shared) {
        (None, None) => {
            for process in &mut sample.processes {
                process.vram_dedicated_bytes = MetricValue::Unavailable;
                process.vram_shared_bytes = MetricValue::Unavailable;
            }
        }
        _ => {
            let dedicated_by_pid = sum_by_pid(dedicated.as_deref().unwrap_or(&[]));
            let shared_by_pid = sum_by_pid(shared.as_deref().unwrap_or(&[]));
            for process in &mut sample.processes {
                process.vram_dedicated_bytes = if dedicated.is_some() {
                    MetricValue::Value(dedicated_by_pid.get(&process.pid).copied().unwrap_or(0))
                } else {
                    MetricValue::Unavailable
                };
                process.vram_shared_bytes = if shared.is_some() {
                    MetricValue::Value(shared_by_pid.get(&process.pid).copied().unwrap_or(0))
                } else {
                    MetricValue::Unavailable
                };
                attach_process_vram(process, dedicated.as_deref(), shared.as_deref());
            }
        }
    }
    let adapter_dedicated = max_by_luid(collect_bytes(&mut enricher.adapter_dedicated).as_deref());
    let adapter_shared = max_by_luid(collect_bytes(&mut enricher.adapter_shared).as_deref());
    if adapter_dedicated.is_empty() && adapter_shared.is_empty() {
        sample.system_vram_dedicated_bytes = if dedicated.is_some() {
            MetricValue::Value(sample.processes.iter().fold(0u64, |sum, process| {
                sum.saturating_add(process.vram_dedicated_bytes.as_option().unwrap_or(0))
            }))
        } else {
            MetricValue::Unavailable
        };
        // Shared process memory overlaps. Do not add it up into a system total.
        sample.system_vram_shared_bytes = MetricValue::Unavailable;
        return;
    }
    for gpu in &mut sample.gpus {
        if let Some(bytes) = adapter_dedicated.get(&gpu.luid) {
            gpu.dedicated_bytes = MetricValue::Value(*bytes);
        }
        if let Some(bytes) = adapter_shared.get(&gpu.luid) {
            gpu.shared_bytes = MetricValue::Value(*bytes);
        }
    }
    sample.system_vram_dedicated_bytes = sum_map(&adapter_dedicated);
    sample.system_vram_shared_bytes = sum_map(&adapter_shared);
}

fn collect_bytes(slot: &mut Option<PdhQuery>) -> Option<Vec<(String, f64)>> {
    let (values, reopen) = {
        let query = slot.as_ref()?;
        match query.collect() {
            Some(PdhSample::Ready(values)) => (Some(values), false),
            Some(PdhSample::NoData) => (None, false),
            None => (None, query.should_reopen()),
        }
    };
    if reopen {
        *slot = None;
    }
    values
}

fn sum_by_pid(values: &[(String, f64)]) -> HashMap<u32, u64> {
    let mut best: HashMap<(u32, u64), u64> = HashMap::new();
    for (name, value) in values {
        let Some((pid, luid)) = parse_process_memory_instance(name) else {
            continue;
        };
        let Some(bytes) = as_bytes(*value) else {
            continue;
        };
        let entry = best.entry((pid, luid)).or_insert(0);
        *entry = (*entry).max(bytes);
    }
    let mut totals: HashMap<u32, u64> = HashMap::new();
    for ((pid, _), bytes) in best {
        let entry = totals.entry(pid).or_insert(0);
        *entry = (*entry).saturating_add(bytes);
    }
    totals
}

fn max_by_luid(values: Option<&[(String, f64)]>) -> HashMap<u64, u64> {
    let mut best: HashMap<u64, u64> = HashMap::new();
    let Some(values) = values else {
        return best;
    };
    for (name, value) in values {
        let Some(luid) = parse_luid_token(name) else {
            continue;
        };
        let Some(bytes) = as_bytes(*value) else {
            continue;
        };
        let entry = best.entry(luid).or_insert(0);
        *entry = (*entry).max(bytes);
    }
    best
}

fn sum_map(values: &HashMap<u64, u64>) -> MetricValue<u64> {
    if values.is_empty() {
        MetricValue::Unavailable
    } else {
        MetricValue::Value(
            values
                .values()
                .fold(0u64, |sum, value| sum.saturating_add(*value)),
        )
    }
}

fn as_bytes(value: f64) -> Option<u64> {
    if value.is_finite() && value >= 0.0 && value <= u64::MAX as f64 {
        Some(value as u64)
    } else {
        None
    }
}

fn process_gpus(buckets: &HashMap<(u32, u64), ClassBuckets>, pid: u32) -> Vec<ProcessGpu> {
    let mut rows: Vec<ProcessGpu> = buckets
        .iter()
        .filter(|((process_pid, _), _)| *process_pid == pid)
        .map(|((_, luid), buckets)| ProcessGpu {
            luid: *luid,
            gpu_3d_percent: bucket_or_zero(Some(buckets), EngineClass::ThreeD),
            gpu_encode_percent: bucket_or_zero(Some(buckets), EngineClass::Encode),
            gpu_decode_percent: bucket_or_zero(Some(buckets), EngineClass::Decode),
            vram_dedicated_bytes: MetricValue::Unavailable,
            vram_shared_bytes: MetricValue::Unavailable,
        })
        .collect();
    rows.sort_by_key(|row| row.luid);
    rows
}

fn attach_process_vram(
    process: &mut RawProcess,
    dedicated: Option<&[(String, f64)]>,
    shared: Option<&[(String, f64)]>,
) {
    let dedicated = memory_by_pid_luid(dedicated, process.pid);
    let shared = memory_by_pid_luid(shared, process.pid);
    let mut luids: Vec<u64> = dedicated.keys().chain(shared.keys()).copied().collect();
    luids.sort_unstable();
    luids.dedup();
    for luid in luids {
        let row = if let Some(row) = process.gpus.iter_mut().find(|row| row.luid == luid) {
            row
        } else {
            process.gpus.push(ProcessGpu {
                luid,
                gpu_3d_percent: MetricValue::Unavailable,
                gpu_encode_percent: MetricValue::Unavailable,
                gpu_decode_percent: MetricValue::Unavailable,
                vram_dedicated_bytes: MetricValue::Unavailable,
                vram_shared_bytes: MetricValue::Unavailable,
            });
            process.gpus.last_mut().unwrap()
        };
        if let Some(bytes) = dedicated.get(&luid) {
            row.vram_dedicated_bytes = MetricValue::Value(*bytes);
        }
        if let Some(bytes) = shared.get(&luid) {
            row.vram_shared_bytes = MetricValue::Value(*bytes);
        }
    }
    process.gpus.sort_by_key(|row| row.luid);
}

fn memory_by_pid_luid(values: Option<&[(String, f64)]>, pid: u32) -> HashMap<u64, u64> {
    let mut best = HashMap::new();
    let Some(values) = values else {
        return best;
    };
    for (name, value) in values {
        let Some((row_pid, luid)) = parse_process_memory_instance(name) else {
            continue;
        };
        if row_pid != pid {
            continue;
        }
        let Some(bytes) = as_bytes(*value) else {
            continue;
        };
        let entry = best.entry(luid).or_insert(0);
        *entry = (*entry).max(bytes);
    }
    best
}

fn bucket_or_zero(buckets: Option<&ClassBuckets>, class: EngineClass) -> MetricValue<f64> {
    match buckets {
        Some(buckets) if !buckets.of(class).is_empty() => {
            fold_engine_percents(buckets.of(class).iter().copied())
        }
        _ => MetricValue::Value(0.0),
    }
}

fn class_or_zero(adapters: &HashMap<u64, ClassBuckets>, class: EngineClass) -> MetricValue<f64> {
    let values: Vec<f64> = adapters
        .values()
        .flat_map(|buckets| buckets.of(class).iter().copied())
        .collect();
    class_metric(&values)
}

fn class_metric(values: &[f64]) -> MetricValue<f64> {
    if values.is_empty() {
        MetricValue::Value(0.0)
    } else {
        fold_engine_percents(values.iter().copied())
    }
}

fn mark_unknown(sample: &mut RawSample) {
    sample.gpu_presence = GpuPresence::Unknown;
    fill_gpu(sample, MetricValue::Unavailable, MetricValue::Unavailable);
}

fn mark_absent(sample: &mut RawSample) {
    sample.gpu_presence = GpuPresence::None;
    sample.gpus.clear();
    fill_gpu(sample, MetricValue::Unsupported, MetricValue::Unsupported);
}

fn mark_gap(sample: &mut RawSample, present: bool) {
    sample.gpu_presence = if present {
        GpuPresence::Present
    } else {
        GpuPresence::Unknown
    };
    sample.gpus.clear();
    fill_gpu(sample, MetricValue::Unavailable, MetricValue::Unavailable);
}

fn fill_gpu(sample: &mut RawSample, percent: MetricValue<f64>, bytes: MetricValue<u64>) {
    sample.system_gpu_3d_percent = percent;
    sample.system_gpu_encode_percent = percent;
    sample.system_gpu_decode_percent = percent;
    sample.system_vram_dedicated_bytes = bytes;
    sample.system_vram_shared_bytes = bytes;
    for process in &mut sample.processes {
        process.gpu_3d_percent = percent;
        process.gpu_encode_percent = percent;
        process.gpu_decode_percent = percent;
        process.vram_dedicated_bytes = bytes;
        process.vram_shared_bytes = bytes;
        process.gpus.clear();
    }
}

#[derive(Default)]
struct ClassBuckets {
    three_d: Vec<f64>,
    encode: Vec<f64>,
    decode: Vec<f64>,
}

impl ClassBuckets {
    fn push(&mut self, class: EngineClass, value: f64) {
        match class {
            EngineClass::ThreeD => self.three_d.push(value),
            EngineClass::Encode => self.encode.push(value),
            EngineClass::Decode => self.decode.push(value),
            EngineClass::Other => {}
        }
    }

    fn of(&self, class: EngineClass) -> &[f64] {
        match class {
            EngineClass::ThreeD => &self.three_d,
            EngineClass::Encode => &self.encode,
            EngineClass::Decode => &self.decode,
            EngineClass::Other => &[],
        }
    }
}
