//! Per-host window location and native-pixel session startup.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::monitor::MonitorHandle;
use winit::window::{Window, WindowAttributes};

use crate::layout::Placement;

#[derive(Debug, Clone, Copy)]
struct WorkArea {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

struct Screen {
    id: String,
    work: WorkArea,
    scale: f64,
}

impl Screen {
    fn from_monitor(monitor: &MonitorHandle) -> Self {
        let pos = monitor.position();
        let size = monitor.size();
        let mut screen = Self {
            id: monitor.name().unwrap_or_default(),
            work: WorkArea {
                x: pos.x,
                y: pos.y,
                width: size.width,
                height: size.height,
            },
            scale: monitor.scale_factor(),
        };
        #[cfg(windows)]
        {
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
            use winit::platform::windows::MonitorHandleExtWindows;
            screen.id = monitor.native_id();
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if unsafe { GetMonitorInfoW(HMONITOR(monitor.hmonitor() as *mut _), &mut info) }
                .as_bool()
            {
                let r = info.rcWork;
                if r.right > r.left && r.bottom > r.top {
                    screen.work = WorkArea {
                        x: r.left,
                        y: r.top,
                        width: (r.right - r.left) as u32,
                        height: (r.bottom - r.top) as u32,
                    };
                }
            }
        }
        screen
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedPosition {
    monitor: String,
    /// Relative to the monitor work area, so rearranging monitors preserves placement.
    offset_x: i32,
    offset_y: i32,
}

#[derive(Debug, PartialEq, Eq)]
struct Startup {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// Native image pixels whenever they fit. Only shrink; never enlarge.
/// Frame dimensions are the actual title bar and borders, not part of the image.
fn startup(
    remote: (u32, u32),
    work: WorkArea,
    frame: (u32, u32),
    saved: Option<&SavedPosition>,
) -> Startup {
    let available_w = work.width.saturating_sub(frame.0).max(1);
    let available_h = work.height.saturating_sub(frame.1).max(1);
    let remote = (remote.0.max(1), remote.1.max(1));
    let (width, height) = if remote.0 <= available_w && remote.1 <= available_h {
        remote
    } else {
        let fit = Placement::fit(remote.0, remote.1, available_w, available_h);
        (fit.width, fit.height)
    };
    let free_x = available_w.saturating_sub(width) as i64;
    let free_y = available_h.saturating_sub(height) as i64;
    let (dx, dy) = saved
        .map(|s| (s.offset_x as i64, s.offset_y as i64))
        .unwrap_or((free_x / 2, free_y / 2));
    let coord = |origin: i32, offset: i64| {
        (origin as i64 + offset).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    };
    Startup {
        x: coord(work.x, dx.clamp(0, free_x)),
        y: coord(work.y, dy.clamp(0, free_y)),
        width,
        height,
    }
}

fn select_screen(
    screens: &[Screen],
    saved: Option<&SavedPosition>,
    pointer: Option<(i32, i32)>,
    primary: Option<&str>,
) -> Option<usize> {
    if let Some(saved) = saved {
        if let Some(index) = screens.iter().position(|s| s.id == saved.monitor) {
            return Some(index);
        }
        // Missing monitor: fall back to an available screen, never stale coordinates.
    } else if let Some((x, y)) = pointer
        && let Some(index) = screens.iter().position(|s| {
            x as i64 >= s.work.x as i64
                && y as i64 >= s.work.y as i64
                && (x as i64) < s.work.x as i64 + s.work.width as i64
                && (y as i64) < s.work.y as i64 + s.work.height as i64
        })
    {
        return Some(index);
    }
    screens
        .iter()
        .position(|s| Some(s.id.as_str()) == primary)
        .or_else(|| (!screens.is_empty()).then_some(0))
}

fn local_pointer() -> Option<(i32, i32)> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT::default();
        unsafe {
            GetCursorPos(&mut point).ok()?;
        }
        Some((point.x, point.y))
    }
    #[cfg(not(windows))]
    None
}

#[derive(Default)]
pub struct WindowMemory {
    path: Option<PathBuf>,
    saved: Option<SavedPosition>,
}

impl WindowMemory {
    /// Certificate identity avoids mixing hosts or losing placement after an IP change.
    pub fn load(fingerprint: &str) -> Self {
        let key = tidedesk_core::identity::normalize_fingerprint(fingerprint);
        if key.len() != 64 {
            return Self::default();
        }
        let Ok(dir) = tidedesk_core::paths::config_dir() else {
            return Self::default();
        };
        Self::load_from(dir.join(format!("window-{key}.toml")))
    }

