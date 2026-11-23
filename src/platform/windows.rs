//! Windows 11 process, CPU, and memory probe.
//!
//! CPU uses `GetProcessTimes` / `GetSystemTimes`. Memory uses
//! `PROCESS_MEMORY_COUNTERS_EX2.PrivateWorkingSetSize`. Disk, network, and GPU
//! stay [`MetricValue::Unsupported`] until a later enricher fills them.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::SystemInformation::{
    GetLocalTime, GetSystemInfo, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetCurrentProcess, GetProcessTimes, GetSystemTimes, OpenProcess,
    ALL_PROCESSOR_GROUPS, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::collector::SystemProbe;
use crate::config::MEMORY_SAMPLE_INTERVAL;
use crate::metric::MetricValue;
use crate::model::{ensure_self_row, ProcessKey, RawProcess, RawSample};

const MAX_PROCESSES: usize = 100_000;

/// Read `AppsUseLightTheme`. A missing key or a failed call returns `None`.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub fn apps_use_light_theme() -> Option<bool> {
    unsafe {
        let mut data: u32 = 0;
        let mut size = u32::try_from(size_of::<u32>()).unwrap_or(4);
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            windows::core::w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut data as *mut u32).cast()),
            Some(&mut size),
        );
        if status.0 == 0 && size >= 4 {
            Some(crate::theme::light_from_dword(data))
        } else {
            None
        }
    }
}

pub struct WindowsSystemProbe {
    last_memory_refresh: Option<Instant>,
    memory_cache: HashMap<ProcessKey, MetricValue<u64>>,
    /// Set when Windows returns a zero private working set for this process while
    /// its ordinary working set is large. That pattern means the EX2 field was
    /// not filled, so showing zero would be wrong.
    private_working_set_unavailable: bool,
}

impl WindowsSystemProbe {
    pub fn new() -> Self {
        Self {
            last_memory_refresh: None,
            memory_cache: HashMap::new(),
            private_working_set_unavailable: false,
        }
    }

    fn read_processes(&mut self) -> Result<(Vec<RawProcess>, Option<String>), String> {
        let refresh_memory = match self.last_memory_refresh {
            Some(at) => at.elapsed() >= MEMORY_SAMPLE_INTERVAL,
            None => true,
        };
        // SAFETY: a process snapshot is a kernel object closed by `OwnedHandle`.
        let snapshot = unsafe {
            CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
                .map_err(|err| format!("プロセス一覧を取得できませんでした（{err}）"))?
        };
        let snapshot = OwnedHandle(snapshot);
        let entry_size = u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0);
        let mut entry = PROCESSENTRY32W {
            dwSize: entry_size,
            ..PROCESSENTRY32W::default()
        };
        // SAFETY: `entry.dwSize` is set and `snapshot` is an open toolhelp snapshot.
        let first = unsafe { Process32FirstW(snapshot.raw(), &mut entry) };
        if first.is_err() {
            return Err("プロセス一覧の先頭を読めませんでした。".to_string());
        }

        let mut processes = Vec::new();
        let mut live = HashSet::new();
        let mut denied = 0u32;
        loop {
            if processes.len() >= MAX_PROCESSES {
                break;
            }
            let pid = entry.th32ProcessID;
            let parent_pid = entry.th32ParentProcessID;
            let name = display_name(pid, wide_to_string(&entry.szExeFile));
            let mut process = RawProcess::new(pid, parent_pid, name);
            match open_process(pid) {
                Some(handle) => {
                    if let Some((creation, cpu)) = process_cpu(&handle) {
                        process.creation_time_100ns = Some(creation);
                        process.cpu_time_100ns = MetricValue::Value(cpu);
                    }
                    let key = process.key();
                    let memory = self.memory_for(&handle, pid, key, refresh_memory);
                    process.memory_bytes = memory;
                }
                None => {
                    denied = denied.saturating_add(1);
                    let key = process.key();
                    process.memory_bytes = self
                        .memory_cache
                        .get(&key)
                        .copied()
                        .unwrap_or(MetricValue::Unavailable);
                }
            }
            live.insert(process.key());
            processes.push(process);

            entry.dwSize = entry_size;
            // SAFETY: same snapshot and `dwSize` as the first call.
            let next = unsafe { Process32NextW(snapshot.raw(), &mut entry) };
            if next.is_err() {
                break;
            }
        }

        self.stamp_self(&mut processes);
        if refresh_memory {
            self.last_memory_refresh = Some(Instant::now());
        }
        if self.private_working_set_unavailable {
            // The EX2 field was not populated. Drop any zeros already copied
            // into this sample so they are not shown as real measurements.
            for process in &mut processes {
                process.memory_bytes = MetricValue::Unavailable;
            }
            self.memory_cache.clear();
        } else {
            self.memory_cache.retain(|key, _| live.contains(key));
        }

