//! Choosing which bundled llama.cpp build to use (spec §16.1, D11).
//!
//! Responsibility: the installer ships several backends side by side
//! (`engine/cuda`, `engine/vulkan`, ...). On first run the app picks CUDA for
//! NVIDIA adapters and Vulkan otherwise.
//! Not responsible for: downloading or updating engines.
//!
//! Implements NFR-7.

use std::path::{Path, PathBuf};

use crate::gpu::AdapterInfo;

/// PCI vendor id of NVIDIA.
const VENDOR_NVIDIA: u32 = 0x10DE;

/// Backends in order of preference when there is no GPU information.
const FALLBACK_ORDER: [&str; 3] = ["vulkan", "cuda", "cpu"];

/// True if `dir/<backend>` holds a runnable server.
fn has_backend(root: &Path, backend: &str) -> bool {
    root.join(backend).join("llama-server.exe").is_file()
}

/// Picks the bundled backend folder under `root` for the monitored adapter.
///
/// NVIDIA discrete GPUs get CUDA when it is bundled; every other case gets
/// Vulkan (works on NVIDIA, AMD and Intel), then CUDA, then CPU.
pub fn pick_bundled_dir(root: &Path, adapter: Option<&AdapterInfo>) -> Option<PathBuf> {
    let wants_cuda = adapter.is_some_and(|a| a.vendor_id == VENDOR_NVIDIA && !a.integrated && !a.software);
    if wants_cuda && has_backend(root, "cuda") {
        return Some(root.join("cuda"));
    }
    FALLBACK_ORDER.iter().find(|b| has_backend(root, b)).map(|b| root.join(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(vendor: u32, integrated: bool) -> AdapterInfo {
        AdapterInfo {
            name: "gpu".into(),
            vendor_id: vendor,
            luid: "luid_0x00000000_0x00000001".into(),
            dedicated_mib: 8192,
            software: false,
            integrated,
        }
    }

    fn engines(backends: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        for b in backends {
            std::fs::create_dir_all(dir.path().join(b)).expect("mkdir");
            std::fs::write(dir.path().join(b).join("llama-server.exe"), b"x").expect("write");
        }
        dir
    }

    #[test]
    fn nvidia_gets_cuda_when_bundled() {
        let d = engines(&["vulkan", "cuda"]);
        assert_eq!(pick_bundled_dir(d.path(), Some(&adapter(0x10DE, false))), Some(d.path().join("cuda")));
    }

    #[test]
    fn amd_intel_and_unknown_get_vulkan() {
        let d = engines(&["vulkan", "cuda"]);
        assert_eq!(pick_bundled_dir(d.path(), Some(&adapter(0x1002, false))), Some(d.path().join("vulkan")));
        assert_eq!(pick_bundled_dir(d.path(), Some(&adapter(0x8086, false))), Some(d.path().join("vulkan")));
        assert_eq!(pick_bundled_dir(d.path(), None), Some(d.path().join("vulkan")));
    }

    #[test]
    fn integrated_nvidia_does_not_force_cuda() {
        let d = engines(&["vulkan", "cuda"]);
        assert_eq!(pick_bundled_dir(d.path(), Some(&adapter(0x10DE, true))), Some(d.path().join("vulkan")));
    }

    #[test]
    fn missing_backends_fall_back_and_empty_roots_give_none() {
        let only_cuda = engines(&["cuda"]);
        assert_eq!(
            pick_bundled_dir(only_cuda.path(), Some(&adapter(0x1002, false))),
            Some(only_cuda.path().join("cuda"))
        );
        let none = engines(&[]);
        assert_eq!(pick_bundled_dir(none.path(), None), None);
    }
}
