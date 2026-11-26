// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Log lines go to logcat (tag `bageo_audio_helper`) and to standard error.

#[cfg(target_os = "android")]
mod android {
    use std::ffi::{c_char, c_int, CString};

    #[link(name = "log")]
    extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub fn write(prio: c_int, msg: &str) {
        let text = CString::new(msg.replace('\0', " ")).unwrap_or_default();
        // SAFETY: both strings are NUL-terminated and live through the call.
        unsafe {
            __android_log_write(prio, c"bageo_audio_helper".as_ptr(), text.as_ptr());
        }
    }
}

pub fn info(msg: &str) {
    #[cfg(target_os = "android")]
    android::write(4, msg);
    eprintln!("bageo-audio-helper: {msg}");
}

pub fn warn(msg: &str) {
    #[cfg(target_os = "android")]
    android::write(5, msg);
    eprintln!("bageo-audio-helper: {msg}");
}
