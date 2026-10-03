//! The calls the Kotlin app makes (`app.tidedesk.viewer.Native`). A session
//! is a pointer to a boxed [`Viewer`], handed to Kotlin as a `long`; 0 means
//! none. Kotlin calls [`connect`](Java_app_tidedesk_viewer_Native_connect)
//! off the main thread (it waits for the network), reads frames on its
//! decoder thread, sends input from the main thread, and frees the session
//! only after closing it and stopping the decoder thread.

use std::path::PathBuf;
use std::sync::Mutex;

use jni::EnvUnowned;
use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{JByteArray, JClass, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong};

use crate::session::{MouseButton, Options, Viewer};

/// Why the last `connect` failed, for the app to show.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// The session behind `handle`.
///
/// # Safety
/// `handle` is 0 or came from `connect` and has not been freed.
unsafe fn viewer<'a>(handle: jlong) -> Option<&'a Viewer> {
    // SAFETY: as the caller promises.
    (handle != 0).then(|| unsafe { &*(handle as *const Viewer) })
}

/// Connects and proves the code; the session, or 0 (see `lastError`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_connect<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    target: JString<'caller>,
    code: JString<'caller>,
    name: JString<'caller>,
    data_dir: JString<'caller>,
) -> jlong {
    unowned
        .with_env(|env| -> jni::errors::Result<jlong> {
            let options = Options {
                target: target.try_to_string(env)?,
                code: code.try_to_string(env)?,
                name: name.try_to_string(env)?,
                data_dir: PathBuf::from(data_dir.try_to_string(env)?),
                service: tidedesk_core::nat::signal::DEFAULT_RENDEZVOUS.into(),
            };
            Ok(match Viewer::connect(&options) {
                Ok(viewer) => Box::into_raw(Box::new(viewer)) as jlong,
                Err(e) => {
                    *LAST_ERROR.lock().unwrap() = format!("{e:#}");
                    0
                }
            })
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_lastError<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
) -> JObject<'caller> {
    unowned
        .with_env(|env| -> jni::errors::Result<JObject<'caller>> {
            let text = LAST_ERROR.lock().unwrap().clone();
            Ok(env.new_string(text)?.into())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_hostName<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
) -> JObject<'caller> {
    unowned
        .with_env(|env| -> jni::errors::Result<JObject<'caller>> {
            // SAFETY: Kotlin passes a live session.
            let name = unsafe { viewer(handle) }.map_or("", Viewer::host_name);
            Ok(env.new_string(name)?.into())
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

/// The next frame, waiting for it: one byte of flags (1: keyframe), then
/// the H.264 access unit. `null` once the session ended.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_nextFrame<'caller>(
    mut unowned: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    handle: jlong,
) -> JByteArray<'caller> {
    unowned
        .with_env(|env| -> jni::errors::Result<JByteArray<'caller>> {
            // SAFETY: Kotlin passes a live session.
            let Some(frame) = unsafe { viewer(handle) }.and_then(Viewer::next_frame) else {
                return Ok(JByteArray::default());
            };
            let mut bytes = Vec::with_capacity(frame.data.len() + 1);
            bytes.push(u8::from(frame.keyframe));
            bytes.extend_from_slice(&frame.data);
            env.byte_array_from_slice(&bytes)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_pointer(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    x: jfloat,
    y: jfloat,
) {
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.pointer(x, y);
    }
}

/// `button`: 0 left, 1 right, 2 middle.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_button(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    button: jint,
    pressed: jboolean,
) {
    let button = match button {
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        _ => MouseButton::Left,
    };
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.button(button, pressed);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_wheel(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    notches: jint,
) {
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.wheel(notches);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_requestKeyframe(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.request_keyframe();
    }
}

/// `scancode`: PC/AT set 1, `0xE0` in the high byte for extended keys.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_key(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
    scancode: jint,
    pressed: jboolean,
) {
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.key(scancode as u16, pressed);
    }
}

/// The host's cursor once it moves, waiting for that: bit 32 set when it is
/// on the shared screen, x in bits 16 to 31 and y in bits 0 to 15, each from
/// 0 to 65535 across the screen. -1 once the session ended.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_nextCursor(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jlong {
    // SAFETY: Kotlin passes a live session.
    let Some(cursor) = unsafe { viewer(handle) }.and_then(Viewer::next_cursor) else {
        return -1;
    };
    let wire = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as i64;
    (i64::from(cursor.visible) << 32) | (wire(cursor.x) << 16) | wire(cursor.y)
}

/// Ends the session; the decoder thread's `nextFrame` then returns `null`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_close(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    // SAFETY: Kotlin passes a live session.
    if let Some(viewer) = unsafe { viewer(handle) } {
        viewer.close();
    }
}

/// Frees the session, after `close` and once nothing reads frames any more.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_tidedesk_viewer_Native_free(
    _unowned: EnvUnowned<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: from `connect`, freed once, as Kotlin promises.
        drop(unsafe { Box::from_raw(handle as *mut Viewer) });
    }
}
