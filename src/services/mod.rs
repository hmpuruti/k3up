//! Services mode: on Windows, each workload is its own Windows service, run by k3up-host and
//! managed through the service manager, with no central agent.
pub mod files;
pub mod status;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod scm;
#[cfg(windows)]
pub mod setup;

#[cfg(windows)]
pub use backend::{Backend, DEFAULT_PREFIX, Settings};

use anyhow::Result;
use std::path::Path;

/// Access rights that let an account change a file or folder, or what it contains:
/// write data, append data, write extended attributes, delete children, write attributes,
/// delete, change permissions, take ownership, and the generic write and all rights.
const CHANGE_RIGHTS: u32 =
    0x2 | 0x4 | 0x10 | 0x40 | 0x100 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x4000_0000 | 0x1000_0000;

pub fn grants_change(mask: u32) -> bool {
    mask & CHANGE_RIGHTS != 0
}

/// Whether `data` is in services mode. A marker in a folder that someone other than SYSTEM
/// and Administrators controls is an error: that account could read the definitions written
/// there and forge the state read from it. Always false outside Windows.
pub fn check(data: &Path) -> Result<bool> {
    #[cfg(windows)]
    {
        let marker = data.join(files::MARKER);
        if std::fs::symlink_metadata(&marker).is_err() {
            return Ok(false);
        }
        if !crate::win32::is_trusted_machine_dir(data) {
            anyhow::bail!(
                "{} is not protected; services mode refuses it. Remove it and run `k3up services enable` from an elevated terminal",
                data.display()
            );
        }
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let _ = data;
        Ok(false)
    }
}

/// Whether `data` is in services mode and protected.
pub fn active(data: &Path) -> bool {
    matches!(check(data), Ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_and_execute_rights_are_not_changes() {
        // FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, as granted to Users.
        assert!(!grants_change(0x1200a9));
        // FILE_ALL_ACCESS, Modify, write data, delete, WRITE_DAC, GENERIC_WRITE.
        for mask in [0x1f01ff, 0x1301bf, 0x2, 0x1_0000, 0x4_0000, 0x4000_0000] {
            assert!(grants_change(mask), "{mask:#x}");
        }
    }

    #[test]
    fn a_marker_never_turns_on_services_mode_outside_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(files::MARKER), b"").unwrap();
        if cfg!(not(windows)) {
            assert!(!check(dir.path()).unwrap());
            assert!(!active(dir.path()));
        }
    }
}
