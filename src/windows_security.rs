//! Small Windows ACL boundary shared by private state and local named pipes.
use anyhow::{Context, Result};
use std::ffi::c_void;
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

fn current_sid() -> Result<String> {
    let mut token = std::ptr::null_mut();
    anyhow::ensure!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0,
        "cannot open process token"
    );
    let result = (|| -> Result<String> {
        let mut size = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        }
        anyhow::ensure!(size > 0, "cannot query token size");
        // usize storage gives TOKEN_USER its required alignment.
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        anyhow::ensure!(
            unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    buffer.as_mut_ptr().cast(),
                    size,
                    &mut size,
                )
            } != 0,
            "cannot query user token"
        );
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid = std::ptr::null_mut();
        anyhow::ensure!(
            unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid) } != 0,
            "cannot convert user SID"
        );
        let value = unsafe {
            let mut len = 0;
            while *sid.add(len) != 0 {
                len += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(sid, len))
        };
        unsafe {
            LocalFree(sid.cast());
        }
        value.context("invalid user SID")
    })();
    unsafe {
        CloseHandle(token);
    }
    result
}

pub struct Descriptor(pub *mut c_void);
impl Descriptor {
    pub fn current_user(inherit: bool) -> Result<Self> {
        let sid = current_sid()?;
        let flags = if inherit { "OICI" } else { "" };
        let sddl: Vec<u16> = format!("D:P(A;{flags};GA;;;{sid})(A;{flags};GA;;;SY)")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut pointer = std::ptr::null_mut();
        anyhow::ensure!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    1,
                    &mut pointer,
                    std::ptr::null_mut(),
                )
            } != 0,
            "cannot create private ACL"
        );
        Ok(Self(pointer))
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub fn protect_directory(path: &std::path::Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    };
    let descriptor = Descriptor::current_user(true)?;
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    anyhow::ensure!(
        unsafe {
            SetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            )
        } != 0,
        "cannot protect data directory: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
