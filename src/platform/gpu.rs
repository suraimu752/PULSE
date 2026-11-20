//! GPU engine and memory counters via PDH.
//!
//! Utilization of several engines is the max, never the sum. Dedicated and
//! shared bytes stay separate. Adapter memory is preferred for the system
//! total because summing process shared memory double-counts.

use std::collections::HashMap;

use crate::collector::MetricEnricher;
use crate::gpuagg::{
    fold_engine_percents, parse_engine_instance, parse_luid_token, parse_process_memory_instance,
    EngineClass,
};
use crate::metric::MetricValue;
use crate::model::{GpuAdapter, GpuPresence, RawSample};

use super::pdh::{PdhQuery, PdhSample};

pub struct GpuEnricher {
    engines: Option<PdhQuery>,
    process_dedicated: Option<PdhQuery>,
    process_shared: Option<PdhQuery>,
    adapter_dedicated: Option<PdhQuery>,
    adapter_shared: Option<PdhQuery>,
    opened: bool,
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
            opened: false,
            saw_engine: false,
            confirmed_empty: false,
        }
    }

    fn open_once(&mut self) {
        if self.opened {
            return;
        }
        self.engines = PdhQuery::open(r"\GPU Engine(*)\Utilization Percentage");
        self.process_dedicated = PdhQuery::open(r"\GPU Process Memory(*)\Dedicated Usage");
        self.process_shared = PdhQuery::open(r"\GPU Process Memory(*)\Shared Usage");
        self.adapter_dedicated = PdhQuery::open(r"\GPU Adapter Memory(*)\Dedicated Usage");
        self.adapter_shared = PdhQuery::open(r"\GPU Adapter Memory(*)\Shared Usage");
        self.opened = true;
    }
}

impl MetricEnricher for GpuEnricher {
    fn id(&self) -> &'static str {
        "gpu"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        self.open_once();
        if self.engines.is_none() {
            mark_unknown(sample);
            return;
        }
        match self.engines.as_ref().and_then(PdhQuery::collect) {
            Some(PdhSample::Ready(values)) if values.is_empty() && !self.saw_engine => {
                self.confirmed_empty = true;
                mark_absent(sample);
            }
            Some(PdhSample::Ready(values)) if values.is_empty() => mark_gap(sample, true),
            Some(PdhSample::Ready(values)) => {
                if apply_engines(sample, &values) {
                    self.saw_engine = true;
                    self.confirmed_empty = false;
                    apply_memory(sample, self);
                } else if self.saw_engine {
                    mark_gap(sample, true);
                } else {
                    mark_unknown(sample);
                }
            }
            Some(PdhSample::NoData) | None => {
                if self.confirmed_empty && !self.saw_engine {
                    mark_absent(sample);
                } else {
                    mark_gap(sample, self.saw_engine);
                }
            }
        }
    }
}

fn apply_engines(sample: &mut RawSample, values: &[(String, f64)]) -> bool {
    let mut by_process: HashMap<u32, ClassBuckets> = HashMap::new();
    let mut by_adapter: HashMap<u64, ClassBuckets> = HashMap::new();
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

fn apply_memory(sample: &mut RawSample, enricher: &GpuEnricher) {
    let dedicated = collect_bytes(&enricher.process_dedicated);
    let shared = collect_bytes(&enricher.process_shared);
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
            }
        }
    }
    let adapter_dedicated = max_by_luid(collect_bytes(&enricher.adapter_dedicated).as_deref());
    let adapter_shared = max_by_luid(collect_bytes(&enricher.adapter_shared).as_deref());
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

fn collect_bytes(query: &Option<PdhQuery>) -> Option<Vec<(String, f64)>> {
    let query = query.as_ref()?;
    match query.collect()? {
        PdhSample::Ready(values) => Some(values),
        PdhSample::NoData => None,
    }
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
