//! Per-process I/O counters and physical-disk throughput.
//!
//! `GetProcessIoCounters` includes file, network, and device I/O. Its delta is
//! not physical disk transfer. The performance card uses PDH
//! `\PhysicalDisk(*)\Disk Bytes/sec`, which is a different measurement.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    GetProcessIoCounters, OpenProcess, IO_COUNTERS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::collector::MetricEnricher;
use crate::devices::publish_disk_sides;
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
        // One side failing must not blank the side that still has a reading.
        let (devices, read_bps, write_bps) = publish_disk_sides(read.as_deref(), write.as_deref());
        if read.is_some() && write.is_some() && devices.is_empty() && stale {
            sample.note("ディスクの PDH インスタンスがありません");
        }
        sample.system_disk_read_bps = read_bps;
        sample.system_disk_write_bps = write_bps;
        sample.disks = devices;
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
            None => (query.status(), query.should_reopen()),
        }
    };
    let (status_on_failure, reopen) = status_on_failure;
    sample.note(&format!("ディスク PDH({status_on_failure:#X})"));
    // A transient miss keeps the primed query. Reopen only after repeated
    // hard failures, and drop that one side alone.
    if reopen {
        super::pdh::note_reopen();
        *slot = None;
    }
    None
}

fn process_io(pid: u32) -> Option<(u64, u64)> {
    if pid == 0 {
        return None;
    }
    // SAFETY: the access mask asks only for query rights. `IoHandle` closes it
    // on every return, including when `GetProcessIoCounters` fails. Leaving the
    // handle open leaked one kernel object per process per sample and, after
    // long enough, made later `OpenProcess`, PDH, and `GetIfTable2` calls fail.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    if handle.is_invalid() {
        return None;
    }
    let handle = IoHandle(handle);
    let mut counters = IO_COUNTERS {
        ReadOperationCount: 0,
        WriteOperationCount: 0,
        OtherOperationCount: 0,
        ReadTransferCount: 0,
        WriteTransferCount: 0,
        OtherTransferCount: 0,
    };
    let result = unsafe { GetProcessIoCounters(handle.0, &mut counters) };
    result.ok()?;
    Some((counters.ReadTransferCount, counters.WriteTransferCount))
}

struct IoHandle(HANDLE);

impl Drop for IoHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    fn handle_count() -> u32 {
        let mut count = 0u32;
        unsafe {
            let _ = GetProcessHandleCount(GetCurrentProcess(), &mut count);
        }
        count
    }

    #[test]
    fn repeated_process_io_does_not_grow_the_handle_table() {
        let before = handle_count();
        for _ in 0..400 {
            let _ = process_io(std::process::id());
            let _ = process_io(0);
            let _ = process_io(u32::MAX);
        }
        let after = handle_count();
        assert!(
            after <= before.saturating_add(8),
            "process_io leaked handles: {before} -> {after}"
        );
    }
}
