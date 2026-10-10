use crate::services::{Ace, Principal, files::Access};
use anyhow::{Context, Result, bail};
use std::{
    ffi::{OsStr, OsString},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    ptr,
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
            GetSecurityInfo, SDDL_REVISION_1, SE_KERNEL_OBJECT,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid, GetTokenInformation,
        INHERIT_ONLY_ACE, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
        TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenUser,
        UNPROTECTED_DACL_SECURITY_INFORMATION, WinBuiltinAdministratorsSid, WinCreatorOwnerSid,
        WinLocalSystemSid,
    },
    Storage::FileSystem::{
        CreateDirectoryW, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER,
    },
    System::{
        Com::CoTaskMemFree,
        Registry::{
            HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW,
            RegGetValueW, RegSetKeyValueW,
        },
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::Shell::{FOLDERID_ProgramData, FOLDERID_ProgramFiles, SHGetKnownFolderPath},
};

mod pinned;
use pinned::{Pinned, SHARE_ALL, SHARE_NO_DELETE};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
const ACCESS_DENIED_OBJECT_ACE_TYPE: u8 = 6;

/// SYSTEM, Administrators and the pipe's owner only. The default pipe DACL also grants
/// Everyone read access, which is enough to occupy the single listening instance.
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)";
const SERVICE_DIRECTORY_SDDL: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// SYSTEM and Administrators only. Owned by Administrators, so the service host trusts it.
pub const PRIVATE_DIR_SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
/// As private, plus read access for every user.
pub const SHARED_DIR_SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
/// Users may open the folder itself to reach the shared folders inside, and nothing more.
pub const ROOT_DIR_SDDL: &str = "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;;0x1200a9;;;BU)";
/// Every user may read the marker's permissions, which is how clients without administrator
/// rights recognise services mode. The root's grant to users is not inherited, so it is set
/// here explicitly.
pub const MARKER_SDDL: &str = "O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;BU)";
/// The install folder and each installed program: users and app packages may run them, as in
/// Program Files. Set explicitly, because inheriting Program Files' CREATOR OWNER entry would
/// grant the account that created them, rather than Administrators, full control.
pub const INSTALL_DIR_SDDL: &str =
    "O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)(A;OICI;0x1200a9;;;AC)";
pub const PROGRAM_SDDL: &str =
    "O:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;0x1200a9;;;BU)(A;;0x1200a9;;;AC)";

pub fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

pub struct SecurityAttributes {
    inner: SECURITY_ATTRIBUTES,
}

impl SecurityAttributes {
    pub fn from_sddl(sddl: &str) -> Result<Self> {
        let text = wide(OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: text is NUL-terminated; the descriptor is LocalAlloc'd and freed on drop.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(std::io::Error::last_os_error()).context("Build security descriptor");
        }
        Ok(Self {
            inner: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    pub fn as_mut_ptr(&mut self) -> *mut SECURITY_ATTRIBUTES {
        &mut self.inner
    }

    fn descriptor(&self) -> PSECURITY_DESCRIPTOR {
        self.inner.lpSecurityDescriptor
    }
}

impl Drop for SecurityAttributes {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            LocalFree(self.inner.lpSecurityDescriptor);
        }
    }
}

/// Creates a new directory with its protected ACL applied at creation, so no other account
/// can plant files in it first. Fails if the directory already exists.
pub fn create_protected_dir(path: &Path) -> Result<()> {
    create_dir_with(path, SERVICE_DIRECTORY_SDDL)
}

/// Creates a new directory with the security descriptor in `sddl` applied at creation.
pub fn create_dir_with(path: &Path, sddl: &str) -> Result<()> {
    let mut attributes = SecurityAttributes::from_sddl(sddl)?;
    let path_wide = wide(path.as_os_str());
    // SAFETY: both pointers reference live, initialized buffers for the duration of the call.
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), attributes.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Create {}", path.display()));
    }
    Ok(())
}

