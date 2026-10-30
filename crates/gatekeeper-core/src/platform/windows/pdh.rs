//! GPU memory performance counters through PDH (spec §12.2, F2, F3).
//!
//! Responsibility: open one PDH query holding the four GPU memory counters,
//! collect it, and return the per-instance byte values as plain Rust data.
//! Not responsible for: interpreting instance names or choosing an adapter
//! (see `gpu::instance`).
//!
//! Counter names are always added with `PdhAddEnglishCounterW` so they work on
//! localised Windows installs (spec F3).
//!
//! Implements FR-VRAM-1.

use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::PCWSTR;

use super::wide;
use crate::error::{CoreError, CoreResult};
use crate::gpu::instance::RawReadings;

/// PDH status meaning "buffer too small, call again with the size I gave you".
/// <https://learn.microsoft.com/windows/win32/perfctrs/pdh-error-codes>
const PDH_MORE_DATA: u32 = 0x8000_07D2;

/// PDH value status meaning the value is valid (`PDH_CSTATUS_VALID_DATA`).
const PDH_CSTATUS_VALID_DATA: u32 = 0;
/// PDH value status for the first valid sample of a counter
/// (`PDH_CSTATUS_NEW_DATA`).
const PDH_CSTATUS_NEW_DATA: u32 = 1;

/// Counter paths, in the order `read` fills `RawReadings`.
const PATHS: [&str; 4] = [
    r"\GPU Adapter Memory(*)\Dedicated Usage",
    r"\GPU Adapter Memory(*)\Shared Usage",
    r"\GPU Process Memory(*)\Dedicated Usage",
    r"\GPU Process Memory(*)\Shared Usage",
];

/// An open PDH query with the GPU memory counters attached.
pub struct PdhQuery {
    /// Query handle from `PdhOpenQueryW`; closed in `Drop`.
    query: PDH_HQUERY,
    /// One handle per entry of [`PATHS`].
    counters: Vec<PDH_HCOUNTER>,
}

// SAFETY: PDH handles are opaque identifiers owned by this struct. PDH
// serialises calls on a query internally, and `PdhQuery` is only ever used
// behind a `Mutex`, so moving it between threads is sound.
unsafe impl Send for PdhQuery {}

impl PdhQuery {
    /// Opens the query and adds the counters.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if the GPU counters do not exist (old
    /// drivers, spec §21) or PDH fails. The app then disables VRAM features.
    pub fn new() -> CoreResult<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: `query` is a valid out-pointer; a null data source means the
        // live system counters.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhopenqueryw
        let rc = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
        if rc != 0 {
            return Err(CoreError::Platform(format!("PdhOpenQueryW failed: 0x{rc:08X}")));
        }

        let mut this = Self { query, counters: Vec::new() };
        for path in PATHS {
            let wide = wide(path);
            let mut counter = PDH_HCOUNTER::default();
            // SAFETY: `wide` is NUL-terminated and outlives the call, and
            // `counter` is a valid out-pointer. The English variant is used
            // so localised Windows works (spec F3).
            // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhaddenglishcounterw
            let rc = unsafe { PdhAddEnglishCounterW(this.query, PCWSTR(wide.as_ptr()), 0, &raw mut counter) };
            if rc != 0 {
                // `this` drops and closes the query.
                return Err(CoreError::Platform(format!("GPU counter '{path}' is unavailable (PDH 0x{rc:08X})")));
            }
            this.counters.push(counter);
        }
        // Prime the query: some counters need one collection before the first read.
        // SAFETY: `this.query` is the valid handle opened above.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhcollectquerydata
        let _ = unsafe { PdhCollectQueryData(this.query) };
        Ok(this)
    }

    /// Collects the query and reads every counter.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if collection or a read fails.
    pub fn read(&mut self) -> CoreResult<RawReadings> {
        // SAFETY: `self.query` is the valid handle opened in `new`.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhcollectquerydata
        let rc = unsafe { PdhCollectQueryData(self.query) };
        if rc != 0 {
            return Err(CoreError::Platform(format!("PdhCollectQueryData failed: 0x{rc:08X}")));
        }
        let mut arrays = Vec::with_capacity(self.counters.len());
        for counter in &self.counters {
            arrays.push(read_array(*counter)?);
        }
        let mut it = arrays.into_iter();
        let mut next = || it.next().unwrap_or_default();
        Ok(RawReadings {
            adapter_dedicated: next(),
            adapter_shared: next(),
            process_dedicated: next(),
            process_shared: next(),
        })
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        // SAFETY: closing a query we own; this also frees its counters.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhclosequery
        let _ = unsafe { PdhCloseQuery(self.query) };
    }
}

