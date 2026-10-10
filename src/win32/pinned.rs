//! Files and folders opened once, without following a final link, so that the checks made on
//! one and the changes made to it apply to the same object even if its path is swapped for a
//! link in between.
use super::{
    Principal, SecurityAttributes, aces, principal, system_or_administrators,
    trusted_owner_and_dacl,
};
use anyhow::{Context, Result};
use std::{
    fs::File,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE, LocalFree},
    Security::{
        ACL, Authorization::GetSecurityInfo, Authorization::SE_FILE_OBJECT,
        Authorization::SetSecurityInfo, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        GetSecurityDescriptorOwner, OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, PSID,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
        GetFinalPathNameByHandleW,
    },
};

/// Others may read and write, but not delete or rename the object while it is open.
pub(super) const SHARE_NO_DELETE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE;
pub(super) const SHARE_ALL: u32 = SHARE_NO_DELETE | FILE_SHARE_DELETE;

pub(super) struct Pinned {
    file: File,
    path: PathBuf,
}

impl Pinned {
    pub(super) fn open(path: &Path, access: u32, share: u32) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(share)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    pub(super) fn from_file(file: File, path: &Path) -> Self {
        Self {
            file,
            path: path.to_path_buf(),
        }
    }

    pub(super) fn into_file(self) -> File {
        self.file
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    fn handle(&self) -> HANDLE {
        self.file.as_raw_handle() as HANDLE
    }

    fn information(&self) -> Result<BY_HANDLE_FILE_INFORMATION> {
        // SAFETY: the handle is open while self lives; the structure is plain data.
        unsafe {
            let mut information: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(self.handle(), &mut information) == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("Read {}", self.path.display()));
            }
            Ok(information)
        }
    }

    pub(super) fn is_link(&self) -> Result<bool> {
        Ok(self.information()?.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }

    pub(super) fn is_dir(&self) -> Result<bool> {
        Ok(self.information()?.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0)
    }

    pub(super) fn links(&self) -> Result<u32> {
        Ok(self.information()?.nNumberOfLinks)
    }

    pub(super) fn security(&self) -> Result<Security> {
        let mut security = Security {
            descriptor: ptr::null_mut(),
            owner: ptr::null_mut(),
            dacl: ptr::null_mut(),
        };
        // SAFETY: the handle is open while self lives; the out-pointers are valid, and the
        // descriptor is freed when `security` drops.
        let status = unsafe {
            GetSecurityInfo(
                self.handle(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut security.owner,
                ptr::null_mut(),
                &mut security.dacl,
                ptr::null_mut(),
                &mut security.descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .with_context(|| format!("Read the permissions of {}", self.path.display()));
        }
        Ok(security)
    }

    /// Applies the owner and DACL from `sddl`, as selected by `information`.
    pub(super) fn set(&self, sddl: &str, information: OBJECT_SECURITY_INFORMATION) -> Result<()> {
        let attributes = SecurityAttributes::from_sddl(sddl)?;
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        // SAFETY: the descriptor is valid while attributes lives and the owner and DACL point
        // into it; the handle is open while self lives.
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
            // A null DACL would grant everyone full control.
            if information & DACL_SECURITY_INFORMATION != 0 && dacl.is_null() {
                anyhow::bail!("The security descriptor has no DACL");
            }
            SetSecurityInfo(
                self.handle(),
                SE_FILE_OBJECT,
                information,
                owner,
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(std::io::Error::from_raw_os_error(status as i32))
                .with_context(|| format!("Protect {}", self.path.display()));
        }
        Ok(())
    }

    /// The path the system resolves this handle to, which a link swapped in after opening
    /// cannot change.
    pub(super) fn final_path(&self) -> Result<PathBuf> {
        let mut buffer = vec![0u16; 512];
        loop {
            // SAFETY: the buffer is writable for the length passed.
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    self.handle(),
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    FILE_NAME_NORMALIZED,
                )
            } as usize;
            if length == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("Resolve {}", self.path.display()));
            }
            if length < buffer.len() {
                use std::os::windows::ffi::OsStringExt;
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                    &buffer[..length],
                )));
            }
            buffer = vec![0u16; length + 1];
        }
    }
}

/// An owner and DACL read from a handle.
pub(super) struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}

impl Security {
    pub(super) fn owner_is_system_or_administrators(&self) -> bool {
        // SAFETY: the owner points into the descriptor, which lives as long as self.
        unsafe { system_or_administrators(self.owner) }
    }

    /// SYSTEM, Administrators or TrustedInstaller. Never the account running this process:
    /// an administrator's own account is not one every administrator controls.
    pub(super) fn owner_is_trusted(&self) -> bool {
        // SAFETY: the owner points into the descriptor, which lives as long as self.
        unsafe { principal(self.owner) == Principal::Trusted }
    }

    /// Owned by SYSTEM, Administrators or TrustedInstaller, and no one else may change it.
    pub(super) fn protected(&self) -> bool {
        // SAFETY: the owner and DACL point into the descriptor, which lives as long as self.
        unsafe { trusted_owner_and_dacl(self.owner, self.dacl) }
    }

    /// Whether the DACL lets an account other than SYSTEM, Administrators and TrustedInstaller
    /// read the object's contents.
    pub(super) fn others_may_read(&self) -> bool {
        // SAFETY: the DACL points into the descriptor, which lives as long as self.
        unsafe {
            !aces(self.dacl).is_some_and(|aces| crate::services::only_trusted_may_read(&aces))
        }
    }

    /// Whether the DACL lets an account other than SYSTEM, Administrators and TrustedInstaller
    /// write to, delete or re-permission the object.
    pub(super) fn others_may_change(&self) -> bool {
        // SAFETY: the DACL points into the descriptor, which lives as long as self.
        unsafe {
            !aces(self.dacl).is_some_and(|aces| crate::services::only_trusted_may_change(&aces))
        }
    }

    /// Owned by SYSTEM, Administrators or TrustedInstaller, and no one else may move, delete
    /// or re-permission it or what it holds.
    pub(super) fn keeps_children_in_place(&self) -> bool {
        // SAFETY: the owner and DACL point into the descriptor, which lives as long as self.
        unsafe {
            principal(self.owner) == Principal::Trusted
                && aces(self.dacl)
                    .is_some_and(|aces| crate::services::keeps_children_in_place(&aces))
        }
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: allocated by GetSecurityInfo, or null.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}