/// Gives a directory the access in `sddl`, creating it if needed. An existing directory owned
/// by an account other than SYSTEM or Administrators is refused, because that account could
/// have planted files the service host would trust. The checks and the change go through one
/// handle that does not follow links and keeps the directory from being renamed meanwhile.
pub fn secure_dir(path: &Path, sddl: &str) -> Result<()> {
    let pinned = match Pinned::open(
        path,
        READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES,
        SHARE_NO_DELETE,
    ) {
        Ok(pinned) => pinned,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return create_dir_with(path, sddl);
        }
        Err(error) => return Err(error).with_context(|| format!("Open {}", path.display())),
    };
    if pinned.is_link()? {
        bail!(
            "{} is a link to another location; services mode refuses it",
            path.display()
        );
    }
    if !pinned.security()?.owner_is_system_or_administrators() {
        bail!(
            "{} belongs to another account. Review its contents and remove it, then try again",
            path.display()
        );
    }
    pinned.set(
        sddl,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
    )
}

/// Whether `path` is a junction, symbolic link or other reparse point. Path-based security
/// calls follow them, so one planted inside the data directory would redirect them elsewhere.
pub fn is_reparse_point(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("Read {}", path.display())),
    }
}

pub fn refuse_reparse_point(path: &Path) -> Result<()> {
    if is_reparse_point(path)? {
        bail!(
            "{} is a link to another location; services mode refuses it",
            path.display()
        );
    }
    Ok(())
}

/// Refuses an existing file that services mode may not write to or trust, as
/// `check_trusted_file` explains.
pub fn refuse_redirected_file(path: &Path, access: Access) -> Result<()> {
    match Pinned::open(path, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_ALL) {
        Ok(pinned) => check_trusted_file(&pinned, access),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("Open {}", path.display())),
    }
}

/// A file services mode writes to as SYSTEM or an administrator, or trusts what it reads from:
/// not a link and with no other hard link, which would redirect the write; owned by SYSTEM,
/// Administrators or TrustedInstaller; and no one else may change it. A private file, such as
/// a log, must not be readable by anyone else either.
fn check_trusted_file(pinned: &Pinned, access: Access) -> Result<()> {
    let path = pinned.path().display();
    if pinned.is_link()? {
        bail!("{path} is a link to another location; services mode refuses it");
    }
    if pinned.links()? > 1 {
        bail!("{path} has another hard link; services mode refuses to write to it");
    }
    let security = pinned.security()?;
    if !security.owner_is_trusted() {
        bail!("{path} belongs to another account; services mode refuses to write to it");
    }
    if !security.protected() {
        bail!("{path} can be changed by other accounts; services mode refuses it");
    }
    if access == Access::Private && security.others_may_read() {
        bail!("{path} can be read by other accounts; services mode refuses to write to it");
    }
    Ok(())
}

/// Opens a file for appending, creating it if missing, and refuses it as
/// `refuse_redirected_file` does. The checks read the handle that is then written to, so the
/// path cannot be swapped for a link in between.
pub fn open_append(path: &Path, access: Access) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .access_mode(FILE_APPEND_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .with_context(|| format!("Open {}", path.display()))?;
    check_open_file(file, path, access)
}

/// Refuses a file already opened, without following a link, as `refuse_redirected_file`
/// does, reading the handle that the caller then uses.
pub fn check_open_file(file: std::fs::File, path: &Path, access: Access) -> Result<std::fs::File> {
    let pinned = Pinned::from_file(file, path);
    check_trusted_file(&pinned, access)?;
    Ok(pinned.into_file())
}

/// Opens a file to read what services mode trusts from it, such as a workload's state, and
/// refuses it as `refuse_redirected_file` does, through the handle that is read.
pub fn open_trusted(path: &Path, access: Access) -> Result<std::fs::File> {
    let pinned = Pinned::open(path, FILE_GENERIC_READ, SHARE_ALL)
        .with_context(|| format!("Open {}", path.display()))?;
    check_trusted_file(&pinned, access)?;
    Ok(pinned.into_file())
}

