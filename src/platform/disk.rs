//! Per-process I/O counters and physical-disk throughput.
//!
//! `GetProcessIoCounters` includes file, network, and device I/O. Its delta is
//! not physical disk transfer. The performance card uses PDH
//! `\PhysicalDisk(*)\Disk Bytes/sec`, which is a different measurement.

use std::time::{Duration, Instant};

use windows::Win32::System::Threading::{
    GetProcessIoCounters, OpenProcess, IO_COUNTERS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::collector::MetricEnricher;
use crate::devices::{sum_side, without_rollup, zip_device_rates};
use crate::metric::MetricValue;
use crate::model::RawSample;

use super::pdh::{PdhQuery, PdhSample};

pub struct DiskEnricher {
    read: Option<PdhQuery>,
    write: Option<PdhQuery>,
    read_error: u32,
    write_error: u32,
    since: Option<Instant>,
}

impl DiskEnricher {
    pub fn new() -> Self {
        Self {
            read: None,
            write: None,
            read_error: 0,
            write_error: 0,
            since: None,
        }
    }

    fn ensure_open(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
        if self.read.is_none() {
            match open_any(&[
                r"\PhysicalDisk(*)\Disk Read Bytes/sec",
                r"\PhysicalDisk(_Total)\Disk Read Bytes/sec",
            ]) {
                Ok(query) => self.read = Some(query),
                Err(code) => self.read_error = code,
            }
        }
        if self.write.is_none() {
            match open_any(&[
                r"\PhysicalDisk(*)\Disk Write Bytes/sec",
                r"\PhysicalDisk(_Total)\Disk Write Bytes/sec",
            ]) {
                Ok(query) => self.write = Some(query),
                Err(code) => self.write_error = code,
            }
        }
    }
}

fn open_any(paths: &[&str]) -> Result<PdhQuery, u32> {
    let mut last = 0u32;
    for path in paths {
        match PdhQuery::open(path) {
            Ok(query) => return Ok(query),
            Err(code) => last = code,
        }
    }
    Err(last)
}

impl MetricEnricher for DiskEnricher {
    fn id(&self) -> &'static str {
        "disk"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        self.ensure_open();
        for process in &mut sample.processes {
            match process_io(process.pid) {
                Some((read, write)) => {
                    process.disk_read_bytes = MetricValue::Value(read);
                    process.disk_write_bytes = MetricValue::Value(write);
                }
                None => {
                    process.disk_read_bytes = MetricValue::Unavailable;
                    process.disk_write_bytes = MetricValue::Unavailable;
                }
            }
        }
        if self.read.is_none() {
            sample.note(&format!("ディスク読取 PDH({:#X})", self.read_error));
        }
        if self.write.is_none() {
            sample.note(&format!("ディスク書込 PDH({:#X})", self.write_error));
        }
        let stale = self
            .since
            .is_some_and(|opened| opened.elapsed() >= Duration::from_secs(2));
        let read = take(&mut self.read, stale, sample);
        let write = take(&mut self.write, stale, sample);
        match (read, write) {
            (Some(read), Some(write)) => {
                if read.is_empty() && write.is_empty() && stale {
                    sample.note("ディスクの PDH インスタンスがありません");
                }
                let devices = without_rollup(zip_device_rates(&read, &write));
                sample.system_disk_read_bps = sum_side(&devices, true);
                sample.system_disk_write_bps = sum_side(&devices, false);
                sample.disks = devices;
            }
            _ => {
                sample.system_disk_read_bps = MetricValue::Unavailable;
                sample.system_disk_write_bps = MetricValue::Unavailable;
                sample.disks.clear();
            }
        }
    }
}

fn take(
    slot: &mut Option<PdhQuery>,
    stale: bool,
    sample: &mut RawSample,
) -> Option<Vec<(String, f64)>> {
    let status_on_failure = {
        let query = slot.as_ref()?;
        match query.collect() {
            Some(PdhSample::Ready(values)) => return Some(values),
            Some(PdhSample::NoData) => {
                if stale {
                    sample.note("ディスク PDH にデータが来ません");
                }
                return None;
            }
            None => query.status(),
        }
    };
    sample.note(&format!("ディスク PDH({status_on_failure:#X})"));
    *slot = None;
    None
}

fn process_io(pid: u32) -> Option<(u64, u64)> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut counters = IO_COUNTERS {
        ReadOperationCount: 0,
        WriteOperationCount: 0,
        OtherOperationCount: 0,
        ReadTransferCount: 0,
        WriteTransferCount: 0,
        OtherTransferCount: 0,
    };
    let result = unsafe { GetProcessIoCounters(handle, &mut counters) };
    result.ok()?;
    Some((counters.ReadTransferCount, counters.WriteTransferCount))
}
