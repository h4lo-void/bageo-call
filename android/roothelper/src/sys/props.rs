// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! System properties (Android's `getprop`).

#[cfg(target_os = "android")]
pub fn get(name: &str) -> Option<String> {
    use std::ffi::{c_char, c_int, CStr, CString};

    extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }
    // PROP_VALUE_MAX (92) bytes, NUL included.
    let key = CString::new(name).ok()?;
    let mut buf = [0 as c_char; 92];
    // SAFETY: the key is NUL-terminated; the buffer has PROP_VALUE_MAX bytes.
    let n = unsafe { __system_property_get(key.as_ptr(), buf.as_mut_ptr()) };
    if n <= 0 {
        return None;
    }
    // SAFETY: bionic NUL-terminates the value within the buffer.
    let v = unsafe { CStr::from_ptr(buf.as_ptr()) };
    Some(v.to_string_lossy().into_owned())
}

#[cfg(not(target_os = "android"))]
pub fn get(_name: &str) -> Option<String> {
    None
}
