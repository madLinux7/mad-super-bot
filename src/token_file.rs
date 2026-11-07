//! Private token files: Unix modes or a protected, current-user-only Windows DACL.
use anyhow::{Context, Result, ensure};
use std::{fs::File, path::Path};

pub(crate) fn create(parent: &Path) -> Result<tempfile::NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".bot-token-");
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::PermissionsExt;
        let file = builder.tempfile_in(parent)?;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file
    };
    #[cfg(windows)]
    let file = builder.make_in(parent, windows::create)?;
    validate(file.as_file())?;
    Ok(file)
}

pub(crate) fn validate(file: &File) -> Result<()> {
    let metadata = file.metadata().context("cannot inspect token file")?;
    ensure!(
        metadata.is_file(),
        "token file must be a private regular file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            metadata.permissions().mode() & 0o077 == 0,
            "token file must be a private regular file with mode 0600"
        );
    }
    #[cfg(windows)]
    windows::validate(file)?;
    Ok(())
}

pub(crate) fn sync_parent(parent: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(parent)
        .and_then(|file| file.sync_all())
        .context("cannot synchronize token directory")?;
    #[cfg(windows)]
    let _ = parent; // Windows does not support opening directories for fsync this way.
    Ok(())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        ffi::c_void,
        io,
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{INVALID_HANDLE_VALUE, LocalFree},
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
        System::{
            SystemServices::ACCESS_ALLOWED_ACE_TYPE,
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    };

    struct LocalMemory(*mut c_void);
    impl Drop for LocalMemory {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    fn win32(code: u32) -> io::Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(code as i32))
        }
    }
    fn checked(ok: i32) -> io::Result<()> {
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    fn user() -> io::Result<Vec<usize>> {
        // The word-aligned buffer owns TOKEN_USER and its embedded SID together.
        unsafe {
            let mut raw = null_mut();
            checked(OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw))?;
            let token = OwnedHandle::from_raw_handle(raw);
            let mut size = 0;
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            checked(GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            ))?;
            Ok(buffer)
        }
    }
    fn sid(user: &[usize]) -> PSID {
        // `user` comes exclusively from a successful TokenUser query above.
        unsafe { (*(user.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }

    pub(super) fn create(path: &Path) -> io::Result<File> {
        let user = user()?;
        unsafe {
            let entry = EXPLICIT_ACCESS_W {
                grfAccessPermissions: FILE_ALL_ACCESS,
                grfAccessMode: SET_ACCESS,
                grfInheritance: NO_INHERITANCE,
                Trustee: TRUSTEE_W {
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_USER,
                    ptstrName: sid(&user).cast(),
                    ..Default::default()
                },
            };
            let mut acl = null_mut();
            win32(SetEntriesInAclW(1, &entry, null(), &mut acl))?;
            let _acl = LocalMemory(acl.cast());
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            let descriptor_ptr = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
            checked(InitializeSecurityDescriptor(descriptor_ptr, 1))?;
            checked(SetSecurityDescriptorDacl(descriptor_ptr, 1, acl, 0))?;
            checked(SetSecurityDescriptorControl(
                descriptor_ptr,
                SE_DACL_PROTECTED,
                SE_DACL_PROTECTED,
            ))?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor_ptr,
                bInheritHandle: 0,
            };
            let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let handle = CreateFileW(
                path.as_ptr(),
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(File::from_raw_handle(handle))
        }
    }

    pub(super) fn validate(file: &File) -> Result<()> {
        let user = user().context("cannot identify token file user")?;
        unsafe {
            let mut acl = null_mut();
            let mut descriptor = null_mut();
            win32(GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut acl,
                null_mut(),
                &mut descriptor,
            ))
            .context("cannot inspect token file permissions")?;
            let _descriptor = LocalMemory(descriptor);
            let mut control = 0;
            let mut revision = 0;
            checked(GetSecurityDescriptorControl(
                descriptor,
                &mut control,
                &mut revision,
            ))?;
            ensure!(
                !acl.is_null() && control & SE_DACL_PROTECTED != 0 && (*acl).AceCount == 1,
                "token file must grant access only to the current Windows user; run auth again"
            );
            let mut ace = null_mut();
            checked(GetAce(acl, 0, &mut ace))?;
            let header = &*(ace.cast::<ACE_HEADER>());
            ensure!(
                header.AceType == ACCESS_ALLOWED_ACE_TYPE as u8
                    && header.AceFlags == 0
                    && header.AceSize as usize >= size_of::<ACCESS_ALLOWED_ACE>(),
                "token file has unsupported permissions; run auth again"
            );
            let ace = &*(ace.cast::<ACCESS_ALLOWED_ACE>());
            ensure!(
                ace.Mask == FILE_ALL_ACCESS
                    && EqualSid((&ace.SidStart as *const u32).cast_mut().cast(), sid(&user)) != 0,
                "token file must grant access only to the current Windows user; run auth again"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn private_tokens_stay_private_after_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tokens.json");
        for value in ["first", "rotated"] {
            let mut file = create(directory.path()).unwrap();
            validate(file.as_file()).unwrap();
            file.write_all(value.as_bytes()).unwrap();
            file.as_file().sync_all().unwrap();
            file.persist(&path).unwrap();
            validate(&File::open(&path).unwrap()).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), value);
        }
    }

    #[cfg(windows)]
    #[test]
    fn rejects_inherited_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.json");
        std::fs::write(&path, "not private").unwrap();
        assert!(validate(&File::open(path).unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_shared_tokens() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let file = create(directory.path()).unwrap();
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(validate(file.as_file()).is_err());
    }
}
