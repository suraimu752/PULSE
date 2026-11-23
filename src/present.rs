//! Turn snapshots into the strings the window shows. No dummy numbers are introduced here.

use crate::format::{
    bytes_kind, format_bytes, format_memory_pair, format_percent, format_rate_pair, percent_kind,
    DisplayKind,
};
use crate::metric::MetricValue;
use crate::model::{
    GpuAdapter, GpuPresence, ProcessNetMode, ProcessSnapshot, ResourceId, Snapshot,
};
use crate::sort::{sort_processes, SortColumn};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedRow {
    pub name: String,
    pub pid: String,
    pub cpu: String,
    pub cpu_kind: DisplayKind,
    pub memory: String,
    pub memory_kind: DisplayKind,
    pub disk: String,
    pub disk_kind: DisplayKind,
    pub network: String,
    pub network_kind: DisplayKind,
    pub gpu3d: String,
    pub gpu3d_kind: DisplayKind,
    pub gpu_encode: String,
    pub gpu_encode_kind: DisplayKind,
    pub gpu_decode: String,
    pub gpu_decode_kind: DisplayKind,
    pub vram: String,
    pub vram_kind: DisplayKind,
    pub shared: String,
    pub shared_kind: DisplayKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailText {
    pub title: String,
    pub value: String,
    pub kind: DisplayKind,
    pub extra: String,
    pub note: String,
    pub show_graph: bool,
    pub graph_is_memory: bool,
}

#[cfg(feature = "ui")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceCard {
    pub text: String,
    pub kind: DisplayKind,
}

#[cfg(feature = "ui")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceCards {
    pub cpu: ResourceCard,
    pub memory: ResourceCard,
    pub disk: ResourceCard,
    pub network: ResourceCard,
    pub gpu3d: ResourceCard,
    pub gpu_encode: ResourceCard,
    pub gpu_decode: ResourceCard,
    pub vram_dedicated: ResourceCard,
    pub vram_shared: ResourceCard,
}

pub fn present_rows(
    snapshot: &Snapshot,
    column: SortColumn,
    descending: bool,
) -> Vec<PresentedRow> {
    let mut processes = snapshot.processes.clone();
    sort_processes(&mut processes, column, descending);
    processes
        .iter()
        .map(|process| present_row(process, snapshot.process_net))
        .collect()
}

pub fn status_line(
    snapshot: &Snapshot,
    self_cpu: MetricValue<f64>,
    self_memory: MetricValue<u64>,
    updated_text: &str,
) -> String {
    format!(
        "プロセス {} 件    最終更新 {updated_text}    CPU {}    メモリ {}    PULSE 自身 CPU {} / メモリ {}",
        snapshot.processes.len(),
        format_percent(snapshot.cpu_percent),
        format_memory_pair(snapshot.memory_used_bytes, snapshot.memory_total_bytes),
        format_percent(self_cpu),
        format_bytes(self_memory),
    )
}

#[cfg(feature = "ui")]
pub fn present_cards(snapshot: &Snapshot) -> ResourceCards {
    ResourceCards {
        cpu: card_percent(snapshot.cpu_percent),
        memory: ResourceCard {
            text: format_memory_pair(snapshot.memory_used_bytes, snapshot.memory_total_bytes),
            kind: memory_kind(snapshot.memory_used_bytes, snapshot.memory_total_bytes),
        },
        disk: card_pair(snapshot.disk_read_bps, snapshot.disk_write_bps),
        network: card_pair(snapshot.net_send_bps, snapshot.net_recv_bps),
        gpu3d: card_percent(snapshot.gpu_3d_percent),
        gpu_encode: card_percent(snapshot.gpu_encode_percent),
        gpu_decode: card_percent(snapshot.gpu_decode_percent),
        vram_dedicated: ResourceCard {
            text: format_bytes(snapshot.vram_dedicated_bytes),
            kind: bytes_kind(snapshot.vram_dedicated_bytes),
        },
        vram_shared: ResourceCard {
            text: format_bytes(snapshot.vram_shared_bytes),
            kind: bytes_kind(snapshot.vram_shared_bytes),
        },
    }
}

pub fn present_detail(snapshot: &Snapshot, resource: ResourceId) -> DetailText {
    present_detail_at(snapshot, resource, None)
}

