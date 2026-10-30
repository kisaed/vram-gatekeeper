//! GPU and VRAM monitoring (spec §12).
//!
//! Responsibility: the [`VramSample`] data type and the [`GpuMonitor`] trait.
//! The Windows implementation (PDH + DXGI) lives in `pdh.rs` / `dxgi.rs`.
//! Not responsible for: acting on samples (see `gate::vram_rules`).
//!
//! Implements FR-VRAM-1.

use async_trait::async_trait;
use serde::Serialize;

/// Bytes per MiB.
pub const BYTES_PER_MIB: u64 = 1024 * 1024;

/// GPU memory used by one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ProcessVram {
    /// Process id.
    pub pid: u32,
    /// Dedicated (on-card) memory in MiB.
    pub dedicated_mib: u64,
    /// Shared (system RAM) memory in MiB; rising values indicate spill.
    pub shared_mib: u64,
}

/// One reading of the monitored adapter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VramSample {
    /// Total dedicated VRAM of the adapter, MiB.
    pub total_mib: u64,
    /// Dedicated VRAM in use across all processes, MiB. This is the
    /// adapter-level counter used for pressure decisions (spec F4).
    pub used_mib: u64,
    /// Shared memory in use on the adapter, MiB.
    pub shared_used_mib: u64,
    /// Per-process breakdown, used for attribution and spill detection only.
    pub per_process: Vec<ProcessVram>,
}

impl VramSample {
    /// Free dedicated VRAM in MiB.
    pub fn free_mib(&self) -> u64 {
        self.total_mib.saturating_sub(self.used_mib)
    }

    /// Looks up one process.
    pub fn process(&self, pid: u32) -> Option<&ProcessVram> {
        self.per_process.iter().find(|p| p.pid == pid)
    }
}

/// Why sampling failed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct GpuError(pub String);

/// A source of VRAM readings.
#[async_trait]
pub trait GpuMonitor: Send + Sync {
    /// Takes one reading.
    ///
    /// # Errors
    /// Returns [`GpuError`] if counters are unavailable (spec §21: VRAM
    /// features are then disabled with a notice).
    async fn sample(&self) -> Result<VramSample, GpuError>;
}

pub mod instance;
pub mod sampler;
#[cfg(windows)]
pub mod windows_monitor;

/// A GPU adapter as reported by DXGI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdapterInfo {
    /// Marketing name, e.g. `NVIDIA GeForce RTX 5090`.
    pub name: String,
    /// PCI vendor id (0x10DE NVIDIA, 0x1002 AMD, 0x8086 Intel).
    pub vendor_id: u32,
    /// LUID in counter-instance form, `luid_0xHHHHHHHH_0xLLLLLLLL`; this is the
    /// value stored in `engine.gpu_adapter_luid`.
    pub luid: String,
    /// Total dedicated video memory in MiB.
    pub dedicated_mib: u64,
    /// True for Microsoft's software adapter.
    pub software: bool,
    /// True for integrated GPUs that share system memory (spec F5).
    pub integrated: bool,
}

/// Chooses the adapter to monitor (spec §12.1): the one named in
/// `engine.gpu_adapter_luid` if it exists, else the discrete adapter with the
/// most dedicated memory.
pub fn select_adapter<'a>(adapters: &'a [AdapterInfo], preferred_luid: Option<&str>) -> Option<&'a AdapterInfo> {
    if let Some(want) = preferred_luid.map(str::trim).filter(|w| !w.is_empty())
        && let Some(found) = adapters.iter().find(|a| a.luid.eq_ignore_ascii_case(want))
    {
        return Some(found);
    }
    adapters.iter().filter(|a| !a.software).max_by_key(|a| (!a.integrated, a.dedicated_mib))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str, luid: &str, mib: u64, software: bool, integrated: bool) -> AdapterInfo {
        AdapterInfo { name: name.into(), vendor_id: 0, luid: luid.into(), dedicated_mib: mib, software, integrated }
    }

    #[test]
    fn picks_the_biggest_discrete_adapter_by_default() {
        let list = vec![
            adapter("iGPU", "luid_0x00000000_0x00000001", 2048, false, true),
            adapter("Basic Render", "luid_0x00000000_0x00000002", 0, true, false),
            adapter("Small dGPU", "luid_0x00000000_0x00000003", 8192, false, false),
            adapter("Big dGPU", "luid_0x00000000_0x00000004", 32768, false, false),
        ];
        assert_eq!(select_adapter(&list, None).map(|a| a.name.as_str()), Some("Big dGPU"));
    }

    #[test]
    fn prefers_discrete_over_a_larger_integrated_adapter() {
        let list = vec![
            adapter("APU with 16 GB carve-out", "luid_0x00000000_0x00000001", 16384, false, true),
            adapter("dGPU", "luid_0x00000000_0x00000002", 8192, false, false),
        ];
        assert_eq!(select_adapter(&list, None).map(|a| a.name.as_str()), Some("dGPU"));
    }

    #[test]
    fn explicit_luid_wins_and_unknown_luid_falls_back() {
        let list = vec![
            adapter("A", "luid_0x00000000_0x0000000A", 1000, false, false),
            adapter("B", "luid_0x00000000_0x0000000B", 9000, false, false),
        ];
        assert_eq!(select_adapter(&list, Some("LUID_0x00000000_0x0000000a")).map(|a| a.name.as_str()), Some("A"));
        assert_eq!(select_adapter(&list, Some("luid_0x0_0xFFFF")).map(|a| a.name.as_str()), Some("B"));
    }

    #[test]
    fn no_adapters_yields_none() {
        assert!(select_adapter(&[], None).is_none());
    }
}
