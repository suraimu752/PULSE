//! Private ETW session for per-logical-processor CPU time.
//!
//! The session name is `PULSE-CPU`. This module never stops a session with
//! any other name. Context switches are the source of CPU time. Sampling is
//! not used: a percent has to be time on that CPU divided by the interval.

#[cfg(feature = "ui")]
use std::os::windows::ffi::OsStrExt;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_SUCCESS,
    ERROR_WMI_INSTANCE_NOT_FOUND,
};
use windows::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, OpenTraceW, PerfInfoGuid, ProcessGuid, ProcessTrace, StartTraceW,
    SystemTraceControlGuid, ThreadGuid, CONTROLTRACE_HANDLE, EVENT_RECORD,
    EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG_CSWITCH,
    EVENT_TRACE_FLAG_DPC, EVENT_TRACE_FLAG_INTERRUPT, EVENT_TRACE_FLAG_PROCESS,
    EVENT_TRACE_FLAG_THREAD, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES,
    EVENT_TRACE_REAL_TIME_MODE, EVENT_TRACE_SYSTEM_LOGGER_MODE, PROCESSTRACE_HANDLE,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Performance::QueryPerformanceCounter;
use windows::Win32::System::Threading::{
    GetActiveProcessorCount, GetActiveProcessorGroupCount, ALL_PROCESSOR_GROUPS,
};

use crate::collector::MetricEnricher;
use crate::cpudetail::{parse_switch_tids, CpuEvent, SwitchLedger};
use crate::model::{CpuDetailSnapshot, LogicalCpuId, RawProcess, RawSample};

const SESSION: &str = "PULSE-CPU";
const CSWITCH: u8 = 36;
const START: u8 = 1;
const END: u8 = 2;
const DC_START: u8 = 3;
const DC_END: u8 = 4;
const DPC: u8 = 0x14;
const ISR: u8 = 0x15;

struct Inner {
    ledger: Mutex<SwitchLedger>,
}

pub struct EtwEnricher {
    enabled: Arc<std::sync::atomic::AtomicBool>,
    session: Option<Session>,
    denied: bool,
}

struct Session {
    control: CONTROLTRACE_HANDLE,
    opened: PROCESSTRACE_HANDLE,
    /// Kept alive because OpenTrace stores a pointer into this buffer.
    name: Vec<u16>,
    inner: Box<Inner>,
    join: Option<JoinHandle<()>>,
    lost_seen: u32,
}

impl EtwEnricher {
    pub fn new(enabled: Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self {
            enabled,
            session: None,
            denied: false,
        }
    }

    fn ensure(&mut self) -> Result<(), SessionError> {
        if self.session.is_some() {
            return Ok(());
        }
        if self.denied {
            return Err(SessionError::Denied);
        }
        let mut inner = Box::new(Inner {
            ledger: Mutex::new(SwitchLedger::new()),
        });
        {
            let mut ledger = inner.ledger.lock().unwrap_or_else(|err| err.into_inner());
            ledger.set_enabled(true);
            ledger.expect(logical_cpus());
            for (tid, pid) in snapshot_threads() {
                ledger.seed_thread(tid, pid, 0, "");
            }
        }
        stop_stale_session();
        let control = start_session()?;
        let mut name = session_wide();
        let opened = open_consumer(inner.as_mut(), &mut name).inspect_err(|_| {
            stop_handle(control);
        })?;
        let opened_for_thread = opened;
        let join = thread::Builder::new()
            .name("pulse-etw".to_string())
            .spawn(move || {
                // SAFETY: `opened_for_thread` stays open until this thread returns.
                let status = unsafe { ProcessTrace(&[opened_for_thread], None, None) };
                let _ = status;
            })
            .map_err(|err| {
                let _ = unsafe { CloseTrace(opened) };
                stop_handle(control);
                SessionError::Message(format!(
                    "ETW の読み取りスレッドを開始できませんでした: {err}"
                ))
            })?;
        self.session = Some(Session {
            control,
            opened,
            name,
            inner,
            join: Some(join),
            lost_seen: 0,
        });
        Ok(())
    }

