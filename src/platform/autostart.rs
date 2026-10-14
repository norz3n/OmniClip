use anyhow::{anyhow, Result};
use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY_CURRENT_USER,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

const RUN_KEY_SUBPATH: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP_REG_NAME: PCWSTR = w!("OmniClip");

/// Checks whether OmniClip is currently configured to run on Windows startup in HKCU.
pub fn is_autostart_enabled() -> bool {
    unsafe {
        let mut hkey = Default::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY_SUBPATH, 0, KEY_QUERY_VALUE, &mut hkey).is_err() {
            return false;
        }
        let mut data_size = 0u32;
        let query_res = RegQueryValueExW(
            hkey,
            APP_REG_NAME,
            None,
            None,
            None,
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        query_res.is_ok() && data_size > 0
    }
}

/// Enables or disables OmniClip autostart with Windows in HKCU.
pub fn set_autostart(enable: bool) -> Result<()> {
    unsafe {
        let mut hkey = Default::default();
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY_SUBPATH,
            0,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            &mut hkey,
        )
        .ok()
        .map_err(|e| anyhow!("Failed to open Windows Run registry key: {}", e))?;

        let res = if enable {
            let current_exe = std::env::current_exe()
                .map_err(|e| anyhow!("Failed to resolve current executable path: {}", e))?;
            // Pass argument --minimized so it starts quietly into tray
            let cmd_str = format!("\"{}\" --minimized\0", current_exe.to_string_lossy());
            let wide: Vec<u16> = cmd_str.encode_utf16().collect();
            let byte_slice = std::slice::from_raw_parts(
                wide.as_ptr() as *const u8,
                wide.len() * std::mem::size_of::<u16>(),
            );
            RegSetValueExW(
                hkey,
                APP_REG_NAME,
                0,
                REG_SZ,
                Some(byte_slice),
            )
            .ok()
            .map_err(|e| anyhow!("Failed to set Run registry value: {}", e))
        } else {
            let _ = RegDeleteValueW(hkey, APP_REG_NAME);
            Ok(())
        };

        let _ = RegCloseKey(hkey);
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autostart_check_does_not_panic() {
        // Querying HKCU Run key should execute safely without crashing
        let _ = is_autostart_enabled();
    }
}
