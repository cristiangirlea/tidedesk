//! The notice that a session is on: a small window above all others in the
//! corner of the screen, "TideDesk: NAME is connected", with **End**. It is
//! shown whenever a viewer is connected, in the window, in the tray and
//! headless alike, and nothing turns it off: the person at this computer
//! always sees that a session is on and can end it.

use std::sync::Arc;

use crate::session::HostState;

/// What the notice says while `viewer` is connected; nothing when none is.
pub fn text(viewer: Option<&str>) -> Option<String> {
    viewer.map(|name| match name.trim() {
        "" => "TideDesk: a viewer is connected".to_string(),
        name => format!("TideDesk: {name} is connected"),
    })
}

/// A rectangle in screen pixels: left, top, right, bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Where the notice goes in `work_area` (the screen without the taskbar) at
/// `scale` (1.0 at 96 DPI), and where its End button is inside it.
pub fn layout(work_area: Rect, scale: f32) -> (Rect, Rect) {
    let px = |v: f32| (v * scale).round() as i32;
    let (width, height, margin) = (px(380.0), px(44.0), px(16.0));
    let window = Rect {
        left: work_area.right - margin - width,
        top: work_area.bottom - margin - height,
        right: work_area.right - margin,
        bottom: work_area.bottom - margin,
    };
    // Inside the window, its own coordinates.
    let inset = px(8.0);
    let end = Rect {
        left: width - inset - px(64.0),
        top: inset,
        right: width - inset,
        bottom: height - inset,
    };
    (window, end)
}