/// An entry an earlier, less protected folder held, before its permissions are reset: not a
/// link, no other hard link, and owned by SYSTEM, Administrators or TrustedInstaller. Its
/// DACL is what the reset replaces.
fn check_resettable(pinned: &Pinned) -> Result<()> {
    let path = pinned.path().display();
    if pinned.is_link()? {
        bail!("{path} is a link to another location; services mode refuses it");
    }
    if !pinned.is_dir()? && pinned.links()? > 1 {
        bail!("{path} has another hard link; services mode refuses to write to it");
    }
    if !pinned.security()?.owner_is_trusted() {
        bail!("{path} belongs to another account; services mode refuses to write to it");
    }
    Ok(())
}

/// Brings what an earlier, less protected folder may hold under the folder's own access:
/// every entry inside gets Administrators as owner and only the permissions it inherits.
/// Refuses, naming them all, entries that are links, have another hard link or belong to
/// another account, since resetting them would follow the link or hide what they held.
///
/// Each entry is opened by name without following a link, and then must resolve to a path
/// directly inside its folder's own resolved path, so a folder swapped for a link after it
/// was listed is caught. Every handle denies deletion and renaming until the reset is done,
/// which also keeps the folders above it in place.
pub fn reset_contents(dir: &Path) -> Result<()> {
    let folder = Pinned::open(dir, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_NO_DELETE)
        .with_context(|| format!("Open {}", dir.display()))?;
    let mut entries = vec![];
    let mut refused = vec![];
    collect_entries(&folder, &mut entries, &mut refused)?;
    if !refused.is_empty() {
        bail!(
            "Remove these from {} and try again:\n{}",
            dir.display(),
            refused.join("\n")
        );
    }
    for entry in &entries {
        entry.set(
            INHERIT_ONLY_SDDL,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | UNPROTECTED_DACL_SECURITY_INFORMATION,
        )?;
    }
    Ok(())
}

/// Administrators as owner and no explicit grants, so only what the parent passes down
/// applies once inheritance is turned back on.
const INHERIT_ONLY_SDDL: &str = "O:BAD:";

/// Every entry under `folder`, parents before their contents, each pinned. Links are refused,
/// not followed.
fn collect_entries(
    folder: &Pinned,
    entries: &mut Vec<Pinned>,
    refused: &mut Vec<String>,
) -> Result<()> {
    let resolved = folder.final_path()?;
    let listing = std::fs::read_dir(folder.path())
        .with_context(|| format!("Read {}", folder.path().display()))?;
    for entry in listing {
        let path = entry
            .with_context(|| format!("Read {}", folder.path().display()))?
            .path();
        let pinned = Pinned::open(
            &path,
            READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES,
            SHARE_NO_DELETE,
        )
        .with_context(|| format!("Open {}", path.display()))?;
        if pinned.final_path()?.parent() != Some(resolved.as_path()) {
            refused.push(format!("  {} moved while it was checked", path.display()));
            continue;
        }
        if let Err(error) = check_resettable(&pinned) {
            refused.push(format!("  {error:#}"));
            continue;
        }
        let descend = pinned.is_dir()?;
        entries.push(pinned);
        if descend {
            let index = entries.len() - 1;
            let mut inside = vec![];
            collect_entries(&entries[index], &mut inside, refused)?;
            entries.extend(inside);
        }
    }
    Ok(())
}

/// Whether only SYSTEM, Administrators and TrustedInstaller control `path`: it is not a
/// reparse point, one of them owns it, and no one else may write to, delete or change the
/// permissions of it. Both are read through one handle that does not follow links.
pub fn is_protected(path: &Path) -> bool {
    let Ok(pinned) = Pinned::open(path, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_ALL) else {
        return false;
    };
    matches!(pinned.is_link(), Ok(false))
        && pinned.security().is_ok_and(|security| security.protected())
}

