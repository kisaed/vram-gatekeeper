//! The production [`GpuMonitor`]: PDH counters for one DXGI adapter
//! (spec §12, decision D5).
//!
//! Responsibility: pick the adapter, hold the PDH query, and produce
//! [`VramSample`]s. Integrated GPUs are refused because their "dedicated"
//! memory reads near zero (spec F5); the app then disables VRAM rules and
//! says so, while game detection keeps working.
//! Not responsible for: acting on samples (see `gate::vram_rules`).
//!
//! Implements FR-VRAM-1.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::instance::{Luid, build_sample};
use super::{AdapterInfo, GpuError, GpuMonitor, VramSample, select_adapter};
use crate::error::{CoreError, CoreResult};
use crate::platform::windows::{dxgi, pdh::PdhQuery};
use crate::sync::MutexExt;

/// Samples one adapter through PDH.
pub struct WindowsGpuMonitor {
    /// The adapter being watched.
    adapter: AdapterInfo,
    /// Parsed LUID used to filter counter instances.
    luid: Luid,
    /// The query; PDH calls block briefly, so they run on a blocking thread.
    query: Arc<Mutex<PdhQuery>>,
}

impl WindowsGpuMonitor {
    /// Chooses an adapter and opens the counters.
    ///
    /// # Errors
    /// Fails if no usable adapter exists, the adapter is integrated (VRAM
    /// features are disabled there), or the GPU counters are missing.
    pub fn new(preferred_luid: Option<&str>) -> CoreResult<Self> {
        Self::from_adapters(&dxgi::list_adapters()?, preferred_luid)
    }

    /// Like [`Self::new`], choosing among adapters that were already listed
    /// (listing them creates a device per adapter, so callers that have the
    /// list should not repeat it).
    ///
    /// # Errors
    /// Same as [`Self::new`].
    pub fn from_adapters(adapters: &[AdapterInfo], preferred_luid: Option<&str>) -> CoreResult<Self> {
        let adapter = select_adapter(adapters, preferred_luid)
            .ok_or_else(|| CoreError::Platform("no GPU adapter found".into()))?
            .clone();
        if adapter.integrated {
            return Err(CoreError::Platform(format!(
                "'{}' is an integrated GPU that shares system memory; VRAM rules are disabled on it",
                adapter.name
            )));
        }
        let luid = Luid::parse(&adapter.luid).ok_or_else(|| CoreError::Platform("unreadable adapter LUID".into()))?;
        Ok(Self { adapter, luid, query: Arc::new(Mutex::new(PdhQuery::new()?)) })
    }

    /// The adapter being monitored.
    pub fn adapter(&self) -> &AdapterInfo {
        &self.adapter
    }

    /// Reads one sample on the calling thread. PDH blocks briefly, so call
    /// this from a blocking context (see [`GpuMonitor::sample`] for the async form).
    ///
    /// # Errors
    /// Returns the counter error.
    pub fn sample_blocking(&self) -> Result<VramSample, GpuError> {
        read_sample(&self.query, self.luid, self.adapter.dedicated_mib)
    }

    /// The adapter's LUID, for filtering counter instances.
    pub fn luid(&self) -> Luid {
        self.luid
    }
}

/// Lists adapters for the settings page.
///
/// # Errors
/// Returns an error if DXGI cannot be initialised.
pub fn list_adapters() -> CoreResult<Vec<AdapterInfo>> {
    dxgi::list_adapters()
}

#[async_trait]
impl GpuMonitor for WindowsGpuMonitor {
    async fn sample(&self) -> Result<VramSample, GpuError> {
        let query = self.query.clone();
        let (luid, total) = (self.luid, self.adapter.dedicated_mib);
        // PDH takes a few hundred microseconds to a few milliseconds; a
        // blocking thread keeps it off the async workers.
        tokio::task::spawn_blocking(move || read_sample(&query, luid, total))
            .await
            .map_err(|e| GpuError(format!("sampler task failed: {e}")))?
    }
}

/// Reads the counters and builds a sample for `luid`.
fn read_sample(query: &Mutex<PdhQuery>, luid: Luid, total_mib: u64) -> Result<VramSample, GpuError> {
    let mut q = query.lock_ok();
    q.read().map(|raw| build_sample(&raw, luid, total_mib)).map_err(|e| GpuError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads GPU memory in use from nvidia-smi, if it is installed.
    fn nvidia_smi_used_mib() -> Option<u64> {
        let out = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
    }

    #[tokio::test]
    async fn adapter_counters_agree_with_nvidia_smi() {
        let Some(smi) = nvidia_smi_used_mib() else {
            eprintln!("skipped: nvidia-smi not available");
            return;
        };
        let Ok(monitor) = WindowsGpuMonitor::new(None) else {
            eprintln!("skipped: no discrete GPU counters on this machine");
            return;
        };
        let sample = monitor.sample().await.expect("sample");
        eprintln!(
            "adapter '{}' total {} MiB | PDH used {} MiB, shared {} MiB | nvidia-smi used {smi} MiB | {} processes",
            monitor.adapter().name,
            sample.total_mib,
            sample.used_mib,
            sample.shared_used_mib,
            sample.per_process.len()
        );
        assert!(sample.total_mib > 1024, "DXGI reports dedicated memory");
        assert!(sample.used_mib > 0 && sample.used_mib <= sample.total_mib);
        let diff = sample.used_mib.abs_diff(smi);
        assert!(diff < 1536, "PDH ({}) and nvidia-smi ({smi}) differ by {diff} MiB", sample.used_mib);
    }
}
