//! English PDH counters. Paths stay in English so a Japanese Windows install
//! resolves the same names. The first collect often has no data; that is not a zero.

use std::cell::Cell;

use windows::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA,
    PDH_FMT_COUNTERVALUE, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY,
    PDH_MORE_DATA, PDH_NO_DATA,
};

pub enum PdhSample {
    /// Counter exists and this interval produced instances (possibly empty).
    Ready(Vec<(String, f64)>),
    /// The query is open but this collect has no interval yet.
    NoData,
}

pub struct PdhQuery {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    last_status: Cell<u32>,
}

// The sampler moves each enricher onto one worker thread and never shares the
// query. PDH handles are raw pointers, so Send is not automatic.
//
// SAFETY: `PdhQuery` owns the query and is not cloned. It is moved to the
// sampler thread before `open` runs, and only that thread calls PDH.
unsafe impl Send for PdhQuery {}

impl PdhQuery {
    pub fn open(path: &str) -> Result<Self, u32> {
        unsafe {
            let mut query = PDH_HQUERY::default();
            let status = PdhOpenQueryW(windows::core::PCWSTR::null(), 0, &mut query);
            if status != 0 {
                return Err(status);
            }
            let mut wide: Vec<u16> = path.encode_utf16().collect();
            wide.push(0);
            let mut counter = PDH_HCOUNTER::default();
            let status =
                PdhAddEnglishCounterW(query, windows::core::PCWSTR(wide.as_ptr()), 0, &mut counter);
            if status != 0 {
                let _ = PdhCloseQuery(query);
                return Err(status);
            }
            let _ = PdhCollectQueryData(query);
            Ok(Self {
                query,
                counter,
                last_status: Cell::new(0),
            })
        }
    }

    pub fn status(&self) -> u32 {
        self.last_status.get()
    }

    pub fn collect(&self) -> Option<PdhSample> {
        unsafe {
            let status = PdhCollectQueryData(self.query);
            if status == PDH_NO_DATA {
                return Some(PdhSample::NoData);
            }
            if status != 0 && status != PDH_CSTATUS_NEW_DATA && status != PDH_CSTATUS_VALID_DATA {
                self.last_status.set(status);
                return None;
            }
            self.read_array()
        }
    }

    fn read_array(&self) -> Option<PdhSample> {
        unsafe {
            let mut size = 0u32;
            let mut count = 0u32;
            let probe = PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                None,
            );
            if probe == PDH_NO_DATA {
                return Some(PdhSample::NoData);
            }
            if probe != 0 && probe != PDH_MORE_DATA {
                return self.collect_single();
            }
            // The size probe often leaves `count` at 0 while `size` is the
            // byte count. Treating that as an empty array made every card N/A.
            if size == 0 {
                return Some(PdhSample::Ready(Vec::new()));
            }
            let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            for _ in 0..4 {
                if size > 8 * 1024 * 1024 {
                    self.last_status.set(PDH_MORE_DATA);
                    return None;
                }
                let mut items = vec![
                    PDH_FMT_COUNTERVALUE_ITEM_W::default();
                    (size as usize).div_ceil(item_size).max(1)
                ];
                size = (items.len() * item_size) as u32;
                let status = PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    Some(items.as_mut_ptr()),
                );
                if status == PDH_MORE_DATA {
                    continue;
                }
                if status == PDH_NO_DATA {
                    return Some(PdhSample::NoData);
                }
                if status != 0 {
                    self.last_status.set(status);
                    return None;
                }
                let mut values = Vec::new();
                for item in items.iter().take(count as usize) {
                    if !status_ok(item.FmtValue.CStatus) {
                        continue;
                    }
                    let number = item.FmtValue.Anonymous.doubleValue;
                    if number.is_finite() && number >= 0.0 {
                        values.push((wide_string(item.szName), number));
                    }
                }
                return Some(PdhSample::Ready(values));
            }
            self.last_status.set(PDH_MORE_DATA);
            None
        }
    }

    fn collect_single(&self) -> Option<PdhSample> {
        unsafe {
            let mut value = PDH_FMT_COUNTERVALUE::default();
            let status =
                PdhGetFormattedCounterValue(self.counter, PDH_FMT_DOUBLE, None, &mut value);
            if status == PDH_NO_DATA {
                return Some(PdhSample::NoData);
            }
            if status != 0 || !status_ok(value.CStatus) {
                self.last_status.set(status);
                return None;
            }
            let number = value.Anonymous.doubleValue;
            if number.is_finite() && number >= 0.0 {
                Some(PdhSample::Ready(vec![("_Total".to_string(), number)]))
            } else {
                Some(PdhSample::NoData)
            }
        }
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            unsafe {
                let _ = PdhCloseQuery(self.query);
            }
            self.query = PDH_HQUERY::default();
        }
    }
}

fn status_ok(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

fn wide_string(pointer: windows::core::PWSTR) -> String {
    if pointer.0.is_null() {
        return String::new();
    }
    unsafe {
        let mut len = 0usize;
        while len < 1024 && *pointer.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(pointer.0, len))
    }
}
