use anyhow::{anyhow, Result};
use log::info;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
    MFCreateDXGIDeviceManager, MFShutdown, MFStartup, IMFDXGIDeviceManager, MFSTARTUP_NOSOCKET,
    MF_VERSION,
};

/// RAII platform guard that initializes Media Foundation on creation and shuts down on drop.
pub struct MfPlatformGuard;

impl MfPlatformGuard {
    /// Initializes the Windows Media Foundation platform.
    pub fn init() -> Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)
                .map_err(|e| anyhow!("Failed to initialize Media Foundation platform: {}", e))?;
        }
        info!("Initialized Windows Media Foundation platform");
        Ok(Self)
    }
}

impl Drop for MfPlatformGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
        info!("Shut down Windows Media Foundation platform");
    }
}

/// Direct3D 11 DXGI Device Manager wrapper for Media Foundation GPU hardware acceleration.
#[derive(Clone)]
pub struct DxgiDeviceManager {
    manager: IMFDXGIDeviceManager,
    reset_token: u32,
}

impl DxgiDeviceManager {
    /// Creates and binds a new IMFDXGIDeviceManager to the given Direct3D 11 device.
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        let mut reset_token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;

        unsafe {
            MFCreateDXGIDeviceManager(&mut reset_token, &mut manager)
                .map_err(|e| anyhow!("Failed to create DXGI device manager: {}", e))?;
        }

        let manager = manager.ok_or_else(|| anyhow!("Device manager pointer is null"))?;

        unsafe {
            manager
                .ResetDevice(device, reset_token)
                .map_err(|e| anyhow!("Failed to bind D3D11 device to DXGI device manager: {}", e))?;
        }

        info!("Created IMFDXGIDeviceManager and bound to Direct3D 11 device");
        Ok(Self {
            manager,
            reset_token,
        })
    }

    /// Returns the raw IUnknown pointer address for passing to MFT_MESSAGE_SET_D3D_MANAGER.
    #[inline]
    pub fn as_raw_param(&self) -> usize {
        let unk: windows::core::IUnknown = match self.manager.cast() {
            Ok(u) => u,
            Err(_) => return 0,
        };
        unk.as_raw() as usize
    }

    /// Reference to the underlying IMFDXGIDeviceManager COM interface.
    #[inline]
    pub fn inner(&self) -> &IMFDXGIDeviceManager {
        &self.manager
    }

    /// Reset token for device changes.
    #[inline]
    pub fn reset_token(&self) -> u32 {
        self.reset_token
    }
}