    fn shutdown(&mut self) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        stop_handle(session.control);
        let _ = unsafe { CloseTrace(session.opened) };
        if let Some(join) = session.join.take() {
            let _ = join.join();
        }
        let _ = session.name.first();
        {
            let mut ledger = session
                .inner
                .ledger
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            ledger.set_enabled(false);
        }
    }
}

impl Drop for EtwEnricher {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl MetricEnricher for EtwEnricher {
    fn id(&self) -> &'static str {
        "cpu-etw"
    }

    fn enrich(&mut self, sample: &mut RawSample) {
        let wanted = self.enabled.load(std::sync::atomic::Ordering::SeqCst);
        if !wanted {
            self.denied = false;
            self.shutdown();
            sample.cpu_detail = CpuDetailSnapshot::off();
            return;
        }
        if let Err(err) = self.ensure() {
            sample.cpu_detail = match err {
                SessionError::Denied => CpuDetailSnapshot::needs_admin(),
                SessionError::Message(text) => CpuDetailSnapshot::unavailable(text),
            };
            return;
        }
        let Some(session) = self.session.as_mut() else {
            sample.cpu_detail = CpuDetailSnapshot::unavailable("ETW セッションがありません");
            return;
        };
        let started = Instant::now();
        if let Ok(mut ledger) = session.inner.ledger.lock() {
            for process in &sample.processes {
                note_process(&mut ledger, process);
            }
            let lost_now = query_lost(session.control);
            if lost_now > session.lost_seen {
                ledger.mark_lost(u64::from(lost_now - session.lost_seen));
            }
            session.lost_seen = lost_now;
            let mut detail = ledger.close(qpc_now());
            let extra = u64::try_from(started.elapsed().as_nanos()).unwrap_or(0);
            detail.aggregate_nanos = detail.aggregate_nanos.saturating_add(extra);
            sample.cpu_detail = detail;
        }
    }
}

fn note_process(ledger: &mut SwitchLedger, process: &RawProcess) {
    ledger.note_live_process(
        process.pid,
        process.creation_time_100ns.unwrap_or(0),
        &process.name,
    );
}

enum SessionError {
    Denied,
    Message(String),
}

fn start_session() -> Result<CONTROLTRACE_HANDLE, SessionError> {
    let mut block = TraceBlock::new();
    let mut handle = CONTROLTRACE_HANDLE::default();
    // SAFETY: `block` is zeroed, sized, and holds the session name for the call.
    let status = unsafe { StartTraceW(&mut handle, block.name_ptr(), block.as_mut_ptr()) };
    if status == ERROR_SUCCESS {
        return Ok(handle);
    }
    if status == ERROR_ACCESS_DENIED {
        return Err(SessionError::Denied);
    }
    if status == ERROR_ALREADY_EXISTS {
        stop_stale_session();
        let mut block = TraceBlock::new();
        let status = unsafe { StartTraceW(&mut handle, block.name_ptr(), block.as_mut_ptr()) };
        if status == ERROR_SUCCESS {
            return Ok(handle);
        }
        if status == ERROR_ACCESS_DENIED {
            return Err(SessionError::Denied);
        }
        return Err(SessionError::Message(format!(
            "ETW セッションを開始できませんでした（{status:?}）"
        )));
    }
    Err(SessionError::Message(format!(
        "ETW セッションを開始できませんでした（{status:?}）"
    )))
}

fn stop_stale_session() {
    let mut block = TraceBlock::new();
    let status = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            block.name_ptr(),
            block.as_mut_ptr(),
            EVENT_TRACE_CONTROL_QUERY,
        )
    };
    if status == ERROR_WMI_INSTANCE_NOT_FOUND || status != ERROR_SUCCESS {
        return;
    }
    let mut block = TraceBlock::new();
    let _ = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            block.name_ptr(),
            block.as_mut_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    };
}

