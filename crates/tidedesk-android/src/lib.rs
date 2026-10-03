//! The Android viewer's Rust side. [`session`] connects to a TideDesk host,
//! proves the access code, hands each video frame on for Android's hardware
//! decoder, and turns touches into mouse input; it has nothing of Android in
//! it, so it is tested on any computer. `jni` is the thin layer the Kotlin
//! app calls.

pub mod session;

#[cfg(target_os = "android")]
mod jni;
