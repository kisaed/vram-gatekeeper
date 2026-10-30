//! Parsing and aggregation of Windows GPU performance-counter instances
//! (spec §12.2).
//!
//! Responsibility: turn instance names such as
//! `pid_1234_luid_0x00000000_0x0000F4F3_phys_0` into structured values and sum
//! them per process / per adapter. Pure code with no Win32 calls, so it is
//! tested on plain strings.
//! Not responsible for: querying PDH (see `platform::windows::pdh`).
//!
//! Implements FR-VRAM-1.

use std::collections::HashMap;

use super::{BYTES_PER_MIB, ProcessVram, VramSample};

/// Identifies a GPU adapter the way Windows names it in counter instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Luid {
    /// High 32 bits (signed in Win32, shown as hex).
    pub high: u32,
    /// Low 32 bits.
    pub low: u32,
}

impl Luid {
    /// Formats the LUID as it appears in counter instance names.
    pub fn to_instance_string(self) -> String {
        format!("luid_0x{:08X}_0x{:08X}", self.high, self.low)
    }

    /// Parses `luid_0xHHHHHHHH_0xLLLLLLLL` (case-insensitive).
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.trim().to_ascii_lowercase();
        let rest = rest.strip_prefix("luid_0x")?;
        let (high, low) = rest.split_once("_0x")?;
        Some(Self { high: u32::from_str_radix(high, 16).ok()?, low: u32::from_str_radix(low, 16).ok()? })
    }
}

/// A parsed counter instance name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instance {
    /// Owning process, for per-process counters.
    pub pid: Option<u32>,
    /// Adapter the value belongs to.
    pub luid: Luid,
    /// Physical adapter index (`phys_N`); values are summed across these.
    pub phys: u32,
}

/// Parses an instance name. Returns `None` for names without a LUID.
///
/// Examples: `luid_0x00000000_0x0000F4F3_phys_0` (adapter counters) and
/// `pid_1234_luid_0x00000000_0x0000F4F3_phys_0` (process counters). Engine
/// counters append `_eng_N_engtype_3D`, which is ignored here.
pub fn parse_instance(name: &str) -> Option<Instance> {
    let lower = name.to_ascii_lowercase();
    let pid = lower.strip_prefix("pid_").and_then(|r| r.split('_').next()).and_then(|p| p.parse().ok());
    let luid_at = lower.find("luid_0x")?;
    let luid_part = &lower[luid_at..];
    let mut pieces = luid_part.split('_');
    let (_, high, low) = (pieces.next()?, pieces.next()?, pieces.next()?);
    let luid = Luid::parse(&format!("luid_{high}_{low}"))?;
    let phys = luid_part
        .find("phys_")
        .and_then(|i| luid_part[i + "phys_".len()..].split('_').next())
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    Some(Instance { pid, luid, phys })
}

/// The engine type of an engine counter instance, e.g. `3D` or `Copy` from
/// `pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_3D`.
pub fn parse_engine_type(name: &str) -> Option<&str> {
    let i = name.to_ascii_lowercase().find("_engtype_")? + "_engtype_".len();
    Some(&name[i..])
}

/// Highest 3D-engine utilisation (percent) of process `pid` on adapter `luid`.
///
/// Counter instances are `..._phys_P_eng_E_engtype_T`; values for one engine
/// across `phys` instances are summed, then the busiest 3D engine wins (the
/// number Task Manager shows for a process).
pub fn process_3d_utilization(readings: &[(String, f64)], pid: u32, luid: Luid) -> f64 {
    let mut per_engine: HashMap<String, f64> = HashMap::new();
    // There is one instance per process, engine and adapter (hundreds), so
    // skip other processes before any parsing allocates.
    let prefix = format!("pid_{pid}_");
    for (name, value) in readings {
        if !name.as_bytes().get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes())) {
            continue;
        }
        if parse_instance(name).is_none_or(|i| i.pid != Some(pid) || i.luid != luid) {
            continue;
        }
        if !parse_engine_type(name).is_some_and(|t| t.eq_ignore_ascii_case("3d")) {
            continue;
        }
        let lower = name.to_ascii_lowercase();
        let engine =
            lower.find("_eng_").map_or(String::new(), |i| lower[i..].split('_').nth(2).unwrap_or_default().to_owned());
        *per_engine.entry(engine).or_default() += value;
    }
    per_engine.values().copied().fold(0.0, f64::max)
}

/// Raw counter values from one query: `(instance name, bytes)` per counter.
#[derive(Debug, Default, Clone)]
pub struct RawReadings {
    /// `\GPU Adapter Memory(*)\Dedicated Usage`
    pub adapter_dedicated: Vec<(String, u64)>,
    /// `\GPU Adapter Memory(*)\Shared Usage`
    pub adapter_shared: Vec<(String, u64)>,
    /// `\GPU Process Memory(*)\Dedicated Usage`
    pub process_dedicated: Vec<(String, u64)>,
    /// `\GPU Process Memory(*)\Shared Usage`
    pub process_shared: Vec<(String, u64)>,
}

/// Sums values belonging to `luid`, keyed by pid (`None` for adapter totals).
fn sum_for(readings: &[(String, u64)], luid: Luid) -> HashMap<Option<u32>, u64> {
    let mut out: HashMap<Option<u32>, u64> = HashMap::new();
    for (name, bytes) in readings {
        if let Some(inst) = parse_instance(name).filter(|i| i.luid == luid) {
            *out.entry(inst.pid).or_default() += bytes;
        }
    }
    out
}