        let warning = if self.private_working_set_unavailable {
            Some(
                "プライベートワーキングセットを取得できないため、プロセスメモリは N/A です。Windows 11 の更新状態を確認してください。"
                    .to_string(),
            )
        } else if denied > 0 {
            Some(format!(
                "{denied} 件のプロセスは権限不足のため CPU またはメモリを計測できませんでした"
            ))
        } else {
            None
        };
        Ok((processes, warning))
    }

    fn memory_for(
        &mut self,
        handle: &OwnedHandle,
        pid: u32,
        key: ProcessKey,
        refresh: bool,
    ) -> MetricValue<u64> {
        if self.private_working_set_unavailable {
            return MetricValue::Unavailable;
        }
        if !refresh {
            if let Some(cached) = self.memory_cache.get(&key).copied() {
                return cached;
            }
        }
        let reading =
            read_private_working_set(handle.raw(), pid, &mut self.private_working_set_unavailable);
        self.memory_cache.insert(key, reading);
        reading
    }

    /// Toolhelp can omit this process, and `OpenProcess` on our own PID can
    /// fail even when the pseudo-handle still works. Diagnostics read this row.
    fn stamp_self(&mut self, processes: &mut Vec<RawProcess>) {
        let pid = std::process::id();
        // SAFETY: the pseudo-handle is not closed. GetProcessTimes and
        // GetProcessMemoryInfo accept it.
        let handle = unsafe { GetCurrentProcess() };
        let (creation, cpu) = match process_times(handle) {
            Some((creation, cpu)) => (Some(creation), MetricValue::Value(cpu)),
            None => (None, MetricValue::Unavailable),
        };
        let memory =
            read_private_working_set(handle, pid, &mut self.private_working_set_unavailable);
        ensure_self_row(processes, pid, &self_image_name(), creation, cpu, memory);
    }
}

impl SystemProbe for WindowsSystemProbe {
    fn probe(&mut self) -> RawSample {
        let mut sample = RawSample::empty(Instant::now(), std::time::SystemTime::now());
        let (busy, total) = read_system_cpu();
        sample.system_busy_100ns = busy;
        sample.system_total_100ns = total;
        sample.logical_processors = read_logical_processors();
        let (total_bytes, available_bytes) = read_physical_memory();
        sample.memory_total_bytes = total_bytes;
        sample.memory_available_bytes = available_bytes;
        match self.read_processes() {
            Ok((processes, warning)) => {
                sample.processes = processes;
                sample.warning = warning;
            }
            Err(message) => {
                let mut processes = Vec::new();
                self.stamp_self(&mut processes);
                sample.processes = processes;
                sample.warning = Some(message);
            }
        }
        sample
    }
}

impl Default for WindowsSystemProbe {
    fn default() -> Self {
        Self::new()
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle came from OpenProcess or CreateToolhelp32Snapshot
            // and this is the only close.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

fn open_process(pid: u32) -> Option<OwnedHandle> {
    // SAFETY: the access mask asks only for query rights. The handle is owned.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    if handle.is_invalid() {
        return None;
    }
    Some(OwnedHandle(handle))
}

fn process_cpu(handle: &OwnedHandle) -> Option<(u64, u64)> {
    process_times(handle.raw())
}

fn process_times(handle: HANDLE) -> Option<(u64, u64)> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the pointers are valid outs. `handle` is either an owned process
    // handle or the `GetCurrentProcess` pseudo-handle, which is not closed here.
    let status =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if status.is_err() {
        return None;
    }
    let cpu = filetime_to_u64(kernel).saturating_add(filetime_to_u64(user));
    Some((filetime_to_u64(creation), cpu))
}

fn read_private_working_set(handle: HANDLE, pid: u32, broken: &mut bool) -> MetricValue<u64> {
    if *broken {
        return MetricValue::Unavailable;
    }
    let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX2>()).unwrap_or(0),
        ..PROCESS_MEMORY_COUNTERS_EX2::default()
    };
    // SAFETY: `cb` tells GetProcessMemoryInfo the real struct size. The API is
    // typed as the smaller PROCESS_MEMORY_COUNTERS, which is the documented call.
    let status = unsafe {
        GetProcessMemoryInfo(
            handle,
            std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        )
    };
    if status.is_err() {
        return MetricValue::Unavailable;
    }
    if pid == std::process::id()
        && counters.PrivateWorkingSetSize == 0
        && counters.WorkingSetSize > 1024 * 1024
    {
        *broken = true;
        return MetricValue::Unavailable;
    }
    u64::try_from(counters.PrivateWorkingSetSize)
        .map(MetricValue::Value)
        .unwrap_or(MetricValue::Unavailable)
}

