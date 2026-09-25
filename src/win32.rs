use anyhow::{Context, Result, bail};
use std::{
    ffi::OsStr,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
            SE_KERNEL_OBJECT,
        },
        EqualSid, GetTokenInformation, IsWellKnownSid, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid,
    },
    Storage::FileSystem::CreateDirectoryW,
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// SYSTEM, Administrators and the pipe's owner only. The default pipe DACL also grants
/// Everyone read access, which is enough to occupy the single listening instance.
pub const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)";
const SERVICE_DIRECTORY_SDDL: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

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
    let mut attributes = SecurityAttributes::from_sddl(SERVICE_DIRECTORY_SDDL)?;
    let path_wide = wide(path.as_os_str());
    // SAFETY: both pointers reference live, initialized buffers for the duration of the call.
    if unsafe { CreateDirectoryW(path_wide.as_ptr(), attributes.as_mut_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("Create {}", path.display()));
    }
    Ok(())
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
        if IsWellKnownSid(owner, WinLocalSystemSid) != 0
            || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
        {
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
