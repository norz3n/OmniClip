pub mod d3d11_device;
pub mod dxgi_duplicator;

pub use d3d11_device::D3D11Context;
pub use dxgi_duplicator::{CaptureStatus, DxgiDuplicator};

use std::path::Path;
use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Resource, ID3D11Texture2D, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};

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

/// Saves a captured GPU frame to a lossless 32-bit BMP image on disk.
pub fn save_screenshot_bmp(
    context: &D3D11Context,
    frame: &CapturedFrame,
    destination_path: &Path,
) -> Result<()> {
    if let Some(parent) = destination_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: frame.width,
        Height: frame.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };

    let mut staging_texture: Option<ID3D11Texture2D> = None;
    unsafe {
        context
            .device()
            .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
            .map_err(|e| anyhow!("Failed to create staging texture for screenshot: {}", e))?;
    }
    let staging = staging_texture.ok_or_else(|| anyhow!("Staging texture pointer is null"))?;

    unsafe {
        let dst_res: ID3D11Resource = staging.cast()?;
        let src_res: ID3D11Resource = frame.texture.cast()?;
        context.context().CopyResource(&dst_res, &src_res);
    }

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        let res: ID3D11Resource = staging.cast()?;
        context
            .context()
            .Map(&res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| anyhow!("Failed to map staging texture for screenshot: {}", e))?;
    }

    let width = frame.width;
    let height = frame.height;
    let row_pitch = mapped.RowPitch as usize;
    let data_ptr = mapped.pData as *const u8;

    let image_size = (width * height * 4) as usize;
    let file_size = 14 + 40 + image_size;

    let mut file_bytes = Vec::with_capacity(file_size);

    // BITMAPFILEHEADER (14 bytes)
    file_bytes.extend_from_slice(&0x4D42u16.to_le_bytes()); // 'BM'
    file_bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
    file_bytes.extend_from_slice(&0u16.to_le_bytes()); // Reserved 1
    file_bytes.extend_from_slice(&0u16.to_le_bytes()); // Reserved 2
    file_bytes.extend_from_slice(&54u32.to_le_bytes()); // Offset to pixel data (14 + 40)

    // BITMAPINFOHEADER (40 bytes)
    file_bytes.extend_from_slice(&40u32.to_le_bytes()); // biSize
    file_bytes.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    file_bytes.extend_from_slice(&(-(height as i32)).to_le_bytes()); // biHeight (negative = top-down)
    file_bytes.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    file_bytes.extend_from_slice(&32u16.to_le_bytes()); // biBitCount (32-bit BGRA)
    file_bytes.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
    file_bytes.extend_from_slice(&(image_size as u32).to_le_bytes()); // biSizeImage
    file_bytes.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    file_bytes.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    file_bytes.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    file_bytes.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    // Copy rows from mapped memory into file_bytes
    for row in 0..height as usize {
        let row_offset = row * row_pitch;
        let row_slice = unsafe { std::slice::from_raw_parts(data_ptr.add(row_offset), (width * 4) as usize) };
        file_bytes.extend_from_slice(row_slice);
    }

    unsafe {
        let res: ID3D11Resource = staging.cast()?;
        context.context().Unmap(&res, 0);
    }

    std::fs::write(destination_path, file_bytes)
        .map_err(|e| anyhow!("Failed to write screenshot file to '{}': {}", destination_path.display(), e))?;

    log::info!("Saved screenshot to '{}'", destination_path.display());
    Ok(())
}