    fn load_from(path: PathBuf) -> Self {
        let saved = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok());
        Self {
            path: Some(path),
            saved,
        }
    }

    pub fn create_window(
        &self,
        event_loop: &ActiveEventLoop,
        attrs: WindowAttributes,
        remote: (u32, u32),
    ) -> Result<Window> {
        let screens: Vec<_> = event_loop
            .available_monitors()
            .map(|m| Screen::from_monitor(&m))
            .collect();
        let primary = event_loop
            .primary_monitor()
            .map(|m| Screen::from_monitor(&m).id);
        let Some(index) = select_screen(
            &screens,
            self.saved.as_ref(),
            local_pointer(),
            primary.as_deref(),
        ) else {
            return Ok(event_loop.create_window(
                attrs.with_inner_size(PhysicalSize::new(remote.0.max(1), remote.1.max(1))),
            )?);
        };
        let screen = &screens[index];
        let saved = self.saved.as_ref().filter(|s| s.monitor == screen.id);
        // Create hidden on the chosen monitor so its DPI determines the decorations.
        let estimate = ((16.0 * screen.scale) as u32, (48.0 * screen.scale) as u32);
        let initial = startup(remote, screen.work, estimate, saved);
        let window = event_loop.create_window(
            attrs
                .with_visible(false)
                .with_position(PhysicalPosition::new(initial.x, initial.y))
                .with_inner_size(PhysicalSize::new(initial.width, initial.height)),
        )?;
        let inner = window.inner_size();
        let outer = window.outer_size();
        let frame = (
            outer.width.saturating_sub(inner.width),
            outer.height.saturating_sub(inner.height),
        );
        let exact = startup(remote, screen.work, frame, saved);
        let _ = window.request_inner_size(PhysicalSize::new(exact.width, exact.height));
        window.set_outer_position(PhysicalPosition::new(exact.x, exact.y));
        window.set_visible(true);
        Ok(window)
    }

    /// Remember the last normal location, not a minimized sentinel or maximized frame.
    pub fn observe(&mut self, window: &Window) {
        if window.is_minimized() == Some(true) || window.is_maximized() {
            return;
        }
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let (Ok(position), Some(monitor)) = (window.outer_position(), window.current_monitor())
        else {
            return;
        };
        let screen = Screen::from_monitor(&monitor);
        // Require the title bar to be on this monitor. In particular, never save (-32000,-32000).
        if (position.x as i64) + window.outer_size().width as i64 <= screen.work.x as i64
            || position.x as i64 >= screen.work.x as i64 + screen.work.width as i64
            || (position.y as i64) + 64 <= screen.work.y as i64
            || position.y as i64 >= screen.work.y as i64 + screen.work.height as i64
        {
            return;
        }
        self.saved = Some(SavedPosition {
            monitor: screen.id,
            offset_x: position.x.saturating_sub(screen.work.x),
            offset_y: position.y.saturating_sub(screen.work.y),
        });
    }