/// Builds a [`VramSample`] for one adapter from raw readings.
///
/// Uses the adapter-level counter for `used_mib` (decisions) and the
/// per-process counters only for attribution (spec F4).
pub fn build_sample(raw: &RawReadings, luid: Luid, total_mib: u64) -> VramSample {
    let used = sum_for(&raw.adapter_dedicated, luid).get(&None).copied().unwrap_or(0);
    let shared = sum_for(&raw.adapter_shared, luid).get(&None).copied().unwrap_or(0);
    let dedicated_by_pid = sum_for(&raw.process_dedicated, luid);
    let shared_by_pid = sum_for(&raw.process_shared, luid);

    let mut pids: Vec<u32> = dedicated_by_pid.keys().chain(shared_by_pid.keys()).filter_map(|p| *p).collect();
    pids.sort_unstable();
    pids.dedup();
    let per_process = pids
        .into_iter()
        .map(|pid| ProcessVram {
            pid,
            dedicated_mib: dedicated_by_pid.get(&Some(pid)).copied().unwrap_or(0) / BYTES_PER_MIB,
            shared_mib: shared_by_pid.get(&Some(pid)).copied().unwrap_or(0) / BYTES_PER_MIB,
        })
        .collect();

    VramSample { total_mib, used_mib: used / BYTES_PER_MIB, shared_used_mib: shared / BYTES_PER_MIB, per_process }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: Luid = Luid { high: 0, low: 0xF4F3 };
    const OTHER: Luid = Luid { high: 0, low: 0x1234 };

    #[test]
    fn parses_adapter_and_process_instances() {
        let a = parse_instance("luid_0x00000000_0x0000F4F3_phys_0").expect("adapter");
        assert_eq!((a.pid, a.luid, a.phys), (None, ME, 0));
        let p = parse_instance("pid_1234_luid_0x00000000_0x0000F4F3_phys_1").expect("process");
        assert_eq!((p.pid, p.luid, p.phys), (Some(1234), ME, 1));
        let e = parse_instance("pid_77_luid_0x00000000_0x0000F4F3_phys_0_eng_3_engtype_3D").expect("engine");
        assert_eq!((e.pid, e.phys), (Some(77), 0));
        assert_eq!(parse_instance("total"), None);
    }

    #[test]
    fn luid_round_trips_through_text() {
        assert_eq!(Luid::parse(&ME.to_instance_string()), Some(ME));
        assert_eq!(Luid::parse("LUID_0x00000000_0x0000f4f3"), Some(ME));
        assert_eq!(Luid::parse("nonsense"), None);
    }

    #[test]
    fn three_d_utilisation_is_the_busiest_3d_engine_of_the_process() {
        let readings = vec![
            ("pid_10_luid_0x00000000_0x0000F4F3_phys_0_eng_0_engtype_3D".to_owned(), 40.0),
            ("pid_10_luid_0x00000000_0x0000F4F3_phys_1_eng_0_engtype_3D".to_owned(), 20.0), // same engine, other phys: summed
            ("pid_10_luid_0x00000000_0x0000F4F3_phys_0_eng_1_engtype_3D".to_owned(), 55.0),
            ("pid_10_luid_0x00000000_0x0000F4F3_phys_0_eng_2_engtype_Copy".to_owned(), 99.0), // not 3D
            ("pid_11_luid_0x00000000_0x0000F4F3_phys_0_eng_0_engtype_3D".to_owned(), 90.0),   // other process
            ("pid_10_luid_0x00000000_0x00001234_phys_0_eng_0_engtype_3D".to_owned(), 80.0),   // other adapter
        ];
        assert!((process_3d_utilization(&readings, 10, ME) - 60.0).abs() < 1e-9);
        assert_eq!(process_3d_utilization(&readings, 12, ME), 0.0);
        assert_eq!(parse_engine_type("pid_1_luid_0x0_0x1_phys_0_eng_3_engtype_VideoDecode"), Some("VideoDecode"));
    }

    #[test]
    fn sample_sums_phys_instances_and_filters_by_adapter() {
        let mib = |n: u64| n * BYTES_PER_MIB;
        let raw = RawReadings {
            adapter_dedicated: vec![
                ("luid_0x00000000_0x0000F4F3_phys_0".into(), mib(5000)),
                ("luid_0x00000000_0x0000F4F3_phys_1".into(), mib(100)),
                ("luid_0x00000000_0x00001234_phys_0".into(), mib(9999)), // another GPU
            ],
            adapter_shared: vec![("luid_0x00000000_0x0000F4F3_phys_0".into(), mib(300))],
            process_dedicated: vec![
                ("pid_10_luid_0x00000000_0x0000F4F3_phys_0".into(), mib(2000)),
                ("pid_10_luid_0x00000000_0x0000F4F3_phys_1".into(), mib(50)),
                ("pid_11_luid_0x00000000_0x0000F4F3_phys_0".into(), mib(700)),
                ("pid_12_luid_0x00000000_0x00001234_phys_0".into(), mib(8000)),
            ],
            process_shared: vec![("pid_10_luid_0x00000000_0x0000F4F3_phys_0".into(), mib(640))],
        };
        let s = build_sample(&raw, ME, 32_000);
        assert_eq!((s.total_mib, s.used_mib, s.shared_used_mib), (32_000, 5100, 300));
        assert_eq!(s.free_mib(), 26_900);
        assert_eq!(s.per_process.len(), 2, "the other adapter's process is excluded");
        let p10 = s.process(10).expect("pid 10");
        assert_eq!((p10.dedicated_mib, p10.shared_mib), (2050, 640));
        assert_eq!(s.process(11).expect("pid 11").shared_mib, 0);
        assert!(s.process(12).is_none());
        assert_eq!(build_sample(&raw, OTHER, 8000).used_mib, 9999);
    }
}