fn read_system_cpu() -> (MetricValue<u64>, MetricValue<u64>) {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the three pointers are valid FILETIME outputs.
    let status = unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) };
    if status.is_err() {
        return (MetricValue::Unavailable, MetricValue::Unavailable);
    }
    let idle = filetime_to_u64(idle);
    let kernel = filetime_to_u64(kernel);
    let user = filetime_to_u64(user);
    let total = kernel.saturating_add(user);
    let busy = kernel.saturating_sub(idle).saturating_add(user);
    (MetricValue::Value(busy), MetricValue::Value(total))
}

fn read_logical_processors() -> MetricValue<u32> {
    // SAFETY: ALL_PROCESSOR_GROUPS is the documented group number for a machine-wide count.
    let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    if count > 0 {
        return MetricValue::Value(count);
    }
    let mut info = SYSTEM_INFO::default();
    // SAFETY: `info` is a valid SYSTEM_INFO out-parameter. This fallback only
    // sees the current processor group (64 logical processors at most).
    unsafe { GetSystemInfo(&mut info) };
    if info.dwNumberOfProcessors > 0 {
        MetricValue::Value(info.dwNumberOfProcessors)
    } else {
        MetricValue::Unavailable
    }
}

fn read_physical_memory() -> (MetricValue<u64>, MetricValue<u64>) {
    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).unwrap_or(0),
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: `dwLength` is set to the struct size, as required by the API.
    let result = unsafe { GlobalMemoryStatusEx(&mut status) };
    if result.is_err() || status.ullTotalPhys == 0 {
        return (MetricValue::Unavailable, MetricValue::Unavailable);
    }
    let available = status.ullAvailPhys.min(status.ullTotalPhys);
    (
        MetricValue::Value(status.ullTotalPhys),
        MetricValue::Value(available),
    )
}

pub fn local_time_hms() -> String {
    // SAFETY: GetLocalTime only writes a SYSTEMTIME that the wrapper returns by value.
    let time = unsafe { GetLocalTime() };
    format!("{:02}:{:02}:{:02}", time.wHour, time.wMinute, time.wSecond)
}

fn filetime_to_u64(value: FILETIME) -> u64 {
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

fn self_image_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "PULSE.exe".to_string())
}

fn display_name(pid: u32, image: String) -> String {
    // Toolhelp names PID 0 "[System Process]". Task Manager's label for that
    // same process is the conventional idle-process name. This is a rename of
    // a real process, not a fabricated row.
    if pid == 0 {
        return "System Idle Process".to_string();
    }
    if image.is_empty() {
        format!("PID {pid}")
    } else {
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn reads_this_process_and_keeps_disk_unsupported() {
        let mut probe = WindowsSystemProbe::new();
        let first = probe.probe();
        let pid = std::process::id();
        let mine = first
            .processes
            .iter()
            .find(|process| process.pid == pid)
            .expect("the current process is in the toolhelp snapshot");
        match mine.memory_bytes {
            MetricValue::Value(bytes) => {
                assert!(bytes > 0, "private working set should be non-zero")
            }
            other => panic!("expected a memory reading, got {other:?}"),
        }
        assert!(matches!(mine.cpu_time_100ns, MetricValue::Value(time) if time > 0));
        assert_eq!(mine.disk_read_bytes, MetricValue::Unsupported);
        assert_eq!(mine.net_send_bytes, MetricValue::Unsupported);
        assert_eq!(mine.gpu_3d_percent, MetricValue::Unsupported);
        assert!(matches!(first.memory_total_bytes, MetricValue::Value(total) if total > 0));
        assert!(matches!(first.logical_processors, MetricValue::Value(count) if count >= 1));
        assert!(matches!(first.system_total_100ns, MetricValue::Value(total) if total > 0));

        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(40) {
            std::hint::spin_loop();
        }
        let second = probe.probe();
        let snapshot = crate::aggregate::aggregate(Some(&first), &second);
        assert!(
            matches!(snapshot.cpu_percent, MetricValue::Value(_)),
            "system CPU should be computable after two real samples"
        );
        let mine = snapshot
            .processes
            .iter()
            .find(|process| process.key.pid == pid)
            .expect("current process still listed");
        assert!(
            matches!(mine.cpu_percent, MetricValue::Value(percent) if percent > 0.0),
            "a short busy loop should show a non-zero CPU percent, got {:?}",
            mine.cpu_percent
        );
        assert_eq!(mine.disk_read_bps, MetricValue::Unsupported);
    }
}