/// Detail text for one snapshot. When `adapter` is set, a GPU resource shows
/// that adapter only. A missing adapter is N/A, not another GPU's number.
pub fn present_detail_at(
    snapshot: &Snapshot,
    resource: ResourceId,
    adapter: Option<u64>,
) -> DetailText {
    let mut detail = present_detail_system(snapshot, resource);
    if let Some(luid) = adapter {
        if let Some(text) = adapter_override(&detail, snapshot, resource, luid) {
            detail = text;
        }
    }
    detail
}

fn adapter_override(
    base: &DetailText,
    snapshot: &Snapshot,
    resource: ResourceId,
    luid: u64,
) -> Option<DetailText> {
    let gpu = snapshot.gpus.iter().find(|gpu| gpu.luid == luid);
    let (value, kind, show_graph) = match resource {
        ResourceId::Gpu3d => gpu_text(gpu, |gpu| gpu.util_3d, true),
        ResourceId::GpuEncode => gpu_text(gpu, |gpu| gpu.util_encode, true),
        ResourceId::GpuDecode => gpu_text(gpu, |gpu| gpu.util_decode, true),
        ResourceId::VramDedicated => gpu_text(gpu, |gpu| bytes_as_f64(gpu.dedicated_bytes), false),
        ResourceId::VramShared => gpu_text(gpu, |gpu| bytes_as_f64(gpu.shared_bytes), false),
        _ => return None,
    };
    let mut detail = base.clone();
    detail.value = value;
    detail.kind = kind;
    detail.show_graph = show_graph;
    Some(detail)
}

fn gpu_text(
    gpu: Option<&GpuAdapter>,
    metric: impl Fn(&GpuAdapter) -> MetricValue<f64>,
    percent: bool,
) -> (String, DisplayKind, bool) {
    let Some(gpu) = gpu else {
        return ("N/A".to_string(), DisplayKind::Unavailable, true);
    };
    let value = metric(gpu);
    if percent {
        (
            format_percent(value),
            percent_kind(value),
            !matches!(value, MetricValue::Unsupported),
        )
    } else {
        let bytes = match value {
            MetricValue::Value(number) if number.is_finite() && number >= 0.0 => {
                MetricValue::Value(number.round() as u64)
            }
            MetricValue::Value(_) => MetricValue::Unavailable,
            MetricValue::Unavailable => MetricValue::Unavailable,
            MetricValue::Unsupported => MetricValue::Unsupported,
        };
        (
            format_bytes(bytes),
            bytes_kind(bytes),
            !matches!(bytes, MetricValue::Unsupported),
        )
    }
}

fn bytes_as_f64(value: MetricValue<u64>) -> MetricValue<f64> {
    match value {
        MetricValue::Value(bytes) => MetricValue::Value(bytes as f64),
        MetricValue::Unavailable => MetricValue::Unavailable,
        MetricValue::Unsupported => MetricValue::Unsupported,
    }
}

