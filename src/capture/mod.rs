pub mod d3d11_device;
pub mod dxgi_duplicator;

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

pub use d3d11_device::D3D11Context;
pub use dxgi_duplicator::{CaptureStatus, DxgiDuplicator};

/// A GPU frame captured via DXGI Desktop Duplication in VRAM.
#[derive(Clone)]
pub struct CapturedFrame {
    /// Direct3D 11 2D texture containing the desktop frame (B8G8R8A8_UNORM) in VRAM.
    pub texture: ID3D11Texture2D,
    /// Absolute QueryPerformanceCounter timestamp when the frame was acquired.
    pub qpc_timestamp: i64,
    /// Timestamp converted to Hundred-Nanosecond (HNS) units.
    pub hns_timestamp: i64,
    /// Display width in pixels.
    pub width: u32,
    /// Display height in pixels.
    pub height: u32,
}
