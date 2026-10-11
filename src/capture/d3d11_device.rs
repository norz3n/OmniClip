use anyhow::{anyhow, Result};
use log::info;
use windows::core::{w, Interface};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1};
use windows::Win32::System::StationsAndDesktops::{
    OpenDesktopW, SetThreadDesktop, DESKTOP_CONTROL_FLAGS,
};

/// Direct3D 11 device and immediate context wrapper configured for GPU video processing.
#[derive(Clone)]
pub struct D3D11Context {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    feature_level: D3D_FEATURE_LEVEL,
    adapter: IDXGIAdapter,
}

impl D3D11Context {
    /// Ensures that the current calling thread is attached to the interactive 'Default' desktop.
    ///
    /// Required for DXGI Desktop Duplication to succeed in sandbox or service environments.
    pub fn ensure_interactive_desktop() {
        unsafe {
            if let Ok(default_desk) = OpenDesktopW(w!("Default"), DESKTOP_CONTROL_FLAGS(0), false, 0x01FF) {
                let _ = SetThreadDesktop(default_desk);
            }
        }
    }

    /// Creates a hardware Direct3D 11 device bound to the primary adapter with BGRA and Video support.
    pub fn new() -> Result<Self> {
        Self::new_on_adapter(0)
    }

    /// Creates a hardware Direct3D 11 device on a specific adapter index.
    pub fn new_on_adapter(adapter_index: u32) -> Result<Self> {
        Self::ensure_interactive_desktop();

        let factory: IDXGIFactory1 = unsafe {
            CreateDXGIFactory1()
                .map_err(|e| anyhow!("Failed to create DXGI factory: {}", e))?
        };

        let adapter1: IDXGIAdapter1 = unsafe {
            factory
                .EnumAdapters1(adapter_index)
                .map_err(|e| anyhow!("Failed to enumerate adapter {}: {}", adapter_index, e))?
        };

        let adapter: IDXGIAdapter = adapter1
            .cast()
            .map_err(|e| anyhow!("Failed to cast IDXGIAdapter1 to IDXGIAdapter: {}", e))?;

        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;

        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let mut feature_level = D3D_FEATURE_LEVEL_11_0;

        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE(std::ptr::null_mut()),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut feature_level),
                Some(&mut context),
            )
            .map_err(|e| anyhow!("Failed to create D3D11 hardware device on adapter {}: {}", adapter_index, e))?;
        }

        let device = device.ok_or_else(|| anyhow!("D3D11 device pointer was null"))?;
        let context = context.ok_or_else(|| anyhow!("D3D11 context pointer was null"))?;

        info!(
            "Created Direct3D 11 hardware device on adapter {} (Feature Level: 0x{:X})",
            adapter_index, feature_level.0
        );

        Ok(Self {
            device,
            context,
            feature_level,
            adapter,
        })
    }

    /// Returns reference to the ID3D11Device.
    #[inline]
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// Returns reference to the ID3D11DeviceContext.
    #[inline]
    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Returns the active Direct3D feature level.
    #[inline]
    pub fn feature_level(&self) -> D3D_FEATURE_LEVEL {
        self.feature_level
    }

    /// Returns reference to the underlying IDXGIAdapter.
    #[inline]
    pub fn adapter(&self) -> &IDXGIAdapter {
        &self.adapter
    }

    /// Obtains the underlying IDXGIDevice interface.
    pub fn dxgi_device(&self) -> Result<IDXGIDevice> {
        self.device
            .cast::<IDXGIDevice>()
            .map_err(|e| anyhow!("Failed to query IDXGIDevice from D3D11Device: {}", e))
    }
}