fn present_detail_system(snapshot: &Snapshot, resource: ResourceId) -> DetailText {
    match resource {
        ResourceId::Cpu => DetailText {
            title: "CPU".to_string(),
            value: format_percent(snapshot.cpu_percent),
            kind: percent_kind(snapshot.cpu_percent),
            extra: format!(
                "{}    {}",
                logical_processor_text(snapshot.logical_processors),
                interval_text(snapshot)
            ),
            note: "全論理プロセッサの容量に対する使用率です。分母は GetSystemTimes のカーネル時間＋ユーザー時間（アイドルを含む総プロセッサ時間）、分子は同じ区間のビジー時間（カーネル−アイドル＋ユーザー）です。プロセス行の CPU は GetProcessTimes のカーネル＋ユーザー時間を同じ分母で割った値です。100% は全論理プロセッサが使用中という意味です。最初の 1 区間は差分がないため N/A です。論理プロセッサごとの割合は、その CPU 上の時間を区間の長さで割った別の値です。".to_string(),
            show_graph: !matches!(snapshot.cpu_percent, MetricValue::Unsupported),
            graph_is_memory: false,
        },
        ResourceId::Memory => DetailText {
            title: "メモリ".to_string(),
            value: format_memory_pair(snapshot.memory_used_bytes, snapshot.memory_total_bytes),
            kind: memory_kind(snapshot.memory_used_bytes, snapshot.memory_total_bytes),
            extra: memory_extra(snapshot),
            note: "システムの使用量は GlobalMemoryStatusEx の物理メモリ合計から利用可能量を引いた値です。プロセスのメモリはプライベートワーキングセット（GetProcessMemoryInfo / PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize）で、約 1 秒ごとに更新します。タスクマネージャーの「アクティブプライベートワーキングセット」とは定義が少し違うため、数値は完全には一致しないことがあります。グラフは使用率です。".to_string(),
            show_graph: !matches!(snapshot.memory_total_bytes, MetricValue::Unsupported),
            graph_is_memory: true,
        },
        ResourceId::Disk => rate_detail(
            "ディスク",
            snapshot.disk_read_bps,
            snapshot.disk_write_bps,
            &disk_extra(snapshot),
            "プロセス列の読み書きは I/O カウンタの増分です。物理ディスクの転送量とは別で、ここには足し合わせていません。",
        ),
        ResourceId::Network => network_detail(snapshot),
        ResourceId::Gpu3d => gpu_detail(snapshot, "GPU 3D", snapshot.gpu_3d_percent, GpuEngine::ThreeD),
        ResourceId::GpuEncode => {
            gpu_detail(snapshot, "GPU Encode", snapshot.gpu_encode_percent, GpuEngine::Encode)
        }
        ResourceId::GpuDecode => {
            gpu_detail(snapshot, "GPU Decode", snapshot.gpu_decode_percent, GpuEngine::Decode)
        }
        ResourceId::VramDedicated => vram_detail(
            snapshot,
            "専用 VRAM",
            snapshot.vram_dedicated_bytes,
            true,
        ),
        ResourceId::VramShared => vram_detail(
            snapshot,
            "共有 GPU メモリ",
            snapshot.vram_shared_bytes,
            false,
        ),
    }
}

enum GpuEngine {
    ThreeD,
    Encode,
    Decode,
}

fn present_row(process: &ProcessSnapshot, net: ProcessNetMode) -> PresentedRow {
    let (disk, disk_kind) = format_rate_pair(process.disk_read_bps, process.disk_write_bps);
    let (network, network_kind) = crate::format::format_network_cell(
        process.net_send_bps,
        process.net_recv_bps,
        net == ProcessNetMode::NeedsAdmin,
    );
    PresentedRow {
        name: process.name.clone(),
        pid: process.key.pid.to_string(),
        cpu: format_percent(process.cpu_percent),
        cpu_kind: percent_kind(process.cpu_percent),
        memory: format_bytes(process.memory_bytes),
        memory_kind: bytes_kind(process.memory_bytes),
        disk,
        disk_kind,
        network,
        network_kind,
        gpu3d: format_percent(process.gpu_3d_percent),
        gpu3d_kind: percent_kind(process.gpu_3d_percent),
        gpu_encode: format_percent(process.gpu_encode_percent),
        gpu_encode_kind: percent_kind(process.gpu_encode_percent),
        gpu_decode: format_percent(process.gpu_decode_percent),
        gpu_decode_kind: percent_kind(process.gpu_decode_percent),
        vram: format_bytes(process.vram_dedicated_bytes),
        vram_kind: bytes_kind(process.vram_dedicated_bytes),
        shared: format_bytes(process.vram_shared_bytes),
        shared_kind: bytes_kind(process.vram_shared_bytes),
    }
}

fn memory_kind(used: MetricValue<u64>, total: MetricValue<u64>) -> DisplayKind {
    match (used, total) {
        (MetricValue::Value(0), MetricValue::Value(_)) => DisplayKind::Zero,
        (MetricValue::Value(_), MetricValue::Value(_)) => DisplayKind::Value,
        (MetricValue::Unsupported, _) | (_, MetricValue::Unsupported) => DisplayKind::Unsupported,
        _ => DisplayKind::Unavailable,
    }
}

#[cfg(feature = "ui")]
fn card_percent(value: MetricValue<f64>) -> ResourceCard {
    ResourceCard {
        text: format_percent(value),
        kind: percent_kind(value),
    }
}

#[cfg(feature = "ui")]
fn card_pair(read: MetricValue<f64>, write: MetricValue<f64>) -> ResourceCard {
    let (text, kind) = format_rate_pair(read, write);
    ResourceCard { text, kind }
}

