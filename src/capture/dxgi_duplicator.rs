use anyhow::{anyhow, Result};
use log::{info, warn};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Resource, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};

use crate::capture::d3d11_device::D3D11Context;
use crate::capture::CapturedFrame;
use crate::platform::time::QpcTimer;

/// Status result from an AcquireNextFrame cycle.
pub enum CaptureStatus {
    /// New frame acquired and copied to VRAM buffer.
    Frame(CapturedFrame),
    /// No desktop changes during the specified timeout.
    Timeout,
    /// Desktop access lost (UAC, mode switch, display change). Duplicator re-initialization needed.
    AccessLost,
    /// Direct3D 11 device removed (GPU driver reset/crash). Full re-initialization needed.
    DeviceRemoved,
}

/// DXGI Desktop Duplication capture engine.
pub struct DxgiDuplicator {
    context: D3D11Context,
    output_index: u32,
    timer: QpcTimer,
    duplicator: Option<IDXGIOutputDuplication>,
    textures: Option<[ID3D11Texture2D; 2]>,
    active_texture_idx: usize,
    width: u32,
    height: u32,
    offset_x: i32,
    offset_y: i32,
    refresh_rate_numerator: u32,
    refresh_rate_denominator: u32,
}

impl DxgiDuplicator {
    /// Creates a new DxgiDuplicator for the specified output index (0 = primary display).
    pub fn new(context: D3D11Context, output_index: u32, timer: QpcTimer) -> Result<Self> {
        let mut duplicator = Self {
            context,
            output_index,
            timer,
            duplicator: None,
            textures: None,
            active_texture_idx: 0,
            width: 0,
            height: 0,
            offset_x: 0,
            offset_y: 0,
            refresh_rate_numerator: 60,
            refresh_rate_denominator: 1,
        };

        duplicator.init_duplication()?;
        Ok(duplicator)
    }

    /// Initializes or re-initializes the IDXGIOutputDuplication instance and texture cache.
    pub fn init_duplication(&mut self) -> Result<()> {
        D3D11Context::ensure_interactive_desktop();

        // Release previous duplicator if any
        self.duplicator = None;

        let adapter = self.context.adapter();
        let output: IDXGIOutput = unsafe {
            adapter
                .EnumOutputs(self.output_index)
                .map_err(|e| anyhow!("Failed to enumerate output {}: {}", self.output_index, e))?
        };

        let desc = unsafe {
            output
                .GetDesc()
                .map_err(|e| anyhow!("Failed to get output desc: {}", e))?
        };

        let width = (desc.DesktopCoordinates.right - desc.DesktopCoordinates.left).unsigned_abs();
        let height = (desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top).unsigned_abs();

        if width == 0 || height == 0 {
            return Err(anyhow!("Invalid display dimensions: {}x{}", width, height));
        }

        self.width = width;
        self.height = height;
        self.offset_x = desc.DesktopCoordinates.left;
        self.offset_y = desc.DesktopCoordinates.top;

        let output1: IDXGIOutput1 = output
            .cast()
            .map_err(|e| anyhow!("Failed to cast IDXGIOutput to IDXGIOutput1: {}", e))?;

        let duplicator: IDXGIOutputDuplication = unsafe {
            output1
                .DuplicateOutput(self.context.device())
                .map_err(|e| anyhow!("Failed to duplicate output {}: {}", self.output_index, e))?
        };

        info!(
            "Initialized DXGI Desktop Duplication on display {} ({}x{})",
            self.output_index, width, height
        );

        // Allocate double-buffered VRAM textures
        self.textures = Some([
            self.create_gpu_texture(width, height)?,
            self.create_gpu_texture(width, height)?,
        ]);
        self.active_texture_idx = 0;
        self.duplicator = Some(duplicator);

        Ok(())
    }

    /// Allocates an internal Direct3D 11 texture in VRAM for zero-copy frame caching.
    fn create_gpu_texture(&self, width: u32, height: u32) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: windows::Win32::Graphics::Direct3D11::D3D11_RESOURCE_MISC_GDI_COMPATIBLE.0 as u32,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe {
            self.context
                .device()
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(|e| anyhow!("Failed to create GPU render texture: {}", e))?;
        }

