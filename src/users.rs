//! User names, by uid, from the system's user database.

use std::collections::HashMap;
use std::ffi::CStr;

/// Names looked up once each: a refresh asks for the same handful of
/// uids hundreds of times a second.
#[derive(Debug, Default)]
pub struct Users {
    names: HashMap<u32, String>,
}

impl Users {
    pub fn name(&mut self, uid: u32) -> String {
        self.names.entry(uid).or_insert_with(|| lookup(uid).unwrap_or_else(|| uid.to_string())).clone()
    }
}

/// `getpwuid_r`, the reentrant one: the refresh runs off the UI thread.
fn lookup(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: zeroed passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is to a live buffer of the stated length.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr().cast(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: on success pw_name points into `buf`, NUL-terminated.
    Some(unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_uid_zero_and_an_unknown_uid_is_its_number() {
        let mut users = Users::default();
        assert_eq!(users.name(0), "root");
        assert_eq!(users.name(4_000_000_000), "4000000000");
    }
}