fn stop_handle(handle: CONTROLTRACE_HANDLE) {
    let mut block = TraceBlock::new();
    let _ = unsafe {
        ControlTraceW(
            handle,
            PCWSTR::null(),
            block.as_mut_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    };
}

fn query_lost(handle: CONTROLTRACE_HANDLE) -> u32 {
    let mut block = TraceBlock::new();
    let status = unsafe {
        ControlTraceW(
            handle,
            PCWSTR::null(),
            block.as_mut_ptr(),
            EVENT_TRACE_CONTROL_QUERY,
        )
    };
    if status == ERROR_SUCCESS {
        block.props.EventsLost
    } else {
        0
    }
}

fn open_consumer(inner: &mut Inner, name: &mut [u16]) -> Result<PROCESSTRACE_HANDLE, SessionError> {
    let mut logfile = EVENT_TRACE_LOGFILEW {
        LoggerName: windows::core::PWSTR(name.as_mut_ptr()),
        ..EVENT_TRACE_LOGFILEW::default()
    };
    logfile.Anonymous1.ProcessTraceMode =
        PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(on_event);
    logfile.Context = inner as *mut Inner as *mut core::ffi::c_void;
    // SAFETY: `logfile` and `name` stay alive across OpenTrace. The callback
    // context points at `inner`, which the session owns until ProcessTrace returns.
    let handle = unsafe { OpenTraceW(&mut logfile) };
    if handle.Value == u64::MAX {
        return Err(SessionError::Message(
            "ETW セッションを開けませんでした".to_string(),
        ));
    }
    Ok(handle)
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    // SAFETY: ETW passes a live record for the duration of this call.
    let record = unsafe { &*record };
    if record.UserContext.is_null() {
        return;
    }
    let inner = unsafe { &*(record.UserContext as *const Inner) };
    let Ok(mut ledger) = inner.ledger.lock() else {
        return;
    };
    if !ledger.enabled() {
        return;
    }
    let time = u64::try_from(record.EventHeader.TimeStamp).unwrap_or(0);
    if time == 0 {
        return;
    }
    let cpu = LogicalCpuId::from_processor_index(u32::from(unsafe {
        record.BufferContext.Anonymous.ProcessorIndex
    }));
    let guid = record.EventHeader.ProviderId;
    let opcode = record.EventHeader.EventDescriptor.Opcode;
    let user = user_bytes(record);
    if guid == ThreadGuid && opcode == CSWITCH {
        if let Some((new_tid, old_tid)) = parse_switch_tids(user) {
            ledger.push(CpuEvent::Switch {
                time,
                cpu,
                old_tid,
                new_tid,
            });
        }
        return;
    }
    if guid == ThreadGuid && (opcode == START || opcode == DC_START) {
        if let Some((pid, tid)) = parse_switch_tids(user) {
            ledger.push(CpuEvent::ThreadStart {
                tid,
                pid,
                create_time_100ns: 0,
            });
        }
        return;
    }
    if guid == ThreadGuid && (opcode == END || opcode == DC_END) {
        if let Some((_pid, tid)) = parse_switch_tids(user) {
            ledger.push(CpuEvent::ThreadEnd { tid });
        }
        return;
    }
    if guid == ProcessGuid && (opcode == START || opcode == DC_START) {
        if let Some(pid) = process_id(user) {
            ledger.push(CpuEvent::ProcessStart {
                pid,
                create_time_100ns: 0,
                name: String::new(),
            });
        }
        return;
    }
    if guid == PerfInfoGuid && (opcode == DPC || opcode == ISR) {
        if let Some(duration) = unattributed_duration(user, time) {
            ledger.push(CpuEvent::Unattributed {
                time,
                cpu,
                duration,
            });
        }
    }
}

fn user_bytes(record: &EVENT_RECORD) -> &[u8] {
    let len = usize::from(record.UserDataLength);
    if record.UserData.is_null() || len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(record.UserData as *const u8, len) }
}

fn process_id(user: &[u8]) -> Option<u32> {
    // Process_V2 puts a pointer-sized key before the PID on 64-bit.
    if user.len() >= 16 {
        return Some(u32::from_ne_bytes(user[8..12].try_into().ok()?));
    }
    parse_switch_tids(user).map(|(pid, _)| pid)
}