        texture.ok_or_else(|| anyhow!("CreateTexture2D returned null pointer"))
    }

    /// Captures the next desktop frame with the specified timeout in milliseconds.
    pub fn acquire_frame(&mut self, timeout_ms: u32) -> Result<CaptureStatus> {
        let duplicator = match self.duplicator.as_ref() {
            Some(d) => d,
            None => return Ok(CaptureStatus::AccessLost),
        };

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource: Option<windows::Win32::Graphics::Dxgi::IDXGIResource> = None;

        let acquire_res = unsafe {
            duplicator.AcquireNextFrame(timeout_ms, &mut frame_info, &mut desktop_resource)
        };

        match acquire_res {
            Ok(()) => {
                let qpc_now = QpcTimer::now_qpc();
                let hns_now = self.timer.qpc_to_hns(qpc_now);

                let resource = desktop_resource.ok_or_else(|| {
                    anyhow!("AcquireNextFrame succeeded but returned no resource")
                })?;

                let src_texture: ID3D11Texture2D = resource
                    .cast()
                    .map_err(|e| anyhow!("Failed to cast resource to ID3D11Texture2D: {}", e))?;

                // Copy GPU texture to current double-buffered target
                let textures = self.textures.as_ref().unwrap();
                let dst_texture = &textures[self.active_texture_idx];

                unsafe {
                    let dst_res: ID3D11Resource = dst_texture.cast()?;
                    let src_res: ID3D11Resource = src_texture.cast()?;
                    self.context.context().CopyResource(&dst_res, &src_res);
                }

                // Immediately release frame to avoid locking the desktop pipeline
                let release_res = unsafe { duplicator.ReleaseFrame() };
                if let Err(e) = release_res {
                    warn!("ReleaseFrame returned error: {}", e);
                }

                // Render hardware mouse cursor directly onto the captured VRAM frame
                if let Ok(surface) = dst_texture.cast::<windows::Win32::Graphics::Dxgi::IDXGISurface1>() {
                    if let Ok(hdc) = unsafe { surface.GetDC(false) } {
                        unsafe {
                            use windows::Win32::UI::WindowsAndMessaging::{
                                DrawIconEx, GetCursorInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL,
                            };
                            let mut ci = CURSORINFO {
                                cbSize: std::mem::size_of::<CURSORINFO>() as u32,
                                ..Default::default()
                            };
                            if GetCursorInfo(&mut ci).is_ok() && (ci.flags.0 & CURSOR_SHOWING.0) != 0 {
                                let x = ci.ptScreenPos.x - self.offset_x;
                                let y = ci.ptScreenPos.y - self.offset_y;
                                let _ = DrawIconEx(hdc, x, y, ci.hCursor, 0, 0, 0, None, DI_NORMAL);
                            }
                            let _ = surface.ReleaseDC(None);
                        }
                    }
                }

                let captured = CapturedFrame {
                    texture: dst_texture.clone(),
                    qpc_timestamp: qpc_now,
                    hns_timestamp: hns_now,
                    width: self.width,
                    height: self.height,
                };

                // Flip double-buffer index
                self.active_texture_idx = (self.active_texture_idx + 1) % 2;

                Ok(CaptureStatus::Frame(captured))
            }
            Err(e) => {
                let err_code = e.code();
                if err_code == DXGI_ERROR_WAIT_TIMEOUT {
                    Ok(CaptureStatus::Timeout)
                } else if err_code == DXGI_ERROR_ACCESS_LOST {
                    warn!("DXGI Desktop Duplication access lost (mode switch or secure desktop)");
                    self.duplicator = None;
                    Ok(CaptureStatus::AccessLost)
                } else if err_code == DXGI_ERROR_DEVICE_REMOVED {
                    warn!("DXGI Direct3D 11 device removed");
                    self.duplicator = None;
                    Ok(CaptureStatus::DeviceRemoved)
                } else {
                    Err(anyhow!("AcquireNextFrame failed with error: {}", e))
                }
            }
        }
    }

    /// Screen width in pixels.
    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Screen height in pixels.
    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Display refresh rate as rational (numerator, denominator), e.g. (60, 1).
    #[inline]
    pub fn refresh_rate(&self) -> (u32, u32) {
        (self.refresh_rate_numerator, self.refresh_rate_denominator)
    }

    /// Reference to the underlying Direct3D 11 context.
    #[inline]
    pub fn d3d11_context(&self) -> &D3D11Context {
        &self.context
    }
}
