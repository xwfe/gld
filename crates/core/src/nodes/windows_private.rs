//! 用当前执行账号的受保护 DACL 创建和读取节点秘密文件。
//! Windows 0.61.3 的 API 声明已核对；不接受继承权限或不实施 ACL 的卷。

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Node state requires a protected ACL owned by and granting access only to this account",
    )
}

fn win(error: windows::core::Error) -> io::Error {
    // Every API below reports a Win32 error through HRESULT_FROM_WIN32.
    // Preserve AlreadyExists / NotFound for the caller's atomic file protocol.
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other("Windows private-file operation failed")
    }
}

// usize backing gives TOKEN_USER/security-descriptor proper native alignment.
fn aligned(bytes: u32) -> io::Result<Vec<usize>> {
    if bytes == 0 || bytes > 64 * 1024 {
        return Err(denied());
    }
    Ok(vec![0; (bytes as usize).div_ceil(size_of::<usize>())])
}

fn current_user() -> io::Result<Vec<usize>> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(win)?;
        let _owned = OwnedHandle::from_raw_handle(token.0);
        let mut bytes = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut bytes);
        if bytes < size_of::<TOKEN_USER>() as u32 {
            return Err(denied());
        }
        let mut buffer = aligned(bytes)?;
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
            &mut bytes,
        )
        .map_err(win)?;
        Ok(buffer)
    }
}

fn user_sid(buffer: &[usize]) -> PSID {
    // buffer remains alive while this borrowed SID is used.
    unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid }
}

/// Creates an empty file exclusively, private from the first instant it exists.
/// Caller writes only after this succeeds, then syncs and atomically installs it.
pub(super) fn create_new_private(path: &Path) -> io::Result<File> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid node state path",
        ));
    }
    name.push(0);
    let user = current_user()?;
    let sid = user_sid(&user);
    unsafe {
        let acl_bytes = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>()
            + GetLengthSid(sid) as usize;
        let mut acl_buffer = vec![0_u32; acl_bytes.div_ceil(size_of::<u32>())];
        let acl = acl_buffer.as_mut_ptr().cast::<ACL>();
        InitializeAcl(acl, acl_bytes as u32, ACL_REVISION).map_err(win)?;
        AddAccessAllowedAce(acl, ACL_REVISION, FILE_ALL_ACCESS.0, sid).map_err(win)?;
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        let descriptor_ptr =
            PSECURITY_DESCRIPTOR((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast());
        // SECURITY_DESCRIPTOR_REVISION == 1; avoids an otherwise needless feature.
        InitializeSecurityDescriptor(descriptor_ptr, 1).map_err(win)?;
        SetSecurityDescriptorOwner(descriptor_ptr, Some(sid), false).map_err(win)?;
        SetSecurityDescriptorDacl(descriptor_ptr, true, Some(acl), false).map_err(win)?;
        SetSecurityDescriptorControl(descriptor_ptr, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
            .map_err(win)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor_ptr.0,
            bInheritHandle: false.into(),
        };
        let handle = CreateFileW(
            PCWSTR(name.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0 | DELETE.0,
            // 共享打开由稳定的文件锁协调，访问仍受当前账号 DACL 约束。
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            Some(&attributes),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL
                | FILE_FLAG_OPEN_REPARSE_POINT
                | SECURITY_SQOS_PRESENT
                | SECURITY_ANONYMOUS,
            None,
        )
        .map_err(win)?;
        let file = File::from_raw_handle(handle.0);
        if let Err(error) = check_private(&file) {
            // Delete this exact still-empty object, not a potentially replaced path.
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            let _ = SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            );
            return Err(error);
        }
        Ok(file)
    }
}

/// Verify the same open handle before reading a permanent or pending secret.
pub(super) fn check_private(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Err(denied());
    }
    let handle = HANDLE(file.as_raw_handle());
    let user = current_user()?;
    unsafe {
        let mut flags = 0;
        GetVolumeInformationByHandleW(handle, None, None, None, Some(&mut flags), None)
            .map_err(win)?;
        // FILE_PERSISTENT_ACLS: Microsoft fileapi.h value 0x00000008.
        // FAT ignores the supplied descriptor. Do not write/read secrets there.
        // This conservative check also rejects volumes whose security properties
        // cannot be queried (including some network shares).
        if flags & 0x0000_0008 == 0 {
            return Err(denied());
        }

        let info = OWNER_SECURITY_INFORMATION.0 | DACL_SECURITY_INFORMATION.0;
        let mut bytes = 0;
        let _ = GetKernelObjectSecurity(handle, info, None, 0, &mut bytes);
        let mut buffer = aligned(bytes)?;
        let descriptor = PSECURITY_DESCRIPTOR(buffer.as_mut_ptr().cast());
        GetKernelObjectSecurity(handle, info, Some(descriptor), bytes, &mut bytes).map_err(win)?;
        let mut owner = PSID::default();
        let mut defaulted = Default::default();
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted).map_err(win)?;
        if owner.0.is_null() || EqualSid(owner, user_sid(&user)).is_err() {
            return Err(denied());
        }
        let mut control = 0_u16;
        let mut revision = 0;
        GetSecurityDescriptorControl(descriptor, &mut control, &mut revision).map_err(win)?;
        if control & SE_DACL_PROTECTED.0 == 0 {
            return Err(denied());
        }

        let mut present = Default::default();
        let mut acl = std::ptr::null_mut();
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted)
            .map_err(win)?;
        if !present.as_bool() || acl.is_null() || (*acl).AceCount != 1 {
            return Err(denied());
        }
        let mut entry = std::ptr::null_mut();
        GetAce(acl, 0, &mut entry).map_err(win)?;
        let ace = entry.cast::<ACCESS_ALLOWED_ACE>();
        // ACCESS_ALLOWED_ACE_TYPE == 0; only this simple, explicit ACE is accepted.
        if (*ace).Header.AceType != 0
            || (*ace).Header.AceFlags != 0
            || (*ace).Header.AceSize < size_of::<ACCESS_ALLOWED_ACE>() as u16
        {
            return Err(denied());
        }
        let ace_sid = PSID(std::ptr::addr_of!((*ace).SidStart).cast_mut().cast());
        if EqualSid(ace_sid, user_sid(&user)).is_err() {
            return Err(denied());
        }
        Ok(())
    }
}