fn unattributed_duration(user: &[u8], end: u64) -> Option<u64> {
    if user.len() < 8 {
        return None;
    }
    let start = u64::from_ne_bytes(user[0..8].try_into().ok()?);
    if start == 0 || start >= end {
        return None;
    }
    let duration = end - start;
    // Ignore a payload that is not a short interval on this clock.
    if duration > end / 2 {
        return None;
    }
    Some(duration)
}

fn logical_cpus() -> Vec<LogicalCpuId> {
    unsafe {
        let groups = GetActiveProcessorGroupCount().max(1);
        let mut out = Vec::new();
        for group in 0..groups {
            let count = GetActiveProcessorCount(group).min(64);
            for number in 0..count {
                out.push(LogicalCpuId {
                    group,
                    number: u8::try_from(number).unwrap_or(0),
                });
            }
        }
        if out.is_empty() {
            let count = GetActiveProcessorCount(ALL_PROCESSOR_GROUPS).clamp(1, 64);
            for number in 0..count {
                out.push(LogicalCpuId {
                    group: 0,
                    number: u8::try_from(number).unwrap_or(0),
                });
            }
        }
        out
    }
}

fn snapshot_threads() -> Vec<(u32, u32)> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }) else {
        return Vec::new();
    };
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(std::mem::size_of::<THREADENTRY32>()).unwrap_or(0),
        ..THREADENTRY32::default()
    };
    let mut out = Vec::new();
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32ThreadID != 0 {
                out.push((entry.th32ThreadID, entry.th32OwnerProcessID));
            }
            entry.dwSize = u32::try_from(std::mem::size_of::<THREADENTRY32>()).unwrap_or(0);
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let _ = unsafe { CloseHandle(snapshot) };
    out
}

fn qpc_now() -> u64 {
    let mut now = 0i64;
    if unsafe { QueryPerformanceCounter(&mut now) }.is_ok() {
        u64::try_from(now).unwrap_or(0)
    } else {
        0
    }
}

fn session_wide() -> Vec<u16> {
    SESSION.encode_utf16().chain(std::iter::once(0)).collect()
}

#[repr(C)]
struct TraceBlock {
    props: EVENT_TRACE_PROPERTIES,
    name: [u16; 128],
}

impl TraceBlock {
    fn new() -> Self {
        let mut block = Self {
            props: EVENT_TRACE_PROPERTIES::default(),
            name: [0; 128],
        };
        let wide = session_wide();
        let n = wide.len().min(block.name.len());
        block.name[..n].copy_from_slice(&wide[..n]);
        block.name[block.name.len() - 1] = 0;
        block.props.Wnode.BufferSize = u32::try_from(std::mem::size_of::<Self>()).unwrap_or(0);
        block.props.Wnode.Guid = SystemTraceControlGuid;
        block.props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        block.props.Wnode.ClientContext = 1;
        block.props.BufferSize = 64;
        block.props.MinimumBuffers = 16;
        block.props.MaximumBuffers = 64;
        block.props.FlushTimer = 1;
        block.props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
        block.props.EnableFlags = EVENT_TRACE_FLAG_PROCESS
            | EVENT_TRACE_FLAG_THREAD
            | EVENT_TRACE_FLAG_CSWITCH
            | EVENT_TRACE_FLAG_DPC
            | EVENT_TRACE_FLAG_INTERRUPT;
        block.props.LoggerNameOffset =
            u32::try_from(std::mem::size_of::<EVENT_TRACE_PROPERTIES>()).unwrap_or(0);
        block
    }

    fn name_ptr(&self) -> PCWSTR {
        PCWSTR(self.name.as_ptr())
    }

    fn as_mut_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        &mut self.props
    }
}

/// Asks Windows to start a new elevated process. Does nothing until called.
#[cfg(feature = "ui")]
pub fn relaunch_elevated() -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let exe = std::env::current_exe()
        .map_err(|err| format!("実行ファイルの場所が分かりません: {err}"))?;
    let mut file: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_mut_ptr()),
            PCWSTR(file.as_mut_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result.0 as isize) <= 32 {
        return Err("管理者としての再起動を開始できませんでした".to_string());
    }
    Ok(())
}
