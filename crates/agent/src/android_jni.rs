// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Zero-crate JNI entrypoint for running Bageo directly inside the Android
//! standalone application (`com.pixelcallai.streamer.EmbeddedAgent`).
//!
//! JNINativeInterface function table indices verified against Android NDK
//! `sysroot/usr/include/jni.h`:
//! - 167: `NewStringUTF`
//! - 169: `GetStringUTFChars`
//! - 170: `ReleaseStringUTFChars`

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use tracing::{error, info};

    use crate::embedded::{self, EmbeddedControl};

    type JNIEnv = *mut *const *const c_void;
    type JClass = *mut c_void;
    type JString = *mut c_void;
    type JBoolean = u8;
    type JInt = i32;

    const IDX_NEW_STRING_UTF: usize = 167;
    const IDX_GET_STRING_UTF_CHARS: usize = 169;
    const IDX_RELEASE_STRING_UTF_CHARS: usize = 170;

    type FnNewStringUTF = unsafe extern "C" fn(JNIEnv, *const c_char) -> JString;
    type FnGetStringUTFChars = unsafe extern "C" fn(JNIEnv, JString, *mut u8) -> *const c_char;
    type FnReleaseStringUTFChars = unsafe extern "C" fn(JNIEnv, JString, *const c_char);

    static CONTROL: OnceLock<Arc<EmbeddedControl>> = OnceLock::new();

    unsafe fn jstring_to_string(env: JNIEnv, s: JString) -> Option<String> {
        if env.is_null() || s.is_null() {
            return None;
        }
        let vtable = *env;
        if vtable.is_null() {
            return None;
        }
        let get_chars: FnGetStringUTFChars = std::mem::transmute(*vtable.add(IDX_GET_STRING_UTF_CHARS));
        let rel_chars: FnReleaseStringUTFChars =
            std::mem::transmute(*vtable.add(IDX_RELEASE_STRING_UTF_CHARS));
        let ptr = get_chars(env, s, std::ptr::null_mut());
        if ptr.is_null() {
            return None;
        }
        let text = CStr::from_ptr(ptr).to_string_lossy().into_owned();
        rel_chars(env, s, ptr);
        Some(text)
    }

    unsafe fn string_to_jstring(env: JNIEnv, s: &str) -> JString {
        if env.is_null() {
            return std::ptr::null_mut();
        }
        let vtable = *env;
        if vtable.is_null() {
            return std::ptr::null_mut();
        }
        let new_utf: FnNewStringUTF = std::mem::transmute(*vtable.add(IDX_NEW_STRING_UTF));
        let cleaned = s.replace('\0', "");
        let cstr = CString::new(cleaned).unwrap_or_else(|_| CString::new("{}").unwrap());
        new_utf(env, cstr.as_ptr())
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStart(
        env: JNIEnv,
        _cls: JClass,
        data_dir: JString,
        assets_dir: JString,
        port: JInt,
        lan_enabled: JBoolean,
    ) -> JBoolean {
        let Some(data) = jstring_to_string(env, data_dir) else {
            return 0;
        };
        let Some(assets) = jstring_to_string(env, assets_dir) else {
            return 0;
        };
        let data_path = PathBuf::from(data);
        let assets_path = PathBuf::from(assets);
        let listen_port = u16::try_from(port).unwrap_or(8787);
        let lan = lan_enabled != 0;

        embedded::init_embedded_logging(&data_path);

        if CONTROL.get().is_some() {
            return 1;
        }

        let ctrl = EmbeddedControl::new(data_path, assets_path, listen_port, lan);
        if CONTROL.set(ctrl.clone()).is_err() {
            return 1;
        }

        let spawned = std::thread::Builder::new()
            .name("bageo-embedded".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .thread_name("bageo-tokio")
                    .build()
                {
                    Ok(r) => r,
                    Err(e) => {
                        error!("failed to build tokio runtime: {e}");
                        return;
                    }
                };
                info!("starting embedded Bageo agent");
                if let Err(e) = rt.block_on(embedded::run_embedded(ctrl)) {
                    error!("embedded Bageo agent exited: {e:#}");
                }
            });

        if spawned.is_ok() {
            1
        } else {
            0
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeSetLanEnabled(
        _env: JNIEnv,
        _cls: JClass,
        enabled: JBoolean,
    ) -> JBoolean {
        let Some(ctrl) = CONTROL.get() else {
            return 0;
        };
        if ctrl.set_lan_enabled(enabled != 0) {
            1
        } else {
            0
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeSetUiPassword(
        env: JNIEnv,
        _cls: JClass,
        password: JString,
    ) -> JBoolean {
        let Some(ctrl) = CONTROL.get() else {
            return 0;
        };
        let Some(pw) = jstring_to_string(env, password) else {
            return 0;
        };
        if ctrl.set_ui_password(&pw).is_ok() {
            1
        } else {
            0
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativePatchSettings(
        env: JNIEnv,
        _cls: JClass,
        patch_json: JString,
    ) -> JBoolean {
        let Some(ctrl) = CONTROL.get() else {
            return 0;
        };
        let Some(raw) = jstring_to_string(env, patch_json) else {
            return 0;
        };
        if ctrl.apply_settings_patch(&raw).is_ok() {
            1
        } else {
            0
        }
    }

    #[no_mangle]
    pub unsafe extern "C" fn Java_com_pixelcallai_streamer_EmbeddedAgent_nativeStatusJson(
        env: JNIEnv,
        _cls: JClass,
    ) -> JString {
        let payload = match CONTROL.get() {
            Some(ctrl) => ctrl.status_json().to_string(),
            None => r#"{"running":false,"ready":false,"lan_enabled":false,"password_set":false,"ui_token":"","port":8787,"lan_ip":"","lan_url":"","mdns_url":"","active_calls":0}"#.to_string(),
        };
        string_to_jstring(env, &payload)
    }
}