fn rate_detail(
    title: &str,
    read: MetricValue<f64>,
    write: MetricValue<f64>,
    extra: &str,
    note: &str,
) -> DetailText {
    let (value, kind) = format_rate_pair(read, write);
    DetailText {
        title: title.to_string(),
        value,
        kind,
        extra: extra.to_string(),
        note: note.to_string(),
        show_graph: !matches!(
            (read, write),
            (MetricValue::Unsupported, MetricValue::Unsupported)
        ),
        graph_is_memory: false,
    }
}

fn disk_extra(snapshot: &Snapshot) -> String {
    let mut lines = Vec::new();
    if snapshot.disks.is_empty() {
        lines.push(interval_text(snapshot));
    } else {
        for disk in &snapshot.disks {
            let (text, _) = format_rate_pair(disk.read_bps, disk.write_bps);
            lines.push(format!("{}  {text}", disk.name));
        }
    }
    lines.join("    ")
}

fn network_detail(snapshot: &Snapshot) -> DetailText {
    let admin = snapshot.process_net == ProcessNetMode::NeedsAdmin;
    let (value, kind) = format_rate_pair(snapshot.net_recv_bps, snapshot.net_send_bps);
    let unsupported = matches!(
        (snapshot.net_recv_bps, snapshot.net_send_bps),
        (MetricValue::Unsupported, MetricValue::Unsupported)
    ) && snapshot.adapters.is_empty();
    let mut extra = if snapshot.adapters.is_empty() {
        interval_text(snapshot)
    } else {
        snapshot
            .adapters
            .iter()
            .map(|adapter| {
                let (text, _) = format_rate_pair(adapter.read_bps, adapter.write_bps);
                format!("{}  {text}", adapter.name)
            })
            .collect::<Vec<_>>()
            .join("    ")
    };
    if admin {
        extra.push_str("    プロセス別の帰属は要管理者");
    }
    if unsupported && !admin {
        return unsupported_detail(
            "ネットワーク",
            "ネットワークの送受信速度を計測する Collector はまだ接続されていません。未対応は実測の 0 ではありません。",
        );
    }
    DetailText {
        title: "ネットワーク".to_string(),
        value: if unsupported && admin {
            "要管理者".to_string()
        } else {
            value
        },
        kind: if unsupported && admin {
            DisplayKind::Unavailable
        } else {
            kind
        },
        extra,
        note: "上の速度はアダプターの送受信です。プロセスに帰属できる速度とは別の計測値です。カーネルのネットワーク追跡は通常ユーザーでは使わないため、プロセス列は要管理者と出します。欠測は 0 にしません。".to_string(),
        show_graph: !unsupported,
        graph_is_memory: false,
    }
}

fn gpu_detail(
    snapshot: &Snapshot,
    title: &str,
    value: MetricValue<f64>,
    engine: GpuEngine,
) -> DetailText {
    if snapshot.gpu_presence == GpuPresence::None {
        return DetailText {
            title: title.to_string(),
            value: "なし".to_string(),
            kind: DisplayKind::Unsupported,
            extra: "GPU エンジンのカウンタが見つかりませんでした。".to_string(),
            note: "GPU が無い、またはドライバが GPU Engine カウンタを出していません。0% とは表示しません。".to_string(),
            show_graph: false,
            graph_is_memory: false,
        };
    }
    if snapshot.gpu_presence == GpuPresence::Unknown && matches!(value, MetricValue::Unsupported) {
        return unsupported_detail(
            title,
            "GPU 使用率を計測する Collector はまだ接続されていません。複数エンジンを足して 100% を超える値にはしません。未対応は実測の 0 ではありません。",
        );
    }
    let extra = if snapshot.gpus.is_empty() {
        interval_text(snapshot)
    } else {
        snapshot
            .gpus
            .iter()
            .map(|gpu| {
                let metric = match engine {
                    GpuEngine::ThreeD => gpu.util_3d,
                    GpuEngine::Encode => gpu.util_encode,
                    GpuEngine::Decode => gpu.util_decode,
                };
                format!("{}  {}", gpu.name, format_percent(metric))
            })
            .collect::<Vec<_>>()
            .join("    ")
    };
    DetailText {
        title: title.to_string(),
        value: format_percent(value),
        kind: percent_kind(value),
        extra,
        note: "同じ種類のエンジンが複数あるときは最大値を使います。3D と Encode と Decode は足しません。アダプターは LUID で区別します。".to_string(),
        show_graph: !matches!(value, MetricValue::Unsupported),
        graph_is_memory: false,
    }
}

