//! Per-process I/O counters and physical-disk throughput.
//!
//! `GetProcessIoCounters` includes file, network, and device I/O. Its delta is
//! not physical disk transfer. The performance card uses PDH
//! `\PhysicalDisk(*)\Disk Bytes/sec`, which is a different measurement.

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
    opened: bool,
}

impl DiskEnricher {
    pub fn new() -> Self {
        Self {
            read: None,
            write: None,
            opened: false,
        }
    }

    fn open_once(&mut self) {
        if self.opened {
            return;
        }
        self.read = PdhQuery::open(r"\PhysicalDisk(*)\Disk Read Bytes/sec");
        self.write = PdhQuery::open(r"\PhysicalDisk(*)\Disk Write Bytes/sec");
        self.opened = true;
    }
}

impl MetricEnricher for DiskEnricher {
    fn id(&self) -> &'static str {
        "disk"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        self.open_once();
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
        match (collect(&self.read), collect(&self.write)) {
            (Some(read), Some(write)) => {
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

fn collect(query: &Option<PdhQuery>) -> Option<Vec<(String, f64)>> {
    let query = query.as_ref()?;
    match query.collect() {
        Some(PdhSample::Ready(values)) => Some(values),
        _ => None,
    }
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
