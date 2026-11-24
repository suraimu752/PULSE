//! Adapter throughput for a normal user, and no per-process attribution.
//!
//! `GetIfTable2` does not need elevation. Kernel ETW that would assign packets
//! to a PID usually does, so process cells stay unavailable and the UI says
//! 要管理者. Adapter bytes are not copied into those cells.

use std::collections::HashMap;
use std::time::Instant;

use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, MIB_IF_TYPE_LOOPBACK,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

use crate::collector::MetricEnricher;
use crate::devices::sum_side;
use crate::metric::MetricValue;
use crate::model::{NamedRate, ProcessNetMode, RawSample};

pub struct NetworkEnricher {
    previous: HashMap<u32, (u64, u64)>,
    previous_at: Option<Instant>,
}

impl NetworkEnricher {
    pub fn new() -> Self {
        Self {
            previous: HashMap::new(),
            previous_at: None,
        }
    }
}

impl MetricEnricher for NetworkEnricher {
    fn id(&self) -> &'static str {
        "network"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        sample.process_net = ProcessNetMode::NeedsAdmin;
        for process in &mut sample.processes {
            process.net_send_bytes = MetricValue::Unavailable;
            process.net_recv_bytes = MetricValue::Unavailable;
        }
        let rows = match read_adapters() {
            Ok(rows) => rows,
            Err(code) => {
                sample.note(&format!("ネットワーク GetIfTable2: WIN32_ERROR({code})"));
                sample.system_net_recv_bps = MetricValue::Unavailable;
                sample.system_net_send_bps = MetricValue::Unavailable;
                sample.adapters.clear();
                // Keep the last octet counters. The next successful read can
                // still form a rate instead of staying N/A for an extra interval.
                return;
            }
        };
        if rows.is_empty() {
            sample.note("ネットワークのアダプタが 0 件です");
        }
        let elapsed = self
            .previous_at
            .map(|started| sample.taken_at.saturating_duration_since(started));
        let secs = elapsed
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0);
        let mut adapters = Vec::new();
        let mut next = HashMap::new();
        for row in rows {
            next.insert(row.index, (row.recv, row.send));
            let (recv, send) = match (self.previous.get(&row.index), secs > f64::EPSILON) {
                (Some((recv, send)), true) if row.recv >= *recv && row.send >= *send => {
                    let recv_bps = (row.recv - *recv) as f64 / secs;
                    let send_bps = (row.send - *send) as f64 / secs;
                    (finite(recv_bps), finite(send_bps))
                }
                _ => (MetricValue::Unavailable, MetricValue::Unavailable),
            };
            adapters.push(NamedRate {
                name: row.name,
                read_bps: recv,
                write_bps: send,
            });
        }
        sample.system_net_recv_bps = sum_side(&adapters, true);
        sample.system_net_send_bps = sum_side(&adapters, false);
        sample.adapters = adapters;
        self.previous = next;
        self.previous_at = Some(sample.taken_at);
    }
}

struct AdapterRow {
    index: u32,
    name: String,
    recv: u64,
    send: u64,
}

fn finite(value: f64) -> MetricValue<f64> {
    if value.is_finite() {
        MetricValue::Value(value)
    } else {
        MetricValue::Unavailable
    }
}

fn read_adapters() -> Result<Vec<AdapterRow>, u32> {
    unsafe {
        let mut table = std::ptr::null_mut();
        let status = GetIfTable2(&mut table);
        if status.0 != 0 {
            if !table.is_null() {
                FreeMibTable(table.cast());
            }
            return Err(status.0);
        }
        if table.is_null() {
            return Err(status.0);
        }
        let count = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        let mut up = Vec::new();
        let mut rest = Vec::new();
        for row in rows {
            if row.Type == MIB_IF_TYPE_LOOPBACK {
                continue;
            }
            let adapter = AdapterRow {
                index: row.InterfaceIndex,
                name: alias(&row.Alias),
                recv: row.InOctets,
                send: row.OutOctets,
            };
            if row.OperStatus == IfOperStatusUp {
                up.push(adapter);
            } else {
                rest.push(adapter);
            }
        }
        FreeMibTable(table.cast());
        // An OperStatus the headers don't name used to drop every adapter and
        // the card stayed N/A. Keep the non-loopback rows in that case.
        if up.is_empty() {
            Ok(rest)
        } else {
            Ok(up)
        }
    }
}

fn alias(wide: &[u16]) -> String {
    let end = wide
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(wide.len());
    let text = String::from_utf16_lossy(&wide[..end]);
    if text.is_empty() {
        "adapter".to_string()
    } else {
        text
    }
}
