//! `SendInput`-based injection.
//!
//! Keys are sent as hardware scancodes rather than virtual-key codes, so the
//! host's own keyboard layout decides which character a key produces — exactly
//! as if the keyboard were plugged into the host.

use anyhow::Result;
use tidedesk_core::protocol::{InputEvent, MouseButton};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

use super::{Backend, denormalize};
use crate::capture::DisplayRect;

pub struct SendInputBackend;

impl SendInputBackend {
    pub fn new() -> Self {
        Self
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(input: INPUT) {
    // A zero return means the input was blocked (e.g. UIPI when an elevated
    // window has focus); there is nothing useful to do but carry on.
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

impl Backend for SendInputBackend {
    fn inject(&mut self, event: InputEvent, display: DisplayRect) -> Result<()> {
        match event {
            InputEvent::MouseMove { x, y } => {
                let px = denormalize(x, display.left, display.width);
                let py = denormalize(y, display.top, display.height);
                // Absolute coordinates are normalised over the whole virtual
                // desktop so any monitor in a multi-display layout is reachable.
                let (vx, vy, vw, vh) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
                    )
                };
                let nx = ((px - vx) as i64 * 65535 / (vw - 1) as i64) as i32;
                let ny = ((py - vy) as i64 * 65535 / (vh - 1) as i64) as i32;
                send(mouse(
                    MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    nx,
                    ny,
                    0,
                ));
            }
            InputEvent::MouseButton { button, pressed } => {
                let (flags, data) = match (button, pressed) {
                    (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
                    (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
                    (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
                    (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
                    (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
                    (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
                    (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
                    (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
                    (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
                    (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
                };
                send(mouse(flags, 0, 0, data));
            }
            InputEvent::MouseWheel { dx, dy } => {
                if dy != 0 {
                    send(mouse(MOUSEEVENTF_WHEEL, 0, 0, dy));
                }
                if dx != 0 {
                    send(mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx));
                }
            }
            InputEvent::Key { scancode, pressed } => {
                let mut flags: KEYBD_EVENT_FLAGS = KEYEVENTF_SCANCODE;
                if scancode & 0xFF00 == 0xE000 {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if !pressed {
                    flags |= KEYEVENTF_KEYUP;
                }
                send(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: scancode & 0xFF,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }
        Ok(())
    }
}
