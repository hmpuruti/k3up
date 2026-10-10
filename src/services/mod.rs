//! Services mode: on Windows, each workload is its own Windows service, run by k3up-host and
//! managed through the service manager, with no central agent.
pub mod files;
pub mod status;

#[cfg(windows)]
mod backend;
#[cfg(windows)]
pub mod host;
#[cfg(any(windows, test))]
mod pause;
#[cfg(windows)]
pub mod scm;
#[cfg(windows)]
pub mod setup;

#[cfg(windows)]
pub use backend::{Backend, DEFAULT_PREFIX, Settings};

use crate::autostart::Registration;
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

/// Whom an access control entry names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    /// SYSTEM, Administrators or TrustedInstaller.
    Trusted,
    /// CREATOR OWNER, which stands for the object's owner.
    CreatorOwner,
    Other,
}

/// An access control entry, reduced to what decides whether it lets someone change the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ace {
    Allow {
        principal: Principal,
        mask: u32,
        /// Applies only to new children, not to the object itself.
        inherit_only: bool,
    },
    Deny,
    /// Conditional, object and other entries, which K3 Up never writes.
    Unknown {
        inherit_only: bool,
    },
}

/// Rights over a folder that let an account move what it holds or the folder itself: delete
/// a child, delete, change permissions, take ownership, and generic all.
const MOVE_RIGHTS: u32 = 0x40 | 0x1_0000 | 0x4_0000 | 0x8_0000 | 0x1000_0000;

/// Whether no untrusted account may rename, delete or re-permission a folder above the data
/// directory, or what it holds, given that its owner is trusted. Creating new entries is
/// fine: nobody can rename or delete an entry they do not own and were not granted.
pub fn keeps_children_in_place(aces: &[Ace]) -> bool {
    aces.iter().all(|ace| match *ace {
        Ace::Deny
        | Ace::Allow {
            inherit_only: true, ..
        }
        | Ace::Unknown { inherit_only: true } => true,
        Ace::Unknown { .. } => false,
        Ace::Allow {
            principal, mask, ..
        } => principal != Principal::Other || mask & MOVE_RIGHTS == 0,
    })
}

/// Whether only trusted principals may change an object with these entries, given that its
/// owner is trusted, which also covers CREATOR OWNER. Inherit-only entries are skipped: they
/// reach new children only, and whoever creates one needs a grant that already counts here.
pub fn only_trusted_may_change(aces: &[Ace]) -> bool {
    aces.iter().all(|ace| match *ace {
        Ace::Deny
        | Ace::Allow {
            inherit_only: true, ..
        }
        | Ace::Unknown { inherit_only: true } => true,
        Ace::Unknown { .. } => false,
        Ace::Allow {
            principal, mask, ..
        } => principal != Principal::Other || !grants_change(mask),
    })
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

/// Refuses to turn services mode on while this account's own agent is set to start at login
/// or is running. Its workloads would keep running beside the new services, and once the
/// default data directory moves to the machine's, the agent commands no longer reach it.
pub fn refuse_user_agent(registration: Registration, running: bool) -> Result<()> {
    let state = match (registration != Registration::None, running) {
        (false, false) => return Ok(()),
        (true, true) => "starts at login and is running",
        (true, false) => "starts at login",
        (false, true) => "is running",
    };
    anyhow::bail!(
        "Your own K3 Up agent {state}, so its workloads would run beside the services. Move them first:\n  k3up export --output workloads.toml\n  k3up agent uninstall\n  k3up services enable\n  k3up apply workloads.toml"
    )
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
    fn only_trusted_principals_may_change_a_protected_object() {
        let allow = |principal, mask, inherit_only| Ace::Allow {
            principal,
            mask,
            inherit_only,
        };
        let program_files = [
            allow(Principal::Trusted, 0x1f01ff, false),
            allow(Principal::Trusted, 0x1000_0000, true),
            allow(Principal::CreatorOwner, 0x1000_0000, true),
            allow(Principal::Other, 0x1200a9, false),
            allow(Principal::Other, 0xa000_0000, true),
            Ace::Deny,
        ];
        assert!(only_trusted_may_change(&program_files));
        // CREATOR OWNER stands for the owner, which the caller requires to be trusted.
        assert!(only_trusted_may_change(&[allow(
            Principal::CreatorOwner,
            0x1f01ff,
            false
        )]));
        // Full control for Users on new children only does not let them change this object.
        assert!(only_trusted_may_change(&[allow(
            Principal::Other,
            0x1f01ff,
            true
        )]));
        for refused in [
            allow(Principal::Other, 0x1301bf, false),
            allow(Principal::Other, 0x4_0000, false),
            Ace::Unknown {
                inherit_only: false,
            },
        ] {
            assert!(!only_trusted_may_change(&[refused]), "{refused:?}");
        }
        assert!(only_trusted_may_change(&[Ace::Unknown {
            inherit_only: true
        }]));
    }

    #[test]
    fn folders_above_the_data_directory_must_keep_it_in_place() {
        let allow = |principal, mask, inherit_only| Ace::Allow {
            principal,
            mask,
            inherit_only,
        };
        // C:\ProgramData: users may read and create entries, CREATOR OWNER gets full control
        // of what they create.
        let program_data = [
            allow(Principal::Trusted, 0x1f01ff, false),
            allow(Principal::Other, 0x1200a9, false),
            allow(Principal::Other, 0x116, false),
            allow(Principal::CreatorOwner, 0x1000_0000, true),
        ];
        assert!(keeps_children_in_place(&program_data));
        assert!(keeps_children_in_place(&[allow(
            Principal::Other,
            0x1f01ff,
            true
        )]));
        for mask in [
            0x1f01ff,
            0x1301bf,
            0x40,
            0x1_0000,
            0x4_0000,
            0x8_0000,
            0x1000_0000,
        ] {
            assert!(
                !keeps_children_in_place(&[allow(Principal::Other, mask, false)]),
                "{mask:#x}"
            );
            assert!(keeps_children_in_place(&[allow(
                Principal::Trusted,
                mask,
                false
            )]));
        }
        assert!(!keeps_children_in_place(&[Ace::Unknown {
            inherit_only: false
        }]));
    }

    #[test]
    fn services_mode_waits_for_the_users_own_agent() {
        refuse_user_agent(Registration::None, false).unwrap();
        for (registration, running, state) in [
            (Registration::ThisDirectory, false, "starts at login"),
            (Registration::OtherDirectory, false, "starts at login"),
            (Registration::None, true, "is running"),
            (
                Registration::ThisDirectory,
                true,
                "starts at login and is running",
            ),
        ] {
            let message = refuse_user_agent(registration, running)
                .unwrap_err()
                .to_string();
            assert!(
                message.starts_with(&format!("Your own K3 Up agent {state}, ")),
                "{message}"
            );
            assert!(
                message.contains("k3up export --output workloads.toml\n  k3up agent uninstall")
            );
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