fn vram_detail(
    snapshot: &Snapshot,
    title: &str,
    value: MetricValue<u64>,
    dedicated: bool,
) -> DetailText {
    if snapshot.gpu_presence == GpuPresence::None {
        return DetailText {
            title: title.to_string(),
            value: "なし".to_string(),
            kind: DisplayKind::Unsupported,
            extra: "GPU がありません。".to_string(),
            note: "専用メモリと共有メモリは別の値です。0 バイトとは表示しません。".to_string(),
            show_graph: false,
            graph_is_memory: false,
        };
    }
    if snapshot.gpu_presence == GpuPresence::Unknown && matches!(value, MetricValue::Unsupported) {
        return unsupported_detail(
            title,
            "GPU メモリを計測する Collector はまだ接続されていません。専用と共有は別の値です。未対応は実測の 0 ではありません。",
        );
    }
    let extra = if snapshot.gpus.is_empty() {
        interval_text(snapshot)
    } else {
        snapshot
            .gpus
            .iter()
            .map(|gpu| {
                let metric = if dedicated {
                    gpu.dedicated_bytes
                } else {
                    gpu.shared_bytes
                };
                format!("{}  {}", gpu.name, format_bytes(metric))
            })
            .collect::<Vec<_>>()
            .join("    ")
    };
    DetailText {
        title: title.to_string(),
        value: format_bytes(value),
        kind: bytes_kind(value),
        extra,
        note: "GPU Process Memory カウンタの専用使用量と共有使用量です。ドライバによって 0 や遅れた値になることがあります。プロセス間で共有分を足すと二重に数えることがあるため、アダプターごとの値を併記します。".to_string(),
        show_graph: !matches!(value, MetricValue::Unsupported),
        graph_is_memory: false,
    }
}

pub struct TopEntry {
    pub name: String,
    pub value: String,
    pub kind: DisplayKind,
}

/// Highest current values for the selected resource. Computed from the full list.
/// Rows without a real reading are omitted. History still stores every process.
pub fn top_entries(snapshot: &Snapshot, resource: ResourceId, limit: usize) -> Vec<TopEntry> {
    if limit == 0 {
        return Vec::new();
    }
    let column = match resource {
        ResourceId::Cpu => SortColumn::Cpu,
        ResourceId::Memory => SortColumn::Memory,
        ResourceId::Disk => SortColumn::Disk,
        ResourceId::Network => SortColumn::Network,
        ResourceId::Gpu3d => SortColumn::Gpu3d,
        ResourceId::GpuEncode => SortColumn::GpuEncode,
        ResourceId::GpuDecode => SortColumn::GpuDecode,
        ResourceId::VramDedicated => SortColumn::VramDedicated,
        ResourceId::VramShared => SortColumn::VramShared,
    };
    let mut processes = snapshot.processes.clone();
    sort_processes(&mut processes, column, true);
    processes
        .into_iter()
        .filter_map(|process| {
            let row = present_row(&process, snapshot.process_net);
            let (text, kind) = match resource {
                ResourceId::Cpu => (row.cpu, row.cpu_kind),
                ResourceId::Memory => (row.memory, row.memory_kind),
                ResourceId::Disk => (row.disk, row.disk_kind),
                ResourceId::Network => (row.network, row.network_kind),
                ResourceId::Gpu3d => (row.gpu3d, row.gpu3d_kind),
                ResourceId::GpuEncode => (row.gpu_encode, row.gpu_encode_kind),
                ResourceId::GpuDecode => (row.gpu_decode, row.gpu_decode_kind),
                ResourceId::VramDedicated => (row.vram, row.vram_kind),
                ResourceId::VramShared => (row.shared, row.shared_kind),
            };
            if !matches!(kind, DisplayKind::Value | DisplayKind::Zero) {
                return None;
            }
            Some(TopEntry {
                name: row.name,
                value: text,
                kind,
            })
        })
        .take(limit)
        .collect()
}

fn unsupported_detail(title: &str, note: &str) -> DetailText {
    DetailText {
        title: title.to_string(),
        value: "未対応".to_string(),
        kind: DisplayKind::Unsupported,
        extra: "Collector は未接続です".to_string(),
        note: note.to_string(),
        show_graph: false,
        graph_is_memory: false,
    }
}