/// Starts the notice on its own thread, for as long as the program runs.
pub fn start(state: Arc<HostState>) {
    #[cfg(windows)]
    {
        let started = std::thread::Builder::new()
            .name("connected-notice".into())
            .spawn(move || {
                if let Err(e) = win::run(state) {
                    tracing::warn!("the connected notice could not open: {e:#}");
                }
            });
        if let Err(e) = started {
            tracing::warn!("the connected notice could not start: {e}");
        }
    }
    #[cfg(not(windows))]
    let _ = state;
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::sync::Arc;

    use anyhow::{Result, anyhow};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
        DEFAULT_CHARSET, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, DeleteObject,
        DrawTextW, EndPaint, FW_NORMAL, FW_SEMIBOLD, FillRect, HBRUSH, HFONT, InvalidateRect,
        OUT_DEFAULT_PRECIS, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::GetDpiForSystem;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW,
        HWND_TOPMOST, IDC_HAND, LoadCursorW, MSG, RegisterClassW, SPI_GETWORKAREA, SW_HIDE,
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetTimer, SetWindowPos, ShowWindow,
        SystemParametersInfoW, TranslateMessage, WM_LBUTTONUP, WM_PAINT, WM_TIMER, WNDCLASSW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };
    use windows::core::w;

    use super::{Rect, layout, text};
    use crate::session::HostState;

    /// How often the notice looks at whether a viewer is connected.
    const TICK_MS: u32 = 250;

    struct Notice {
        state: Arc<HostState>,
        shown: Option<String>,
        end: Rect,
        font: HFONT,
        bold: HFONT,
        background: HBRUSH,
        button: HBRUSH,
    }

    thread_local! {
        static NOTICE: RefCell<Option<Notice>> = const { RefCell::new(None) };
    }

    const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
        COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
    }

    pub fn run(state: Arc<HostState>) -> Result<()> {
        // SAFETY: plain Win32 calls on this thread's own window; the
        // notice's handles live as long as the thread.
        unsafe {
            let instance = GetModuleHandleW(None)?.into();
            let class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_HAND)?,
                lpszClassName: w!("TideDeskConnectedNotice"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(anyhow!("RegisterClassW failed"));
            }
            let scale = GetDpiForSystem() as f32 / 96.0;
            let mut area = RECT::default();
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut area as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )?;
            let work = Rect {
                left: area.left,
                top: area.top,
                right: area.right,
                bottom: area.bottom,
            };
            let (window, end) = layout(work, scale);
            let font = |weight: i32| {
                CreateFontW(
                    -(14.0 * scale).round() as i32,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    0,
                    w!("Segoe UI"),
                )
            };
            NOTICE.with_borrow_mut(|notice| {
                *notice = Some(Notice {
                    state,
                    shown: None,
                    end,
                    font: font(FW_NORMAL.0 as i32),
                    bold: font(FW_SEMIBOLD.0 as i32),
                    background: CreateSolidBrush(rgb(0x1B, 0x26, 0x35)),
                    button: CreateSolidBrush(rgb(0xC2, 0x3B, 0x3B)),
                })
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("TideDeskConnectedNotice"),
                w!("TideDesk"),
                WS_POPUP,
                window.left,
                window.top,
                window.right - window.left,
                window.bottom - window.top,
                None,
                None,
                Some(instance),
                None,
            )?;
            // A program started hidden (a script, a service, a scheduled
            // task) has its first ShowWindow call replaced by that start's
            // own: let it be this one, so the notice's own showing counts.
            let _ = ShowWindow(hwnd, SW_HIDE);
            SetTimer(Some(hwnd), 1, TICK_MS, None);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            NOTICE.with_borrow_mut(|notice| {
                if let Some(n) = notice.take() {
                    let _ = DeleteObject(n.font.into());
                    let _ = DeleteObject(n.bold.into());
                    let _ = DeleteObject(n.background.into());
                    let _ = DeleteObject(n.button.into());
                }
            });
        }
        Ok(())
    }

    /// Shows, hides or redraws the notice as the session comes and goes.
    unsafe fn tick(hwnd: HWND) {
        let change = NOTICE.with_borrow_mut(|notice| {
            let notice = notice.as_mut()?;
            let name = notice
                .state
                .viewer
                .lock()
                .unwrap()
                .as_ref()
                .map(|v| v.name.clone());
            let now = text(name.as_deref());
            let visible = now.is_some();
            let changed = now != notice.shown;
            notice.shown = now;
            Some((changed, visible))
        });
        let Some((changed, visible)) = change else {
            return;
        };
        // SAFETY: `hwnd` is this thread's live window.
        unsafe {
            if changed {
                let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
                let _ = InvalidateRect(Some(hwnd), None, true);
            }
            if visible {
                // Stays above windows that went on top after it.
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    unsafe fn paint(hwnd: HWND) {
        // SAFETY: painting this thread's window between BeginPaint and
        // EndPaint, with GDI objects the notice owns.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut ps);
            NOTICE.with_borrow(|notice| {
                let Some(n) = notice else { return };
                let mut all = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut all);
                FillRect(dc, &all, n.background);
                let end = RECT {
                    left: n.end.left,
                    top: n.end.top,
                    right: n.end.right,
                    bottom: n.end.bottom,
                };
                FillRect(dc, &end, n.button);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, rgb(0xFF, 0xFF, 0xFF));
                let pad = end.top * 2;
                let mut line = RECT {
                    left: pad,
                    top: 0,
                    right: end.left - pad / 2,
                    bottom: all.bottom,
                };
                let old = SelectObject(dc, n.font.into());
                let mut wide: Vec<u16> = n.shown.as_deref().unwrap_or("").encode_utf16().collect();
                DrawTextW(
                    dc,
                    &mut wide,
                    &mut line,
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
                SelectObject(dc, n.bold.into());
                let mut label: Vec<u16> = "End".encode_utf16().collect();
                let mut button = end;
                DrawTextW(
                    dc,
                    &mut label,
                    &mut button,
                    DT_SINGLELINE | DT_VCENTER | DT_CENTER,
                );
                SelectObject(dc, old);
            });
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// End: closes the session, as Disconnect in the window does.
    fn click(x: i32, y: i32) {
        NOTICE.with_borrow(|notice| {
            let Some(n) = notice else { return };
            let inside =
                (n.end.left..n.end.right).contains(&x) && (n.end.top..n.end.bottom).contains(&y);
            if !inside {
                return;
            }
            if let Some(viewer) = n.state.viewer.lock().unwrap().as_ref() {
                tracing::info!("session ended from the connected notice");
                viewer
                    .connection
                    .close(2u32.into(), b"disconnected by host");
            }
        });
    }

    unsafe extern "system" fn procedure(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // SAFETY: called by Windows for this thread's window.
        unsafe {
            match message {
                WM_TIMER => {
                    tick(hwnd);
                    LRESULT(0)
                }
                WM_PAINT => {
                    paint(hwnd);
                    LRESULT(0)
                }
                WM_LBUTTONUP => {
                    let x = (lparam.0 & 0xFFFF) as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
                    click(x, y);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notice_names_the_viewer_while_one_is_connected() {
        assert_eq!(text(None), None);
        assert_eq!(
            text(Some("Ana's laptop")).as_deref(),
            Some("TideDesk: Ana's laptop is connected")
        );
        assert_eq!(
            text(Some("  ")).as_deref(),
            Some("TideDesk: a viewer is connected")
        );
    }

    #[test]
    fn the_notice_sits_in_the_corner_above_the_taskbar() {
        let screen = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let (window, end) = layout(screen, 1.0);
        assert_eq!(
            window,
            Rect {
                left: 1920 - 16 - 380,
                top: 1040 - 16 - 44,
                right: 1904,
                bottom: 1024
            }
        );
        assert!(end.right <= 380 && end.bottom <= 44 && end.left > 380 / 2);
        let (big, big_end) = layout(screen, 1.5);
        assert_eq!(big.right - big.left, 570, "scaled with the screen");
        assert_eq!(big_end.right - big_end.left, 96);
        // A second monitor to the left: still inside the work area given.
        let left = Rect {
            left: -1280,
            top: 0,
            right: 0,
            bottom: 984,
        };
        let (window, _) = layout(left, 1.0);
        assert!(window.left >= left.left && window.right <= left.right);
    }
}
