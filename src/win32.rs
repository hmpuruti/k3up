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
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT,
            SetNamedSecurityInfoW,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorDacl,
        GetSecurityDescriptorOwner, GetTokenInformation, INHERIT_ONLY_ACE, IsWellKnownSid,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TokenElevation,
        TokenUser, WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        GetFileInformationByHandle,
    },
    System::{
        Com::CoTaskMemFree,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
    UI::Shell::{FOLDERID_ProgramData, FOLDERID_ProgramFiles, SHGetKnownFolderPath},
};

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
/// have planted files the service host would trust.
pub fn secure_dir(path: &Path, sddl: &str) -> Result<()> {
    refuse_reparse_point(path)?;
    if !path.exists() {
        return create_dir_with(path, sddl);
    }
    if !owned_by_system_or_administrators(path)? {
        bail!(
            "{} belongs to another account. Review its contents and remove it, then try again",
            path.display()
        );
    }
    let attributes = SecurityAttributes::from_sddl(sddl)?;
    let path_wide = wide(path.as_os_str());
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut ACL = ptr::null_mut();
    // SAFETY: the descriptor is valid while attributes lives; the DACL points into it.
    let status = unsafe {
        if GetSecurityDescriptorDacl(
            attributes.descriptor(),
            &mut present,
            &mut dacl,
            &mut defaulted,
        ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("Read directory ACL");
        }
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .with_context(|| format!("Protect {}", path.display()));
    }
    Ok(())
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

/// Refuses an existing file that a write as an administrator or SYSTEM could be redirected
/// through: a reparse point, or a file with another hard link.
pub fn refuse_redirected_file(path: &Path) -> Result<()> {
    refuse_reparse_point(path)?;
    let file = match std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("Open {}", path.display())),
    };
    // SAFETY: the handle is open for the duration of the call; the structure is plain data.
    let links = unsafe {
        let mut information: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut information) == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("Read {}", path.display()));
        }
        information.nNumberOfLinks
    };
    if links > 1 {
        bail!(
            "{} has another hard link; services mode refuses to write to it",
            path.display()
        );
    }
    Ok(())
}

/// Whether only SYSTEM and Administrators control `path`: it is not a reparse point, one of
/// them owns it, and no one else may write to, delete or change the permissions of it.
pub fn is_protected(path: &Path) -> bool {
    if !matches!(is_reparse_point(path), Ok(false)) {
        return false;
    }
    let path_wide = wide(path.as_os_str());
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the path is NUL-terminated; out-pointers are valid for the call.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }
    // SAFETY: owner and dacl point into descriptor, which stays allocated until LocalFree.
    let protected = unsafe { system_or_administrators(owner) && only_trusted_may_change(dacl) };
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(descriptor);
    }
    protected
}

/// Whether a data directory may be trusted as a services mode directory: the directory and
/// its marker are protected.
pub fn is_trusted_machine_dir(path: &Path) -> bool {
    is_protected(path) && is_protected(&path.join(crate::services::files::MARKER))
}

/// # Safety
/// `dacl` must be null or point to a valid ACL.
unsafe fn only_trusted_may_change(dacl: *const ACL) -> bool {
    // A missing DACL grants everyone full control.
    if dacl.is_null() {
        return false;
    }
    unsafe {
        for index in 0..(*dacl).AceCount {
            let mut ace: *mut core::ffi::c_void = ptr::null_mut();
            if GetAce(dacl, index as u32, &mut ace) == 0 {
                return false;
            }
            let header = &*(ace as *const ACE_HEADER);
            if header.AceFlags as u32 & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            match header.AceType {
                ACCESS_DENIED_ACE_TYPE | ACCESS_DENIED_OBJECT_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE => {
                    let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
                    let sid = &allowed.SidStart as *const u32 as PSID;
                    if crate::services::grants_change(allowed.Mask)
                        && !system_or_administrators(sid)
                    {
                        return false;
                    }
                }
                // Conditional and object grants are never written by K3 Up; refuse them.
                _ => return false,
            }
        }
    }
    true
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
/// service host trusts.
pub fn owned_by_system_or_administrators(path: &Path) -> Result<bool> {
    let path_wide = wide(path.as_os_str());
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the path is NUL-terminated; out-pointers are valid for the call.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
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
            .with_context(|| format!("Read the owner of {}", path.display()));
    }
    // SAFETY: owner points into descriptor, which stays allocated until LocalFree below.
    let trusted = unsafe { system_or_administrators(owner) };
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe {
        LocalFree(descriptor);
    }
    Ok(trusted)
}

/// Makes Administrators the owner of a file this process created, whatever its token's
/// default owner is.
pub fn set_owner_to_administrators(path: &Path) -> Result<()> {
    let attributes = SecurityAttributes::from_sddl("O:BA")?;
    let path_wide = wide(path.as_os_str());
    let mut owner: PSID = ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: the descriptor is valid while attributes lives; the owner points into it.
    let status = unsafe {
        if GetSecurityDescriptorOwner(attributes.descriptor(), &mut owner, &mut defaulted) == 0 {
            return Err(std::io::Error::last_os_error()).context("Read owner");
        }
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            owner,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .with_context(|| format!("Set the owner of {}", path.display()));
    }
    Ok(())
}

/// Gives a file this process created the owner and DACL in `sddl`, with inheritance blocked.
pub fn set_security(path: &Path, sddl: &str) -> Result<()> {
    let attributes = SecurityAttributes::from_sddl(sddl)?;
    let path_wide = wide(path.as_os_str());
    let mut owner: PSID = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: the descriptor is valid while attributes lives; the owner and DACL point into it.
    let status = unsafe {
        if GetSecurityDescriptorOwner(attributes.descriptor(), &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(
                attributes.descriptor(),
                &mut present,
                &mut dacl,
                &mut defaulted,
            ) == 0
        {
            return Err(std::io::Error::last_os_error()).context("Read security descriptor");
        }
        SetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            ptr::null_mut(),
            dacl,
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(status as i32))
            .with_context(|| format!("Protect {}", path.display()));
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
