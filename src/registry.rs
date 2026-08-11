//! Minimal HKCU registry access.
//!
//! Everything Baton writes lives under HKEY_CURRENT_USER, so no elevation is
//! needed and a mistake can only affect this user. Reads return `None` for a
//! value that does not exist, which is what lets rollback tell "restore the old
//! value" apart from "delete the value I created".

use anyhow::{bail, Result};
use std::ffi::c_void;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn open(subkey: &str, access: REG_SAM_FLAGS) -> Option<Key> {
    let path = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            0,
            access,
            &mut hkey,
        )
    };
    (rc == ERROR_SUCCESS).then_some(Key(hkey))
}

fn create(subkey: &str) -> Result<Key> {
    let path = wide(subkey);
    let mut hkey = HKEY::default();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(path.as_ptr()),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        bail!("could not open HKCU\\{subkey} for writing (code {})", rc.0);
    }
    Ok(Key(hkey))
}

/// `None` means the value is genuinely absent, which rollback treats as
/// "delete this again" rather than "restore zero".
pub fn read_dword(subkey: &str, name: &str) -> Option<u32> {
    let key = open(subkey, KEY_READ)?;
    let value_name = wide(name);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegQueryValueExW(
            key.0,
            PCWSTR(value_name.as_ptr()),
            None,
            None,
            Some(&mut data as *mut u32 as *mut u8),
            Some(&mut size),
        )
    };
    (rc == ERROR_SUCCESS).then_some(data)
}

pub fn write_dword(subkey: &str, name: &str, value: u32) -> Result<()> {
    let key = create(subkey)?;
    let value_name = wide(name);
    let bytes = value.to_ne_bytes();
    let rc = unsafe {
        RegSetValueExW(
            key.0,
            PCWSTR(value_name.as_ptr()),
            0,
            REG_DWORD,
            Some(&bytes),
        )
    };
    if rc != ERROR_SUCCESS {
        bail!("could not write HKCU\\{subkey}\\{name} (code {})", rc.0);
    }
    Ok(())
}

/// Deleting an already-absent value is success: rollback must be idempotent.
pub fn delete_value(subkey: &str, name: &str) -> Result<()> {
    let Some(key) = open(subkey, KEY_WRITE) else {
        return Ok(());
    };
    let value_name = wide(name);
    let rc = unsafe { RegDeleteValueW(key.0, PCWSTR(value_name.as_ptr())) };
    if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
        bail!("could not delete HKCU\\{subkey}\\{name} (code {})", rc.0);
    }
    Ok(())
}

/// Tell the shell that user settings changed. Without this, things like the
/// dark-mode switch sit in the registry doing nothing until you log out.
///
/// Uses a timeout because a wedged top-level window would otherwise hang us.
pub fn broadcast_setting_change() {
    for topic in ["ImmersiveColorSet", "Environment", "Policy"] {
        let param = wide(topic);
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(param.as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                200,
                None,
            );
        }
    }
    let _ = HWND::default();
    let _: *const c_void = std::ptr::null();
}