fn logical_processor_text(value: MetricValue<u32>) -> String {
    match value {
        MetricValue::Value(count) => format!("論理プロセッサ {count} 個"),
        MetricValue::Unavailable => "論理プロセッサ数: N/A".to_string(),
        MetricValue::Unsupported => "論理プロセッサ数: 未対応".to_string(),
    }
}

fn interval_text(snapshot: &Snapshot) -> String {
    match snapshot.interval {
        Some(interval) => format!("計測区間 {} ms", interval.as_millis()),
        None => "計測区間はまだ確定していません".to_string(),
    }
}

fn memory_extra(snapshot: &Snapshot) -> String {
    let percent = match (snapshot.memory_used_bytes, snapshot.memory_total_bytes) {
        (MetricValue::Value(used), MetricValue::Value(total)) if total > 0 => {
            format!("使用率 {:.1}%", (used as f64) / (total as f64) * 100.0)
        }
        (MetricValue::Unsupported, _) | (_, MetricValue::Unsupported) => {
            "使用率: 未対応".to_string()
        }
        _ => "使用率: N/A".to_string(),
    };
    format!("{percent}    {}", interval_text(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProcessKey;
    use std::time::SystemTime;

    fn snapshot() -> Snapshot {
        Snapshot {
            sequence: 1,
            monotonic: std::time::Instant::now(),
            taken_at: SystemTime::UNIX_EPOCH,
            interval: Some(std::time::Duration::from_millis(500)),
            logical_processors: MetricValue::Value(8),
            cpu_percent: MetricValue::Value(0.0),
            memory_used_bytes: MetricValue::Value(0),
            memory_total_bytes: MetricValue::Value(1024 * 1024),
            disk_read_bps: MetricValue::Unsupported,
            disk_write_bps: MetricValue::Unsupported,
            net_send_bps: MetricValue::Unsupported,
            net_recv_bps: MetricValue::Unsupported,
            gpu_3d_percent: MetricValue::Unsupported,
            gpu_encode_percent: MetricValue::Unsupported,
            gpu_decode_percent: MetricValue::Unsupported,
            vram_dedicated_bytes: MetricValue::Unsupported,
            vram_shared_bytes: MetricValue::Unsupported,
            disks: Vec::new(),
            adapters: Vec::new(),
            gpus: Vec::new(),
            process_net: crate::model::ProcessNetMode::Attributed,
            gpu_presence: crate::model::GpuPresence::Unknown,
            processes: vec![ProcessSnapshot {
                key: ProcessKey {
                    pid: 4,
                    creation_time_100ns: 1,
                },
                parent_pid: 0,
                name: "System".to_string(),
                cpu_percent: MetricValue::Value(0.0),
                memory_bytes: MetricValue::Value(0),
                disk_read_bps: MetricValue::Unsupported,
                disk_write_bps: MetricValue::Unsupported,
                net_send_bps: MetricValue::Unsupported,
                net_recv_bps: MetricValue::Unsupported,
                gpu_3d_percent: MetricValue::Unsupported,
                gpu_encode_percent: MetricValue::Unsupported,
                gpu_decode_percent: MetricValue::Unsupported,
                vram_dedicated_bytes: MetricValue::Unsupported,
                vram_shared_bytes: MetricValue::Unsupported,
                gpus: Vec::new(),
            }],
            warning: None,
            cpu_detail: crate::model::CpuDetailSnapshot::off(),
        }
    }

    #[test]
    fn zero_measurements_stay_distinct_from_unsupported_columns() {
        let rows = present_rows(&snapshot(), SortColumn::Name, false);
        assert_eq!(rows[0].cpu, "0.0%");
        assert_eq!(rows[0].cpu_kind, DisplayKind::Zero);
        assert_eq!(rows[0].memory, "0 B");
        assert_eq!(rows[0].disk, "未対応");
        assert_eq!(rows[0].disk_kind, DisplayKind::Unsupported);
        assert_eq!(rows[0].gpu3d, "未対応");
        let detail = present_detail(&snapshot(), ResourceId::Disk);
        assert_eq!(detail.value, "未対応");
        assert!(!detail.show_graph);
        let cpu = present_detail(&snapshot(), ResourceId::Cpu);
        assert_eq!(cpu.value, "0.0%");
        assert!(cpu.show_graph);
    }
}
