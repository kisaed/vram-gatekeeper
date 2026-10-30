//! GPU adapter enumeration through DXGI and Direct3D 12 (spec §12.1).
//!
//! Responsibility: list adapters with name, vendor, LUID, total dedicated
//! memory, and whether they share memory with the CPU (integrated GPUs).
//! Not responsible for: choosing an adapter (see `gpu::select_adapter`).
//!
//! Implements FR-VRAM-1.

use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FEATURE_ARCHITECTURE, D3D12_FEATURE_DATA_ARCHITECTURE, D3D12CreateDevice, ID3D12Device,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1};

use crate::error::{CoreError, CoreResult};
use crate::gpu::instance::Luid;
use crate::gpu::{AdapterInfo, BYTES_PER_MIB};

/// Lists every DXGI adapter.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the DXGI factory cannot be created.
pub fn list_adapters() -> CoreResult<Vec<AdapterInfo>> {
    // SAFETY: CreateDXGIFactory1 has no preconditions; it returns a COM
    // object that is released when dropped.
    // https://learn.microsoft.com/windows/win32/api/dxgi/nf-dxgi-createdxgifactory1
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| CoreError::Platform(format!("CreateDXGIFactory1 failed: {e}")))?;

    let mut out = Vec::new();
    let mut index = 0;
    // SAFETY: EnumAdapters1 is safe to call with increasing indices; it fails
    // with DXGI_ERROR_NOT_FOUND after the last adapter, which ends the loop.
    // https://learn.microsoft.com/windows/win32/api/dxgi/nf-dxgi-idxgifactory1-enumadapters1
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(index) } {
        index += 1;
        // SAFETY: `adapter` is a live COM object; GetDesc1 fills a plain struct.
        // https://learn.microsoft.com/windows/win32/api/dxgi/nf-dxgi-idxgiadapter1-getdesc1
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else { continue };
        let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        let dedicated_mib = desc.DedicatedVideoMemory as u64 / BYTES_PER_MIB;
        out.push(AdapterInfo {
            name: String::from_utf16_lossy(&desc.Description[..len]),
            vendor_id: desc.VendorId,
            // LUID.HighPart is signed in Win32; counters print its bit pattern.
            luid: Luid { high: desc.AdapterLuid.HighPart as u32, low: desc.AdapterLuid.LowPart }.to_instance_string(),
            dedicated_mib,
            software,
            integrated: !software && is_uma(&adapter).unwrap_or(dedicated_mib < INTEGRATED_FALLBACK_MIB),
        });
    }
    Ok(out)
}

/// If Direct3D 12 is unavailable, an adapter with less dedicated memory than
/// this is assumed to be integrated. Only used as a fallback (spec §12.1
/// prefers the UMA flag because some APUs reserve several GB).
const INTEGRATED_FALLBACK_MIB: u64 = 512;

/// Asks Direct3D 12 whether the adapter uses unified memory (an integrated GPU).
fn is_uma(adapter: &IDXGIAdapter1) -> Option<bool> {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: `adapter` is a live COM object and `device` a valid out-slot.
    // Failure (no D3D12 support) is handled by returning `None`.
    // https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-d3d12createdevice
    unsafe { D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }.ok()?;
    let device = device?;

    let mut arch = D3D12_FEATURE_DATA_ARCHITECTURE::default();
    // SAFETY: `arch` is a D3D12_FEATURE_DATA_ARCHITECTURE and the size passed
    // is exactly its size, as CheckFeatureSupport requires for this feature.
    // https://learn.microsoft.com/windows/win32/api/d3d12/nf-d3d12-id3d12device-checkfeaturesupport
    unsafe {
        device
            .CheckFeatureSupport(
                D3D12_FEATURE_ARCHITECTURE,
                std::ptr::from_mut(&mut arch).cast(),
                u32::try_from(std::mem::size_of::<D3D12_FEATURE_DATA_ARCHITECTURE>()).ok()?,
            )
            .ok()?;
    }
    Some(arch.UMA.as_bool())
}