/// Whether every folder above `path`, up to the volume root, stays where it is: none is a
/// link, and no account other than SYSTEM, Administrators and TrustedInstaller may rename,
/// delete or re-permission it or what it holds. Otherwise someone could swap a folder on the
/// way to the data directory for a link to their own copy after the checks.
pub fn ancestors_protected(path: &Path) -> bool {
    let Ok(path) = std::path::absolute(path) else {
        return false;
    };
    path.ancestors().skip(1).all(|folder| {
        let Ok(pinned) = Pinned::open(folder, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_ALL)
        else {
            return false;
        };
        matches!(pinned.is_link(), Ok(false))
            && pinned
                .security()
                .is_ok_and(|security| security.keeps_children_in_place())
    })
}

/// Whether an account other than SYSTEM, Administrators and TrustedInstaller may write to,
/// delete or re-permission `path` as it stands, which for a definition means it may have been
/// edited. A link is judged by its own permissions; other checks refuse links.
pub fn writable_by_others(path: &Path) -> Result<bool> {
    let pinned = Pinned::open(path, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_ALL)
        .with_context(|| format!("Open {}", path.display()))?;
    Ok(pinned.security()?.others_may_change())
}

/// Opens a file to read it, without following a link, and only if it resolves to a path
/// directly inside its folder as that folder resolves, so a link swapped in for the folder
/// is caught. Others may read but not change or delete the file while it is open.
pub fn open_inside(path: &Path) -> Result<std::fs::File> {
    let parent = path.parent().context("The file has no folder")?;
    let folder = Pinned::open(parent, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_NO_DELETE)
        .with_context(|| format!("Open {}", parent.display()))?;
    let file = Pinned::open(path, FILE_GENERIC_READ, FILE_SHARE_READ)
        .with_context(|| format!("Open {}", path.display()))?;
    if file.is_link()? || file.final_path()?.parent() != Some(folder.final_path()?.as_path()) {
        bail!(
            "{} leads to another location; services mode refuses it",
            path.display()
        );
    }
    Ok(file.into_file())
}

/// # Safety
/// `owner` must point to a valid SID, and `dacl` must be null or point to a valid ACL.
unsafe fn trusted_owner_and_dacl(owner: PSID, dacl: *const ACL) -> bool {
    unsafe {
        principal(owner) == Principal::Trusted
            && aces(dacl).is_some_and(|aces| crate::services::only_trusted_may_change(&aces))
    }
}

/// Opens a program to install, sharing it for reading only, so nobody can change, replace or
/// delete it while it is open. `None` unless just SYSTEM, Administrators and TrustedInstaller
/// may change it: it is not a link, has no other hard link, and its owner and permissions,
/// read through the same handle, are trusted.
pub fn open_protected(path: &Path) -> Result<Option<std::fs::File>> {
    let pinned = Pinned::open(path, FILE_GENERIC_READ, FILE_SHARE_READ)
        .with_context(|| format!("Open {}", path.display()))?;
    let protected = !pinned.is_link()? && pinned.links()? == 1 && pinned.security()?.protected();
    Ok(protected.then(|| pinned.into_file()))
}

/// # Safety
/// `dacl` must be null or point to a valid ACL. A null DACL grants everyone full control,
/// so it has no entries to return.
unsafe fn aces(dacl: *const ACL) -> Option<Vec<Ace>> {
    if dacl.is_null() {
        return None;
    }
    let mut found = vec![];
    unsafe {
        for index in 0..(*dacl).AceCount {
            let mut ace: *mut core::ffi::c_void = ptr::null_mut();
            if GetAce(dacl, index as u32, &mut ace) == 0 {
                return None;
            }
            let header = &*(ace as *const ACE_HEADER);
            let inherit_only = header.AceFlags as u32 & INHERIT_ONLY_ACE != 0;
            found.push(match header.AceType {
                ACCESS_DENIED_ACE_TYPE | ACCESS_DENIED_OBJECT_ACE_TYPE => Ace::Deny,
                ACCESS_ALLOWED_ACE_TYPE => {
                    let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
                    Ace::Allow {
                        principal: principal(&allowed.SidStart as *const u32 as PSID),
                        mask: allowed.Mask,
                        inherit_only,
                    }
                }
                _ => Ace::Unknown { inherit_only },
            });
        }
    }
    Some(found)
}

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn principal(sid: PSID) -> Principal {
    unsafe {
        if system_or_administrators(sid) || is_trusted_installer(sid) {
            Principal::Trusted
        } else if IsWellKnownSid(sid, WinCreatorOwnerSid) != 0 {
            Principal::CreatorOwner
        } else {
            Principal::Other
        }
    }
}

