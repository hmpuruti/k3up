//! Services mode: on Windows, each workload is its own Windows service, run by k3up-host and
//! managed through the service manager, with no central agent.
pub mod exposure;
pub mod files;
pub mod integrity;
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

/// How the names of workload services start, unless tests set their own.
pub const DEFAULT_SERVICE_PREFIX: &str = "K3Up_";

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

/// Rights that let an account read a file's contents: read data, and the generic read and
/// all rights.
const READ_RIGHTS: u32 = 0x1 | 0x8000_0000 | 0x1000_0000;

/// The one rule every check below applies: an entry that applies to the object itself
/// (not inherit-only) and grants any of `rights` must name SYSTEM, Administrators or
/// TrustedInstaller. That includes CREATOR OWNER: on the object itself it stands for no one,
/// and an entry for it is only expected as an inherit-only template for new children.
/// Entries K3 Up cannot read, such as conditional ones, are refused unless inherit-only.
fn only_trusted_granted(aces: &[Ace], rights: u32) -> bool {
    aces.iter().all(|ace| match *ace {
        Ace::Deny
        | Ace::Allow {
            inherit_only: true, ..
        }
        | Ace::Unknown { inherit_only: true } => true,
        Ace::Unknown { .. } => false,
        Ace::Allow {
            principal, mask, ..
        } => principal == Principal::Trusted || mask & rights == 0,
    })
}

/// Whether no untrusted account may rename, delete or re-permission a folder above the data
/// directory, or what it holds. Creating new entries is fine: nobody can rename or delete an
/// entry they do not own and were not granted.
pub fn keeps_children_in_place(aces: &[Ace]) -> bool {
    only_trusted_granted(aces, MOVE_RIGHTS)
}

/// Whether only trusted principals may write to, delete or re-permission an object.
pub fn only_trusted_may_change(aces: &[Ace]) -> bool {
    only_trusted_granted(aces, CHANGE_RIGHTS)
}

