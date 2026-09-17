//! Screen capture.
//!
//! Capturers must only report a frame when the screen actually changed, so an
//! idle desktop costs (almost) nothing downstream.

#[cfg(windows)]
mod dxgi;

use std::time::Duration;

use anyhow::Result;

/// A captured frame in BGRA8, cropped to even dimensions for the encoder.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub bgra: Vec<u8>,
}

/// The captured display's position within the whole virtual desktop, in
/// physical pixels. Used to map viewer pointer positions back onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub index: usize,
    pub name: String,
    pub rect: DisplayRect,
    pub primary: bool,
}

pub trait Capturer {
    /// Waits up to `timeout` for the screen to change. Returns `true` when
    /// [`Capturer::frame`] now holds a newer image.
    fn next_frame(&mut self, timeout: Duration) -> Result<bool>;
    fn frame(&self) -> &Frame;
    fn rect(&self) -> DisplayRect;
}

pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    #[cfg(windows)]
    return dxgi::list_displays();
    #[cfg(not(windows))]
    anyhow::bail!("screen capture is not implemented on this platform yet");
}

pub fn open(display: usize) -> Result<Box<dyn Capturer>> {
    #[cfg(windows)]
    return Ok(Box::new(dxgi::DxgiCapturer::new(display)?));
    #[cfg(not(windows))]
    {
        let _ = display;
        anyhow::bail!("screen capture is not implemented on this platform yet");
    }
}
