//! Notification-area (tray) icon and menu for the host.
//!
//! Tray events are handled right in the tray callbacks rather than in the UI
//! loop, because the window may be hidden and not drawing at all.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use anyhow::Result;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::gui::WINDOW_TITLE;
use crate::session::HostState;
use crate::{icon, platform};

pub struct Tray {
    icon: TrayIcon,
    accept: CheckMenuItem,
    last_tooltip: String,
}

impl Tray {
    /// Must be called on the UI thread once its event loop is running.
    pub fn new(state: Arc<HostState>) -> Result<Self> {
        let open = MenuItem::new("Open TideDesk Host", true, None);
        let accept = CheckMenuItem::new(
            "Accept new connections",
            true,
            state.accepting.load(Ordering::SeqCst),
            None,
        );
        let disconnect = MenuItem::new("Disconnect viewer", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::with_items(&[
            &open,
            &PredefinedMenuItem::separator(),
            &accept,
            &disconnect,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let icon = TrayIconBuilder::new()
            .with_icon(tray_icon::Icon::from_rgba(
                icon::rgba(),
                icon::SIZE,
                icon::SIZE,
            )?)
            .with_tooltip("TideDesk Host")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()?;

        TrayIconEvent::set_event_handler(Some(|event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                platform::set_window_visible(WINDOW_TITLE, true);
            }
        }));

        let (open_id, accept_id, disconnect_id, quit_id) = (
            open.id().clone(),
            accept.id().clone(),
            disconnect.id().clone(),
            quit.id().clone(),
        );
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == open_id {
                platform::set_window_visible(WINDOW_TITLE, true);
            } else if event.id == accept_id {
                // The check item has already toggled itself.
                let now = !state.accepting.load(Ordering::SeqCst);
                state.accepting.store(now, Ordering::SeqCst);
                state.changed();
            } else if event.id == disconnect_id {
                if let Some(v) = state.viewer.lock().unwrap().as_ref() {
                    v.connection.close(2u32.into(), b"disconnected by host");
                }
            } else if event.id == quit_id {
                if let Some(v) = state.viewer.lock().unwrap().as_ref() {
                    v.connection.close(0u32.into(), b"host quit");
                }
                // Let the close frame leave before the process ends.
                std::thread::sleep(std::time::Duration::from_millis(150));
                std::process::exit(0);
            }
        }));

        Ok(Self {
            icon,
            accept,
            last_tooltip: String::new(),
        })
    }

    /// Mirrors state changed elsewhere (e.g. in the window) into the tray.
    pub fn sync(&mut self, state: &HostState) {
        let accepting = state.accepting.load(Ordering::SeqCst);
        if self.accept.is_checked() != accepting {
            self.accept.set_checked(accepting);
        }
        let tooltip = match state.viewer.lock().unwrap().as_ref() {
            Some(v) => format!("TideDesk Host — {} connected", v.name),
            None if accepting => "TideDesk Host — waiting for a viewer".to_string(),
            None => "TideDesk Host — paused".to_string(),
        };
        if tooltip != self.last_tooltip {
            let _ = self.icon.set_tooltip(Some(&tooltip));
            self.last_tooltip = tooltip;
        }
    }
}
