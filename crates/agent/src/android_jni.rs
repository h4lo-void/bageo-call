// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Zero-crate JNI entrypoint for running Bageo directly inside the Android
//! standalone application (`com.pixelcallai.streamer.EmbeddedAgent`).
//!
//! Text and request bodies cross as Java `byte[]` holding UTF-8 (never
//! `GetStringUTFChars` / `NewStringUTF`, whose "modified UTF-8" mangles emoji and
//! NUL bytes). JNINativeInterface function table indices from Android NDK
//! `sysroot/usr/include/jni.h`:
//! - 171: `GetArrayLength`
//! - 176: `NewByteArray`
//! - 200: `GetByteArrayRegion`
//! - 208: `SetByteArrayRegion`

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::c_void;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::path::PathBuf;

    use crate::embedded::{self, LocalRequest, LocalResponse};

    type JNIEnv = *mut *const *const c_void;
    type JClass = *mut c_void;
    type JByteArray = *mut c_void;
    type JBoolean = u8;
    type JSize = i32;

    const IDX_GET_ARRAY_LENGTH: usize = 171;
    const IDX_NEW_BYTE_ARRAY: usize = 176;
    const IDX_GET_BYTE_ARRAY_REGION: usize = 200;
    const IDX_SET_BYTE_ARRAY_REGION: usize = 208;

    type FnGetArrayLength = unsafe extern "C" fn(JNIEnv, JByteArray) -> JSize;
    type FnNewByteArray = unsafe extern "C" fn(JNIEnv, JSize) -> JByteArray;
    type FnGetByteArrayRegion = unsafe extern "C" fn(JNIEnv, JByteArray, JSize, JSize, *mut i8);
    type FnSetByteArrayRegion = unsafe extern "C" fn(JNIEnv, JByteArray, JSize, JSize, *const i8);

    /// Entry `idx` of the JNI function table.
    unsafe fn table_entry(env: JNIEnv, idx: usize) -> Option<*const c_void> {
        if env.is_null() {
            return None;
        }
        let table = *env;
        if table.is_null() {
            return None;
        }
        let f = *table.add(idx);
        (!f.is_null()).then_some(f)
    }

    /// The bytes of a Java `byte[]` (None for null).
    unsafe fn read_bytes(env: JNIEnv, arr: JByteArray) -> Option<Vec<u8>> {
        if arr.is_null() {
            return None;
        }
        let len_of: FnGetArrayLength = std::mem::transmute(table_entry(env, IDX_GET_ARRAY_LENGTH)?);
        let copy: FnGetByteArrayRegion = std::mem::transmute(table_entry(env, IDX_GET_BYTE_ARRAY_REGION)?);
        let len = len_of(env, arr);
        if len <= 0 {
            return Some(Vec::new());
        }
        let mut buf = vec![0u8; len as usize];
        copy(env, arr, 0, len, buf.as_mut_ptr().cast());
        Some(buf)
    }

    /// A Java `byte[]` of UTF-8 text.
    unsafe fn read_text(env: JNIEnv, arr: JByteArray) -> Option<String> {
        String::from_utf8(read_bytes(env, arr)?).ok()
    }

    /// A new Java `byte[]` holding `data` (null when it cannot be made).
    unsafe fn new_bytes(env: JNIEnv, data: &[u8]) -> JByteArray {
        let Ok(len) = JSize::try_from(data.len()) else {
            return std::ptr::null_mut();
        };
        let (Some(make), Some(fill)) = (
            table_entry(env, IDX_NEW_BYTE_ARRAY),
            table_entry(env, IDX_SET_BYTE_ARRAY_REGION),
        ) else {
            return std::ptr::null_mut();
        };
        let make: FnNewByteArray = std::mem::transmute(make);
        let fill: FnSetByteArrayRegion = std::mem::transmute(fill);
        let arr = make(env, len);
        if !arr.is_null() && len > 0 {
            fill(env, arr, 0, len, data.as_ptr().cast());
        }
        arr
    }

    /// Start the agent, or find it running. Called again after it stopped, it starts it anew.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStart(
        env: JNIEnv,
        _cls: JClass,
        data_dir: JByteArray,
        assets_dir: JByteArray,
    ) -> JBoolean {
        let (Some(data), Some(assets)) = (read_text(env, data_dir), read_text(env, assets_dir)) else {
            return 0;
        };
        let data_path = PathBuf::from(data);
        embedded::init_embedded_logging(&data_path);
        let started = catch_unwind(AssertUnwindSafe(|| {
            embedded::start(data_path, PathBuf::from(assets)).is_some()
        }));
        u8::from(started.unwrap_or(false))
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativePatchSettings(
        env: JNIEnv,
        _cls: JClass,
        patch_json: JByteArray,
    ) -> JBoolean {
        let Some(raw) = read_text(env, patch_json) else {
            return 0;
        };
        let applied = catch_unwind(AssertUnwindSafe(|| {
            embedded::current().is_some_and(|ctrl| ctrl.apply_settings_patch(&raw).is_ok())
        }));
        u8::from(applied.unwrap_or(false))
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStatusJson(
        env: JNIEnv,
        _cls: JClass,
    ) -> JByteArray {
        let payload = catch_unwind(embedded::status_json)
            .map(|v| v.to_string())
            .unwrap_or_else(|_| r#"{"running":false,"ready":false,"active_calls":0}"#.to_string());
        new_bytes(env, payload.as_bytes())
    }

    /// The key the app's call service shows on the phone bridge (empty when not running).
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeBridgeKey(
        env: JNIEnv,
        _cls: JClass,
    ) -> JByteArray {
        let key = catch_unwind(|| embedded::current().map(|c| c.bridge_key().to_string()))
            .ok()
            .flatten()
            .unwrap_or_default();
        new_bytes(env, key.as_bytes())
    }

    /// One console request: method, path with query, headers (one
    /// `name: value` per line) and body, all UTF-8 bytes; the answer as
    /// [`LocalResponse::encode`]. Blocks: call it from a background thread.
    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeHttp(
        env: JNIEnv,
        _cls: JClass,
        method: JByteArray,
        target: JByteArray,
        headers: JByteArray,
        body: JByteArray,
    ) -> JByteArray {
        let (Some(method), Some(target)) = (read_text(env, method), read_text(env, target)) else {
            return std::ptr::null_mut();
        };
        let headers = read_bytes(env, headers)
            .map(|raw| embedded::parse_header_lines(&raw))
            .unwrap_or_default();
        let body = read_bytes(env, body).unwrap_or_default();
        let req = LocalRequest { method, target, headers, body };
        let res = catch_unwind(AssertUnwindSafe(move || match embedded::current() {
            Some(ctrl) => ctrl.http(req),
            None => LocalResponse::not_ready(),
        }))
        .unwrap_or_else(|_| LocalResponse::failed());
        new_bytes(env, &res.encode())
    }
}
