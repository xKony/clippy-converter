//! Start-with-Windows via the current-user Run key.

use anyhow::{Context, Result};
use std::path::Path;

/// Registry value under `HKCU\...\Run`.
const VALUE_NAME: &str = "Clippy Converter";

/// Adds or removes this executable from the current-user Startup list.
///
/// No-op on non-Windows targets.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined or the registry write fails.
pub fn set_enabled(enabled: bool) -> Result<()> {
    #[cfg(windows)]
    {
        windows_set_enabled(enabled)
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

/// Quoted `REG_SZ` payload for the Run key (`"C:\path\clippy-converter.exe"` plus NUL).
fn run_value_bytes(exe: &Path) -> Vec<u8> {
    let quoted = format!("\"{}\"", exe.display());
    quoted
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn windows_set_enabled(enabled: bool) -> Result<()> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
        RegOpenKeyExW, RegSetValueExW,
    };
    use windows::core::HSTRING;

    let mut hkey = HKEY::default();
    // SAFETY: `phkresult` points to a valid `HKEY` slot; the Run key is a well-known HKCU path.
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            KEY_SET_VALUE,
            std::ptr::from_mut(&mut hkey),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(anyhow::anyhow!(
            "failed to open HKCU Run key (Win32 {status:?})"
        ));
    }

    let value_name = HSTRING::from(VALUE_NAME);
    let result = if enabled {
        let exe = std::env::current_exe().context("Failed to resolve current executable path")?;
        let data = run_value_bytes(&exe);
        // SAFETY: `hkey` is an open key; `data` is a NUL-terminated UTF-16 REG_SZ buffer.
        let status = unsafe { RegSetValueExW(hkey, &value_name, 0, REG_SZ, Some(&data)) };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "failed to write HKCU Run value (Win32 {status:?})"
            ))
        }
    } else {
        // SAFETY: `hkey` is an open key; deleting a missing value is treated as success below.
        let status = unsafe { RegDeleteValueW(hkey, &value_name) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(anyhow::anyhow!(
                "failed to remove HKCU Run value (Win32 {status:?})"
            ))
        }
    };

    // SAFETY: `hkey` was opened above and is not used after this close.
    let _ = unsafe { RegCloseKey(hkey) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_value_bytes_should_quote_path_and_nul_terminate() {
        let bytes = run_value_bytes(&PathBuf::from(r"C:\Program Files\clippy-converter.exe"));
        let (pairs, remainder) = bytes.as_chunks::<2>();
        assert!(remainder.is_empty());
        let u16s: Vec<u16> = pairs.iter().copied().map(u16::from_le_bytes).collect();
        assert_eq!(u16s.last().copied(), Some(0));
        let text = String::from_utf16_lossy(&u16s[..u16s.len() - 1]);
        assert_eq!(text, r#""C:\Program Files\clippy-converter.exe""#);
    }

    #[test]
    fn value_name_is_stable() {
        assert_eq!(VALUE_NAME, "Clippy Converter");
    }
}
