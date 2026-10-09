/// Adds `dir` to a `;`-separated PATH value. `None` when an entry already names it.
pub fn with_entry(path: &str, dir: &str) -> Option<String> {
    if entries(path).any(|entry| same(entry, dir)) {
        return None;
    }
    let path = path.trim_end_matches(';');
    Some(if path.is_empty() {
        dir.to_string()
    } else {
        format!("{path};{dir}")
    })
}

/// Removes every entry naming `dir` and leaves the rest untouched. `None` when absent.
pub fn without_entry(path: &str, dir: &str) -> Option<String> {
    if !entries(path).any(|entry| same(entry, dir)) {
        return None;
    }
    Some(
        path.split(';')
            .filter(|entry| !same(entry, dir))
            .collect::<Vec<_>>()
            .join(";"),
    )
}

fn entries(path: &str) -> impl Iterator<Item = &str> {
    path.split(';').filter(|entry| !entry.trim().is_empty())
}

/// Windows paths compare case-insensitively, and a trailing separator names the same folder.
fn same(entry: &str, dir: &str) -> bool {
    let normal = |value: &str| value.trim().trim_end_matches(['\\', '/']).to_lowercase();
    !entry.trim().is_empty() && normal(entry) == normal(dir)
}

/// Whose PATH to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The current user's PATH.
    User,
    /// The PATH of every user on the machine. Needs an elevated process.
    System,
}

/// Adds `dir` to a PATH. Returns whether anything changed.
#[cfg(windows)]
pub fn add(dir: &std::path::Path, scope: Scope) -> anyhow::Result<bool> {
    update(dir, scope, with_entry)
}

/// Removes `dir` from a PATH. Returns whether anything changed.
#[cfg(windows)]
pub fn remove(dir: &std::path::Path, scope: Scope) -> anyhow::Result<bool> {
    update(dir, scope, without_entry)
}

#[cfg(windows)]
fn update(
    dir: &std::path::Path,
    scope: Scope,
    change: fn(&str, &str) -> Option<String>,
) -> anyhow::Result<bool> {
    let current = registry::read(scope)?;
    let Some(updated) = change(&current, &dir.to_string_lossy()) else {
        return Ok(false);
    };
    registry::write(scope, &updated)?;
    registry::announce();
    Ok(true)
}

#[cfg(windows)]
mod registry {
    use super::Scope;
    use crate::win32::wide;
    use anyhow::{Context, Result};
    use std::{ffi::OsStr, ptr};
    use windows_sys::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_EXPAND_SZ, RRF_NOEXPAND,
            RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW,
        },
        UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        },
    };

    const KEY: &str = "Environment";
    const SYSTEM_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
    const VALUE: &str = "Path";

    fn location(scope: Scope) -> (HKEY, &'static str, &'static str) {
        match scope {
            Scope::User => (HKEY_CURRENT_USER, KEY, "user"),
            Scope::System => (HKEY_LOCAL_MACHINE, SYSTEM_KEY, "system"),
        }
    }

    /// The raw value, with `%VARIABLES%` left unexpanded so they survive the rewrite.
    pub fn read(scope: Scope) -> Result<String> {
        let (root, key, name) = location(scope);
        let key = wide(OsStr::new(key));
        let value = wide(OsStr::new(VALUE));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut bytes = 0u32;
        // SAFETY: NUL-terminated names; a null buffer asks only for the size.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut bytes,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(String::new());
        }
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .with_context(|| format!("Read the {name} PATH"));
        }
        let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
        // SAFETY: the buffer is at least `bytes` long, as reported by the first call.
        let status = unsafe {
            RegGetValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                flags,
                ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .with_context(|| format!("Read the {name} PATH"));
        }
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        Ok(String::from_utf16_lossy(&buffer[..length]))
    }

    pub fn write(scope: Scope, path: &str) -> Result<()> {
        let (root, key, name) = location(scope);
        let key = wide(OsStr::new(key));
        let value = wide(OsStr::new(VALUE));
        let data = wide(OsStr::new(path));
        // SAFETY: all buffers are NUL-terminated and outlive the call.
        let status = unsafe {
            RegSetKeyValueW(
                root,
                key.as_ptr(),
                value.as_ptr(),
                REG_EXPAND_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .with_context(|| format!("Write the {name} PATH"));
        }
        Ok(())
    }

    /// Tells Explorer and other programs to reload the environment, so new terminals see it.
    pub fn announce() {
        let area = wide(OsStr::new(KEY));
        // SAFETY: `area` is NUL-terminated and outlives the call; the timeout bounds a hung window.
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                area.as_ptr() as isize,
                SMTO_ABORTIFHUNG,
                5000,
                ptr::null_mut(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_once_and_keeps_existing_entries() {
        let path = r"C:\Windows;%USERPROFILE%\bin;";
        assert_eq!(
            with_entry(path, r"C:\K3 Up").as_deref(),
            Some(r"C:\Windows;%USERPROFILE%\bin;C:\K3 Up")
        );
        assert_eq!(with_entry(r"C:\k3 up\;C:\Windows", r"C:\K3 Up"), None);
        assert_eq!(with_entry("", r"C:\K3 Up").as_deref(), Some(r"C:\K3 Up"));
    }

    #[test]
    fn removes_only_matching_entries() {
        let path = r"C:\Windows;C:\K3 Up\;%USERPROFILE%\bin";
        assert_eq!(
            without_entry(path, r"c:\k3 up").as_deref(),
            Some(r"C:\Windows;%USERPROFILE%\bin")
        );
        assert_eq!(without_entry(r"C:\Windows", r"C:\K3 Up"), None);
    }
}
