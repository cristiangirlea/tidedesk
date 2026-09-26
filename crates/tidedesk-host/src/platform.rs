//! Small OS integration helpers.

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub use fallback::*;

#[cfg(windows)]
mod windows_impl {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SW_HIDE, SW_SHOW,
        SetForegroundWindow, ShowWindow,
    };
    use windows::core::BOOL;

    pub fn attach_console() {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        // A GUI-subsystem program has no console; when started from a terminal,
        // borrow the terminal's so `--headless` and `--help` output is visible.
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    pub fn enable_dpi_awareness() {
        use windows::Win32::UI::HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
        };
        // Without this, Windows reports scaled coordinates to us and pointer
        // positions land in the wrong place on high-DPI displays.
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    }

    /// Shows a fatal error when there may be no console to print it to.
    pub fn error_box(title: &str, message: &str) {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        use windows::core::HSTRING;
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(message),
                &HSTRING::from(title),
                MB_OK | MB_ICONERROR,
            )
        };
    }

    /// Finds this process's top-level window with the given title.
    fn own_window(title: &str) -> Option<HWND> {
        struct Search {
            title: Vec<u16>,
            pid: u32,
            found: Option<HWND>,
        }
        unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let search = unsafe { &mut *(lparam.0 as *mut Search) };
            let mut pid = 0;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid == search.pid {
                let mut buf = [0u16; 128];
                let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
                if buf[..len] == search.title[..] {
                    search.found = Some(hwnd);
                    return false.into();
                }
            }
            true.into()
        }
        let mut search = Search {
            title: title.encode_utf16().collect(),
            pid: unsafe { GetCurrentProcessId() },
            found: None,
        };
        let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut _ as isize)) };
        search.found
    }

    /// Shows or hides the window natively. Used from the tray, where the UI
    /// loop of a hidden window may not be running.
    pub fn set_window_visible(title: &str, visible: bool) {
        if let Some(hwnd) = own_window(title) {
            unsafe {
                let _ = ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
                if visible {
                    let _ = SetForegroundWindow(hwnd);
                }
            }
        }
    }

    /// Adds or removes the window's taskbar button without recreating it.
    pub fn set_taskbar_button(title: &str, show: bool) {
        let Some(hwnd) = own_window(title) else {
            return;
        };
        // The UI thread already has COM initialised (winit does it for drag and drop).
        let result = (|| -> windows::core::Result<()> {
            unsafe {
                let list: ITaskbarList =
                    CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
                list.HrInit()?;
                if show {
                    list.AddTab(hwnd)
                } else {
                    list.DeleteTab(hwnd)
                }
            }
        })();
        if let Err(e) = result {
            tracing::warn!("could not change the taskbar button: {e}");
        }
    }

    /// Makes the window's close button hide it instead of quitting; the app
    /// then lives on in the tray. Safe to call repeatedly.
    pub fn hide_on_close(title: &str) {
        use windows::Win32::Foundation::{LRESULT, WPARAM};
        use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
        use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

        unsafe extern "system" fn proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
            _id: usize,
            _data: usize,
        ) -> LRESULT {
            if msg == WM_CLOSE {
                let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
                return LRESULT(0);
            }
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }

        if let Some(hwnd) = own_window(title) {
            // Re-installing with the same id just replaces the existing subclass.
            let _ = unsafe { SetWindowSubclass(hwnd, Some(proc), 0x7D, 0) };
        }
    }

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const RUN_VALUE: &str = "TideDesk Host";

    fn is_packaged() -> bool {
        use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
        use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
        let mut length = 0;
        unsafe { GetCurrentPackageFullName(&mut length, None) == ERROR_INSUFFICIENT_BUFFER }
    }

    fn startup_task() -> windows::core::Result<windows::ApplicationModel::StartupTask> {
        use windows::ApplicationModel::StartupTask;
        StartupTask::GetAsync(&windows::core::HSTRING::from("TideDeskHost"))?.join()
    }

    /// Whether the host is registered to start when the user signs in.
    pub fn autostart_enabled() -> bool {
        if is_packaged() {
            use windows::ApplicationModel::StartupTaskState;
            return startup_task()
                .and_then(|task| task.State())
                .is_ok_and(|state| {
                    state == StartupTaskState::Enabled || state == StartupTaskState::EnabledByPolicy
                });
        }
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::HSTRING;
        unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(RUN_KEY),
                &HSTRING::from(RUN_VALUE),
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        }
        .is_ok()
    }

    /// Registers (or removes) the host under the current user's Run key,
    /// starting hidden in the tray.
    pub fn set_autostart(enable: bool) -> anyhow::Result<()> {
        if is_packaged() {
            use windows::ApplicationModel::StartupTaskState;
            let task = startup_task()?;
            if enable {
                let state = task.RequestEnableAsync()?.join()?;
                anyhow::ensure!(
                    state == StartupTaskState::Enabled
                        || state == StartupTaskState::EnabledByPolicy,
                    "Windows has disabled startup. Check Settings > Apps > Startup or your administrator's policy."
                );
            } else {
                task.Disable()?;
                anyhow::ensure!(
                    task.State()? != StartupTaskState::EnabledByPolicy,
                    "Your administrator's policy requires startup."
                );
            }
            return Ok(());
        }
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteKeyValueW};
        use windows::core::HSTRING;
        if enable {
            let exe = std::env::current_exe()?;
            set_run_value(&super::autostart_command(&exe, crate::self_prefix()))?;
        } else if autostart_enabled() {
            let (key, name) = (HSTRING::from(RUN_KEY), HSTRING::from(RUN_VALUE));
            unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &name) }.ok()?;
        }
        Ok(())
    }

    /// Points an autostart entry that starts `tidedesk-host.exe`, which is
    /// no longer shipped, at this program; `true` when it did. The Store
    /// build's startup task names this program already.
    pub fn migrate_autostart() -> anyhow::Result<bool> {
        // A development build must not take over the entry of an installed one.
        if is_packaged() || cfg!(debug_assertions) {
            return Ok(false);
        }
        let Some(existing) = run_value() else {
            return Ok(false);
        };
        let exe = std::env::current_exe()?;
        let Some(command) = super::migrated_autostart(&existing, &exe) else {
            return Ok(false);
        };
        set_run_value(&command)?;
        Ok(true)
    }

    /// The command under the Run key, if there is one.
    fn run_value() -> Option<String> {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
        use windows::core::HSTRING;
        let (key, name) = (HSTRING::from(RUN_KEY), HSTRING::from(RUN_VALUE));
        let mut bytes = 0u32;
        let read = |data: Option<*mut std::ffi::c_void>, bytes: &mut u32| unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                RRF_RT_REG_SZ,
                None,
                data,
                Some(bytes),
            )
        };
        read(None, &mut bytes).ok().ok()?;
        let mut text = vec![0u16; (bytes as usize).div_ceil(2)];
        read(Some(text.as_mut_ptr().cast()), &mut bytes).ok().ok()?;
        let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
        Some(String::from_utf16_lossy(&text[..end]))
    }

    fn set_run_value(command: &str) -> anyhow::Result<()> {
        use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
        use windows::core::HSTRING;
        let (key, name) = (HSTRING::from(RUN_KEY), HSTRING::from(RUN_VALUE));
        let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &key,
                &name,
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        }
        .ok()?;
        Ok(())
    }
}