/// Reads all instances of one wildcard counter as `(instance name, bytes)`.
fn read_array(counter: PDH_HCOUNTER) -> CoreResult<Vec<(String, u64)>> {
    let mut bytes: u32 = 0;
    let mut count: u32 = 0;
    // First call with no buffer asks how much space is needed.
    // SAFETY: a null buffer with size 0 is the documented way to query the
    // required size; `bytes` and `count` are valid out-pointers.
    // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhgetformattedcounterarrayw
    let rc = unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &raw mut bytes, &raw mut count, None) };
    if rc == 0 {
        return Ok(Vec::new()); // no instances right now
    }
    if rc != PDH_MORE_DATA {
        return Err(CoreError::Platform(format!("PdhGetFormattedCounterArrayW (size) failed: 0x{rc:08X}")));
    }

    // Items contain pointers, so the buffer needs 8-byte alignment: allocate
    // u64 words, not bytes. The instance-name strings are stored in the tail
    // of this same buffer, so it must outlive the reads below.
    let words = (bytes as usize).div_ceil(8) + 1;
    let mut buffer = vec![0u64; words];
    let items_ptr = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    // SAFETY: `items_ptr` points to at least `bytes` writable, 8-aligned bytes
    // (we allocated `words * 8 >= bytes`), and `bytes`/`count` are valid.
    // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhgetformattedcounterarrayw
    let rc = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &raw mut bytes, &raw mut count, Some(items_ptr))
    };
    if rc != 0 {
        return Err(CoreError::Platform(format!("PdhGetFormattedCounterArrayW failed: 0x{rc:08X}")));
    }

    // SAFETY: on success PDH filled `count` consecutive items at `items_ptr`.
    let items = unsafe { std::slice::from_raw_parts(items_ptr, count as usize) };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA {
            continue;
        }
        // SAFETY: `szName` points to a NUL-terminated UTF-16 string stored
        // inside `buffer`, which is still alive here.
        let name = unsafe { item.szName.to_string() }.unwrap_or_default();
        // SAFETY: we requested PDH_FMT_LARGE, so `largeValue` is the active
        // union member.
        let value = unsafe { item.FmtValue.Anonymous.largeValue };
        out.push((name, u64::try_from(value).unwrap_or(0)));
    }
    Ok(out)
}

/// `\GPU Engine(*)\Utilization Percentage`, used only by the heavy-GPU
/// heuristic (spec §11.5), so it is never added when the heuristic is off.
pub struct EngineQuery {
    /// Query handle; closed in `Drop`.
    query: PDH_HQUERY,
    /// The utilisation counter.
    counter: PDH_HCOUNTER,
}

// SAFETY: see `PdhQuery`; the same reasoning applies, and the value is only
// used behind a `Mutex`.
unsafe impl Send for EngineQuery {}

impl EngineQuery {
    /// Opens the query.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if the counter does not exist.
    pub fn new() -> CoreResult<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: valid out-pointer, null data source (live counters).
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhopenqueryw
        let rc = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &raw mut query) };
        if rc != 0 {
            return Err(CoreError::Platform(format!("PdhOpenQueryW failed: 0x{rc:08X}")));
        }
        let path = wide(r"\GPU Engine(*)\Utilization Percentage");
        let mut counter = PDH_HCOUNTER::default();
        // SAFETY: NUL-terminated path, valid out-pointer; English names (spec F3).
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhaddenglishcounterw
        let rc = unsafe { PdhAddEnglishCounterW(query, PCWSTR(path.as_ptr()), 0, &raw mut counter) };
        if rc != 0 {
            // SAFETY: closing the query we just opened.
            let _ = unsafe { PdhCloseQuery(query) };
            return Err(CoreError::Platform(format!("GPU engine counter unavailable (PDH 0x{rc:08X})")));
        }
        // Utilisation is a rate: the first collection only primes it.
        // SAFETY: valid query handle.
        let _ = unsafe { PdhCollectQueryData(query) };
        Ok(Self { query, counter })
    }

    /// Reads `(instance name, percent)` for every engine instance.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if collection or the read fails.
    pub fn read(&mut self) -> CoreResult<Vec<(String, f64)>> {
        // SAFETY: valid query handle.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhcollectquerydata
        let rc = unsafe { PdhCollectQueryData(self.query) };
        if rc != 0 {
            return Err(CoreError::Platform(format!("PdhCollectQueryData failed: 0x{rc:08X}")));
        }
        let mut bytes = 0u32;
        let mut count = 0u32;
        // SAFETY: size query with no buffer, valid out-pointers.
        // https://learn.microsoft.com/windows/win32/api/pdh/nf-pdh-pdhgetformattedcounterarrayw
        let rc =
            unsafe { PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &raw mut bytes, &raw mut count, None) };
        if rc == 0 {
            return Ok(Vec::new());
        }
        if rc != PDH_MORE_DATA {
            return Err(CoreError::Platform(format!("PdhGetFormattedCounterArrayW (size) failed: 0x{rc:08X}")));
        }
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8) + 1];
        let items_ptr = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        // SAFETY: the buffer holds at least `bytes` 8-aligned bytes.
        let rc = unsafe {
            PdhGetFormattedCounterArrayW(self.counter, PDH_FMT_DOUBLE, &raw mut bytes, &raw mut count, Some(items_ptr))
        };
        if rc != 0 {
            return Err(CoreError::Platform(format!("PdhGetFormattedCounterArrayW failed: 0x{rc:08X}")));
        }
        // SAFETY: on success PDH filled `count` items at `items_ptr`.
        let items = unsafe { std::slice::from_raw_parts(items_ptr, count as usize) };
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA {
                continue;
            }
            // SAFETY: `szName` points into `buffer`, which is alive.
            let name = unsafe { item.szName.to_string() }.unwrap_or_default();
            // SAFETY: we requested PDH_FMT_DOUBLE, so `doubleValue` is the active member.
            out.push((name, unsafe { item.FmtValue.Anonymous.doubleValue }));
        }
        Ok(out)
    }
}

impl Drop for EngineQuery {
    fn drop(&mut self) {
        // SAFETY: closing a query we own.
        let _ = unsafe { PdhCloseQuery(self.query) };
    }
}

#[cfg(test)]
mod engine_tests {
    use super::*;
    use crate::gpu::instance::{parse_engine_type, parse_instance};

    #[test]
    fn engine_counter_reads_real_instances() {
        let Ok(mut q) = EngineQuery::new() else {
            eprintln!("skipped: no GPU engine counters on this machine");
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let readings = q.read().expect("read");
        eprintln!("{} engine instances; first: {:?}", readings.len(), readings.first());
        for (name, value) in &readings {
            assert!(parse_instance(name).is_some() && parse_engine_type(name).is_some(), "unparseable instance {name}");
            assert!(*value >= 0.0);
        }
    }
}