/// Whether only trusted principals may read an object's contents, for private files.
pub fn only_trusted_may_read(aces: &[Ace]) -> bool {
    only_trusted_granted(aces, READ_RIGHTS)
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

/// Where `services enable` records, for the whole machine, the one data directory services
/// mode uses. Service names are machine-wide, so a second directory would fight over them.
pub const REGISTRY_KEY: &str = r"SOFTWARE\K3 Up";
pub const REGISTRY_VALUE: &str = "ServicesData";

/// The data directory services mode is on for, as recorded for the whole machine.
pub fn registered() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        crate::win32::registry_string(
            crate::win32::Hive::LocalMachine,
            REGISTRY_KEY,
            REGISTRY_VALUE,
        )
        .filter(|path| !path.is_empty())
        .map(Into::into)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Whether services mode is on anywhere on this machine.
pub fn machine_mode_on() -> bool {
    in_use().is_some()
}

/// The data directory services mode is on for: the one recorded in the registry, or, for an
/// installation set up before that record existed, the default machine directory when it
/// holds a trusted marker.
pub fn in_use() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let default = crate::platform::machine_data_dir();
        let marked = active(&default);
        current(registered(), &default, marked)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub fn current(
    registered: Option<std::path::PathBuf>,
    default: &Path,
    default_marked: bool,
) -> Option<std::path::PathBuf> {
    registered.or_else(|| default_marked.then(|| default.to_path_buf()))
}

/// Refuses the legacy central agent service while workloads run as their own services.
pub fn refuse_central_agent(services_mode: bool, workload_services: bool) -> Result<()> {
    if services_mode || workload_services {
        anyhow::bail!(
            "Services mode is on; the central agent service can't be installed alongside it"
        );
    }
    Ok(())
}

/// Refuses services mode for `requested` while it is on for another directory.
pub fn check_registration(registered: Option<&Path>, requested: &Path) -> Result<()> {
    match registered {
        Some(registered) if !same_path(registered, requested) => anyhow::bail!(
            "Services mode already uses {}. Run `k3up services disable` there first.",
            registered.display()
        ),
        _ => Ok(()),
    }
}

/// Windows paths ignore case and a trailing separator.
fn same_path(a: &Path, b: &Path) -> bool {
    let plain = |path: &Path| {
        std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    plain(a) == plain(b)
}

/// The two records that turn services mode on for a directory: the machine-wide registry
/// value, through which ordinary commands find it, and the marker inside it.
pub trait Records {
    fn register(&mut self) -> Result<()>;
    fn unregister(&mut self) -> Result<()>;
    fn mark(&mut self) -> Result<()>;
    fn unmark(&mut self) -> Result<()>;
}

/// Writes the registry value before the marker, so that a marked directory is always one
/// ordinary commands find. When the marker fails, the value this call added is taken back. A
/// directory that was already registered keeps its value.
pub fn record(records: &mut impl Records, registered: bool) -> Result<()> {
    if !registered {
        records.register()?;
    }
    let Err(error) = records.mark() else {
        return Ok(());
    };
    if registered {
        return Err(error);
    }
    match records.unregister() {
        Ok(()) => Err(error),
        Err(undo) => Err(anyhow::anyhow!(
            "{error:#}\nRemoving the services mode record from the registry failed too: {undo:#}"
        )),
    }
}

/// Removes the registry value before the marker, the reverse of `record`, so that a failure
/// never leaves a registered directory that ordinary commands no longer recognise. When the
/// marker cannot be removed, the value is put back.
pub fn unrecord(records: &mut impl Records, registered: bool) -> Result<()> {
    if registered {
        records.unregister()?;
    }
    let Err(error) = records.unmark() else {
        return Ok(());
    };
    if !registered {
        return Err(error);
    }
    match records.register() {
        Ok(()) => Err(error),
        Err(undo) => Err(anyhow::anyhow!(
            "{error:#}\nPutting the services mode record back in the registry failed too: {undo:#}"
        )),
    }
}

/// Whether `data` is in services mode and protected.
pub fn active(data: &Path) -> bool {
    matches!(check(data), Ok(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        // Full control for Users on new children only does not let them change this object.
        assert!(only_trusted_may_change(&[allow(
            Principal::Other,
            0x1f01ff,
            true
        )]));
        for refused in [
            allow(Principal::Other, 0x1301bf, false),
            allow(Principal::Other, 0x4_0000, false),
            // On the object itself CREATOR OWNER is accepted only as an inherit-only template.
            allow(Principal::CreatorOwner, 0x1f01ff, false),
            Ace::Unknown {
                inherit_only: false,
            },
        ] {
            assert!(!only_trusted_may_change(&[refused]), "{refused:?}");
        }
        assert!(only_trusted_may_change(&[Ace::Unknown {
            inherit_only: true
        }]));
        assert!(only_trusted_may_change(&[allow(
            Principal::CreatorOwner,
            0x1200a9,
            false
        )]));
    }

    #[test]
    fn private_files_may_be_read_only_by_trusted_principals() {
        let allow = |principal, mask, inherit_only| Ace::Allow {
            principal,
            mask,
            inherit_only,
        };
        let private = [
            allow(Principal::Trusted, 0x1f01ff, false),
            allow(Principal::Other, 0x80, false),
            allow(Principal::Other, 0x1200a9, true),
        ];
        assert!(only_trusted_may_read(&private));
        for refused in [
            allow(Principal::Other, 0x1200a9, false),
            allow(Principal::Other, 0x8000_0000, false),
            allow(Principal::Other, 0x1000_0000, false),
            allow(Principal::CreatorOwner, 0x1, false),
        ] {
            assert!(!only_trusted_may_read(&[refused]), "{refused:?}");
        }
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
            for principal in [Principal::Other, Principal::CreatorOwner] {
                assert!(
                    !keeps_children_in_place(&[allow(principal, mask, false)]),
                    "{principal:?} {mask:#x}"
                );
            }
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
    fn services_mode_keeps_to_one_data_directory() {
        let base = std::env::temp_dir();
        let first = base.join("K3 Up");
        check_registration(None, &first).unwrap();
        check_registration(Some(&first), &first).unwrap();
        let same = PathBuf::from(format!(
            "{}{}",
            first.to_string_lossy().to_uppercase(),
            std::path::MAIN_SEPARATOR
        ));
        check_registration(Some(&first), &same).unwrap();
        let error = check_registration(Some(&first), &base.join("Other"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            format!(
                "Services mode already uses {}. Run `k3up services disable` there first.",
                first.display()
            )
        );
        if cfg!(not(windows)) {
            assert!(!machine_mode_on());
            assert_eq!(registered(), None);
        }
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

    #[derive(Default)]
    struct FakeRecords {
        fail: Vec<&'static str>,
        calls: Vec<&'static str>,
    }

    impl FakeRecords {
        fn call(&mut self, name: &'static str) -> Result<()> {
            self.calls.push(name);
            if self.fail.contains(&name) {
                anyhow::bail!("{name} failed");
            }
            Ok(())
        }
    }

    impl Records for FakeRecords {
        fn register(&mut self) -> Result<()> {
            self.call("register")
        }

        fn unregister(&mut self) -> Result<()> {
            self.call("unregister")
        }

        fn mark(&mut self) -> Result<()> {
            self.call("mark")
        }

        fn unmark(&mut self) -> Result<()> {
            self.call("unmark")
        }
    }

    fn recorded(fail: &[&'static str], registered: bool) -> (Vec<&'static str>, Option<String>) {
        let mut fake = FakeRecords {
            fail: fail.to_vec(),
            ..Default::default()
        };
        let result = record(&mut fake, registered);
        (fake.calls, result.err().map(|error| error.to_string()))
    }

    #[test]
    fn the_marker_is_written_only_after_the_registry_value() {
        assert_eq!(recorded(&[], false), (vec!["register", "mark"], None));
        assert_eq!(
            recorded(&["register"], false),
            (vec!["register"], Some("register failed".into()))
        );
        assert_eq!(
            recorded(&["mark"], false),
            (
                vec!["register", "mark", "unregister"],
                Some("mark failed".into())
            )
        );
        assert_eq!(
            recorded(&["mark", "unregister"], false),
            (
                vec!["register", "mark", "unregister"],
                Some(
                    "mark failed\nRemoving the services mode record from the registry failed too: unregister failed"
                        .into()
                )
            )
        );
    }

    #[test]
    fn a_registered_directory_keeps_its_registry_value() {
        assert_eq!(recorded(&[], true), (vec!["mark"], None));
        assert_eq!(
            recorded(&["mark"], true),
            (vec!["mark"], Some("mark failed".into()))
        );
    }

    fn unrecorded(fail: &[&'static str], registered: bool) -> (Vec<&'static str>, Option<String>) {
        let mut fake = FakeRecords {
            fail: fail.to_vec(),
            ..Default::default()
        };
        let result = unrecord(&mut fake, registered);
        (fake.calls, result.err().map(|error| error.to_string()))
    }

    #[test]
    fn the_registry_value_is_removed_before_the_marker() {
        assert_eq!(unrecorded(&[], true), (vec!["unregister", "unmark"], None));
        assert_eq!(
            unrecorded(&["unregister"], true),
            (vec!["unregister"], Some("unregister failed".into()))
        );
        assert_eq!(
            unrecorded(&["unmark"], true),
            (
                vec!["unregister", "unmark", "register"],
                Some("unmark failed".into())
            )
        );
        assert_eq!(
            unrecorded(&["unmark", "register"], true),
            (
                vec!["unregister", "unmark", "register"],
                Some(
                    "unmark failed\nPutting the services mode record back in the registry failed too: register failed"
                        .into()
                )
            )
        );
    }

    #[test]
    fn a_directory_without_a_registry_value_only_loses_its_marker() {
        assert_eq!(unrecorded(&[], false), (vec!["unmark"], None));
        assert_eq!(
            unrecorded(&["unmark"], false),
            (vec!["unmark"], Some("unmark failed".into()))
        );
    }

    #[test]
    fn a_marked_default_directory_counts_as_registered() {
        let base = std::env::temp_dir();
        let default = base.join("K3 Up");
        let other = base.join("Other");
        assert_eq!(current(None, &default, false), None);
        assert_eq!(current(None, &default, true), Some(default.clone()));
        assert_eq!(
            current(Some(other.clone()), &default, true),
            Some(other.clone())
        );

        let legacy = current(None, &default, true);
        let registered = current(Some(default.clone()), &default, false);
        let refused = |registration: &Option<PathBuf>| {
            check_registration(registration.as_deref(), &other)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(refused(&legacy), refused(&registered));
        check_registration(legacy.as_deref(), &default).unwrap();
        // Enabling the default directory then writes the registry value, which migrates it.
        assert_eq!(recorded(&[], false), (vec!["register", "mark"], None));
    }

    #[test]
    fn the_central_agent_service_is_refused_in_services_mode() {
        refuse_central_agent(false, false).unwrap();
        for (services_mode, workload_services) in [(true, false), (false, true), (true, true)] {
            assert_eq!(
                refuse_central_agent(services_mode, workload_services)
                    .unwrap_err()
                    .to_string(),
                "Services mode is on; the central agent service can't be installed alongside it"
            );
        }
        if cfg!(not(windows)) {
            assert_eq!(in_use(), None);
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