    pub fn save(&self) -> Result<()> {
        if let (Some(path), Some(saved)) = (&self.path, &self.saved) {
            save_to(path, saved)?;
        }
        Ok(())
    }
}

fn save_to(path: &Path, saved: &SavedPosition) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, toml::to_string_pretty(saved)?).context("writing window position")?;
    std::fs::rename(tmp, path).context("saving window position")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(id: &str, x: i32, width: u32, height: u32) -> Screen {
        Screen {
            id: id.into(),
            work: WorkArea {
                x,
                y: 0,
                width,
                height,
            },
            scale: 1.0,
        }
    }

    #[test]
    fn two_k_on_four_k_is_exactly_native_not_scaled() {
        let s = screen("4k", 0, 3840, 2120);
        for frame in [(16, 39), (24, 58), (32, 78)] {
            let result = startup((2560, 1600), s.work, frame, None);
            assert_eq!((result.width, result.height), (2560, 1600));
            assert!(result.x >= 0 && result.y >= 0);
        }
    }

    #[test]
    fn downscales_only_when_needed_and_preserves_aspect() {
        let s = screen("small", 0, 1920, 1040);
        let result = startup((2560, 1600), s.work, (16, 40), None);
        assert_eq!((result.width, result.height), (1600, 1000));
        assert!(result.x as u32 + result.width + 16 <= s.work.width);
        assert_eq!(startup((800, 600), s.work, (16, 40), None).width, 800);
    }

    #[test]
    fn restores_same_screen_and_location_even_with_negative_coordinates() {
        let screens = [
            screen("primary", 0, 3840, 2120),
            screen("left", -3840, 3840, 2120),
        ];
        let saved = SavedPosition {
            monitor: "left".into(),
            offset_x: 100,
            offset_y: 150,
        };
        let index =
            select_screen(&screens, Some(&saved), Some((500, 500)), Some("primary")).unwrap();
        assert_eq!(index, 1);
        let result = startup((2560, 1600), screens[index].work, (16, 40), Some(&saved));
        assert_eq!((result.x, result.y), (-3740, 150));
        let rearranged = screen("left", 3840, 3840, 2120);
        assert_eq!(
            startup((2560, 1600), rearranged.work, (16, 40), Some(&saved)).x,
            3940
        );
    }

    #[test]
    fn missing_monitor_and_stale_coordinates_stay_on_screen() {
        let screens = [screen("primary", 0, 1920, 1040)];
        let saved = SavedPosition {
            monitor: "missing".into(),
            offset_x: i32::MAX,
            offset_y: i32::MIN,
        };
        assert_eq!(
            select_screen(&screens, Some(&saved), None, Some("primary")),
            Some(0)
        );
        let result = startup((1280, 720), screens[0].work, (16, 40), Some(&saved));
        assert_eq!((result.x, result.y), (624, 0));
        assert_eq!(select_screen(&[], None, None, None), None);
    }

    #[test]
    fn first_session_uses_pointer_monitor_instead_of_primary() {
        let screens = [
            screen("primary", 0, 1920, 1040),
            screen("right", 1920, 3840, 2120),
        ];
        assert_eq!(
            select_screen(&screens, None, Some((2500, 200)), Some("primary")),
            Some(1)
        );
    }

    #[test]
    fn positions_round_trip_and_corrupt_file_is_safe() {
        let dir = std::env::temp_dir().join(format!("tidedesk-window-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("position.toml");
        let saved = SavedPosition {
            monitor: "left".into(),
            offset_x: 120,
            offset_y: 80,
        };
        save_to(&path, &saved).unwrap();
        assert_eq!(
            WindowMemory::load_from(path.clone()).saved,
            Some(saved.clone())
        );
        save_to(
            &path,
            &SavedPosition {
                offset_x: 240,
                ..saved
            },
        )
        .unwrap();
        assert_eq!(
            WindowMemory::load_from(path.clone())
                .saved
                .unwrap()
                .offset_x,
            240
        );
        std::fs::write(&path, "invalid = [").unwrap();
        assert!(WindowMemory::load_from(path.clone()).saved.is_none());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