/// NT SERVICE\TrustedInstaller, which owns Windows' own folders, Program Files among them.
const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn is_trusted_installer(sid: PSID) -> bool {
    static SID: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    let known = SID.get_or_init(|| {
        let text = wide(OsStr::new(TRUSTED_INSTALLER));
        let mut converted: PSID = ptr::null_mut();
        // SAFETY: text is NUL-terminated; the SID is copied, then freed with LocalFree.
        unsafe {
            if ConvertStringSidToSidW(text.as_ptr(), &mut converted) == 0 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(
                converted as *const u8,
                GetLengthSid(converted) as usize,
            )
            .to_vec();
            LocalFree(converted);
            Some(bytes)
        }
    });
    match known {
        // SAFETY: both point to valid SIDs; EqualSid only reads them.
        Some(bytes) => unsafe { EqualSid(sid, bytes.as_ptr() as PSID) != 0 },
        None => false,
    }
}

/// Whether a data directory may be trusted as a services mode directory: the directory and
/// its marker are protected, and so are the folders above it.
pub fn is_trusted_machine_dir(path: &Path) -> bool {
    is_protected(path)
        && is_protected(&path.join(crate::services::files::MARKER))
        && ancestors_protected(path)
}

pub fn program_data() -> Result<PathBuf> {
    known_folder(&FOLDERID_ProgramData)
}

pub fn program_files() -> Result<PathBuf> {
    known_folder(&FOLDERID_ProgramFiles)
}

/// Asks the shell rather than reading environment variables, which the caller controls.
fn known_folder(id: &windows_sys::core::GUID) -> Result<PathBuf> {
    let mut text: windows_sys::core::PWSTR = ptr::null_mut();
    // SAFETY: id is a valid GUID; the returned string is freed with CoTaskMemFree below.
    unsafe {
        let result = SHGetKnownFolderPath(id, 0, ptr::null_mut(), &mut text);
        let path = (result >= 0 && !text.is_null()).then(|| {
            let mut length = 0;
            while *text.add(length) != 0 {
                length += 1;
            }
            PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
                text, length,
            )))
        });
        CoTaskMemFree(text as *const core::ffi::c_void);
        path.with_context(|| format!("Locate a system folder (error {result:#x})"))
    }
}

/// Whether a file or directory belongs to SYSTEM or Administrators, the only owners the
/// service host trusts. A link is read itself, not followed.
pub fn owned_by_system_or_administrators(path: &Path) -> Result<bool> {
    let pinned = Pinned::open(path, READ_CONTROL | FILE_READ_ATTRIBUTES, SHARE_ALL)
        .with_context(|| format!("Read the owner of {}", path.display()))?;
    Ok(pinned.security()?.owner_is_system_or_administrators())
}

/// Makes Administrators the owner of a file this process created, whatever its token's
/// default owner is.
pub fn set_owner_to_administrators(path: &Path) -> Result<()> {
    pin_unlinked(path)?.set("O:BA", OWNER_SECURITY_INFORMATION)
}

/// Gives a file this process created the owner and DACL in `sddl`, with inheritance blocked.
pub fn set_security(path: &Path, sddl: &str) -> Result<()> {
    pin_unlinked(path)?.set(
        sddl,
        OWNER_SECURITY_INFORMATION
            | DACL_SECURITY_INFORMATION
            | PROTECTED_DACL_SECURITY_INFORMATION,
    )
}