#[cfg(not(windows))]
mod fallback {
    pub fn attach_console() {}
    pub fn enable_dpi_awareness() {}
    pub fn error_box(_title: &str, message: &str) {
        eprintln!("error: {message}");
    }
    pub fn set_window_visible(_title: &str, _visible: bool) {}
    pub fn set_taskbar_button(_title: &str, _show: bool) {}
    pub fn hide_on_close(_title: &str) {}
    pub fn autostart_enabled() -> bool {
        false
    }
    pub fn set_autostart(_enable: bool) -> anyhow::Result<()> {
        anyhow::bail!("starting with the system is not supported on this platform yet")
    }
    pub fn migrate_autostart() -> anyhow::Result<bool> {
        Ok(false)
    }
}

/// The Run-key command that starts the host hidden in the tray.
#[cfg(windows)]
fn autostart_command(exe: &std::path::Path, prefix: &[&str]) -> String {
    let mut command = format!("\"{}\"", exe.display());
    for word in prefix {
        command.push(' ');
        command.push_str(word);
    }
    command.push_str(" --tray");
    command
}

/// What an autostart entry should say instead, if it starts
/// `tidedesk-host.exe`: that program is no longer shipped, and an old copy
/// left in its folder would keep starting an old version. It becomes `exe`'s
/// window, hidden in the tray. `None` for any other entry.
#[cfg(windows)]
fn migrated_autostart(existing: &str, exe: &std::path::Path) -> Option<String> {
    let existing = existing.trim_start();
    let program = match existing.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next().unwrap_or_default(),
        None => existing.split_whitespace().next().unwrap_or_default(),
    };
    std::path::Path::new(program)
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("tidedesk-host.exe"))
        .then(|| autostart_command(exe, &[]))
}

#[cfg(all(test, windows))]
mod tests {
    use std::path::Path;

    #[test]
    fn autostart_command_names_the_mode_inside_the_one_program() {
        assert_eq!(
            super::autostart_command(Path::new(r"C:\Apps\tidedesk.exe"), &["host"]),
            r#""C:\Apps\tidedesk.exe" host --tray"#
        );
        assert_eq!(
            super::autostart_command(Path::new(r"C:\Apps\tidedesk.exe"), &[]),
            r#""C:\Apps\tidedesk.exe" --tray"#,
            "the one window, hidden in the tray"
        );
    }

    #[test]
    fn an_autostart_entry_for_the_old_host_program_moves_to_this_one() {
        let exe = Path::new(r"C:\New\tidedesk.exe");
        let one_window = Some(r#""C:\New\tidedesk.exe" --tray"#.to_string());
        for old in [
            r#""C:\Old\tidedesk-host.exe" --tray"#,
            r#""C:\Program Files\TideDesk\TIDEDESK-HOST.EXE" --tray"#,
            r"C:\Old\tidedesk-host.exe --tray",
            r#"  "D:\x y\tidedesk-host.exe""#,
        ] {
            assert_eq!(super::migrated_autostart(old, exe), one_window, "{old}");
        }
        for current in [
            r#""C:\New\tidedesk.exe" --tray"#,
            r#""C:\Old\tidedesk.exe" host --tray"#,
            r#""C:\Old\not-tidedesk-host.exe" --tray"#,
            r#""C:\tidedesk-host.exe\tidedesk.exe" --tray"#,
            "",
        ] {
            assert_eq!(super::migrated_autostart(current, exe), None, "{current}");
        }
    }
}