/// Opens `path` to change its owner and permissions, refusing a link.
fn pin_unlinked(path: &Path) -> Result<Pinned> {
    let pinned = Pinned::open(
        path,
        READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES,
        SHARE_NO_DELETE,
    )
    .with_context(|| format!("Open {}", path.display()))?;
    if pinned.is_link()? {
        bail!(
            "{} is a link to another location; services mode refuses it",
            path.display()
        );
    }
    Ok(pinned)
}

/// The registry hives K3 Up writes to.
#[derive(Debug, Clone, Copy)]
pub enum Hive {
    /// This user's settings, such as the login item.
    CurrentUser,
    /// Settings for the whole machine, which only administrators may change.
    LocalMachine,
}

impl Hive {
    fn handle(self) -> HKEY {
        match self {
            Hive::CurrentUser => HKEY_CURRENT_USER,
            Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        }
    }
}

/// A string value, or `None` when the key or the value is missing or not a string.
pub fn registry_string(hive: Hive, key: &str, value: &str) -> Option<String> {
    let key = wide(key.as_ref());
    let value = wide(value.as_ref());
    let mut bytes = 0u32;
    // SAFETY: NUL-terminated names; a null buffer asks only for the size.
    let status = unsafe {
        RegGetValueW(
            hive.handle(),
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: the buffer is at least `bytes` long, as reported by the first call.
    let status = unsafe {
        RegGetValueW(
            hive.handle(),
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Writes a string value, creating the key if needed.
pub fn set_registry_string(hive: Hive, key: &str, value: &str, data: &str) -> Result<()> {
    let key = wide(key.as_ref());
    let value = wide(value.as_ref());
    let data = wide(data.as_ref());
    // SAFETY: all buffers are NUL-terminated and outlive the call.
    let status = unsafe {
        RegSetKeyValueW(
            hive.handle(),
            key.as_ptr(),
            value.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    Ok(())
}

/// Deletes a value. A missing key or value is not an error.
pub fn delete_registry_value(hive: Hive, key: &str, value: &str) -> Result<()> {
    let key = wide(key.as_ref());
    let value = wide(value.as_ref());
    // SAFETY: NUL-terminated names that outlive the call.
    let status = unsafe { RegDeleteKeyValueW(hive.handle(), key.as_ptr(), value.as_ptr()) };
    if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    Ok(())
}

/// Whether this process runs with administrator rights.
pub fn is_elevated() -> bool {
    // SAFETY: the token handle is closed below; the output buffer matches the class asked for.
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = 0;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        CloseHandle(token);
        read != 0 && elevation.TokenIsElevated != 0
    }
}

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn system_or_administrators(sid: PSID) -> bool {
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
    }
}

/// Refuses a pipe owned by anyone other than this user, SYSTEM or Administrators. Otherwise
/// another local account could create the pipe while the agent is down and pose as it.
pub fn verify_pipe_owner(pipe: &std::fs::File) -> Result<()> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is open for the duration of the call; out-pointers are valid.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle() as HANDLE,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .context("Read agent pipe owner");
    }
    // SAFETY: owner points into descriptor, which stays allocated until LocalFree below.
    let trusted = unsafe {
        if system_or_administrators(owner) {
            Ok(true)
        } else {
            current_user_is(owner)
        }
    };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe {
        LocalFree(descriptor);
    }
    if !trusted? {
        bail!("The agent pipe belongs to another account; refusing to connect");
    }
    Ok(())
}

/// # Safety
/// `sid` must point to a valid SID.
unsafe fn current_user_is(sid: PSID) -> Result<bool> {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error()).context("Open process token");
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        // usize elements keep TOKEN_USER's pointer field aligned.
        let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        let read = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        );
        let error = std::io::Error::last_os_error();
        CloseHandle(token);
        if read == 0 {
            return Err(error).context("Read process user");
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        Ok(EqualSid(sid, user.User.Sid) != 0)
    }
}
