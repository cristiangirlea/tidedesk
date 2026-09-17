//! The viewer window: presents pictures and turns local input into events.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tidedesk_core::clipboard::ClipboardBridge;
use tidedesk_core::protocol::{ClientMessage, InputEvent, MouseButton, ServerMessage};
use tidedesk_core::sharing::{PointerPosition, SharingState};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{CursorIcon, Window, WindowId};

use crate::layout::{self, Placement};
use crate::pointer::{Motion, PointerFlow};
use crate::settings::{self, ViewerSettings};
use crate::stream::{Picture, UiEvent};
use crate::window_placement::WindowMemory;

struct Surface {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

pub struct App {
    title: String,
    remote_size: (u32, u32),
    picture: Arc<Mutex<Picture>>,
    control: UnboundedSender<ClientMessage>,
    surface: Option<Surface>,
    window_memory: WindowMemory,
    placement: Placement,
    held_keys: HashSet<u16>,
    suppressed_keys: HashSet<u16>,
    modifiers: ModifiersState,
    focused: bool,
    last_cursor: Option<(f64, f64)>,
    settings: ViewerSettings,
    sharing: SharingState,
    sharing_request: u64,
    clipboard: ClipboardBridge,
    pending_clipboard: Option<(u64, String)>,
    pointer: PointerFlow,
    host_cursor: Option<PointerPosition>,
    handoff_started: Option<Instant>,
    next_tick: Instant,
    settings_window: Option<Child>,
    notice: Option<String>,
    pub exit_message: Option<String>,
}

impl App {
    pub fn new(
        title: String,
        remote_size: (u32, u32),
        picture: Arc<Mutex<Picture>>,
        control: UnboundedSender<ClientMessage>,
        window_memory: WindowMemory,
    ) -> Self {
        Self {
            title,
            remote_size,
            picture,
            control,
            surface: None,
            window_memory,
            placement: Placement::fit(0, 0, 0, 0),
            held_keys: HashSet::new(),
            suppressed_keys: HashSet::new(),
            modifiers: ModifiersState::empty(),
            focused: false,
            last_cursor: None,
            settings: ViewerSettings::load().unwrap_or_default(),
            sharing: SharingState::default(),
            sharing_request: 0,
            clipboard: ClipboardBridge::default(),
            pending_clipboard: None,
            pointer: PointerFlow::default(),
            host_cursor: None,
            handoff_started: None,
            next_tick: Instant::now(),
            settings_window: None,
            notice: None,
            exit_message: None,
        }
    }

    fn send(&self, event: InputEvent) {
        let _ = self.control.send(ClientMessage::Input(event));
    }

    fn mouse_enabled(&self) -> bool {
        self.settings.mouse && self.sharing.mouse && self.sharing.request == self.sharing_request
    }

    /// Use the live cursor, not a WM_MOUSEMOVE coordinate queued before a warp.
    fn cursor_position(&self) -> Option<(f64, f64)> {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
            let mut point = POINT::default();
            unsafe {
                GetCursorPos(&mut point).ok()?;
            }
            let origin = self.surface.as_ref()?.window.inner_position().ok()?;
            Some(((point.x - origin.x) as f64, (point.y - origin.y) as f64))
        }
        #[cfg(not(windows))]
        self.last_cursor
    }

    fn clipboard_enabled(&self) -> bool {
        self.settings.clipboard
            && self.sharing.clipboard
            && self.sharing.request == self.sharing_request
    }

    fn release_mouse(&mut self) {
        self.pointer.invalidate();
        self.handoff_started = None;
        let _ = self.control.send(ClientMessage::ReleaseMouse);
    }

    fn configure(&mut self) {
        self.sharing_request = self.sharing_request.wrapping_add(1);
        self.clipboard.set_enabled(false);
        self.pending_clipboard = None;
        self.release_mouse();
        let _ = self.control.send(ClientMessage::SetSharing {
            request: self.sharing_request,
            clipboard: self.settings.clipboard,
            mouse: self.settings.mouse,
        });
        self.update_title();
    }

    fn update_title(&self) {
        let status = |wanted: bool, enabled: bool| {
            if !wanted {
                "off"
            } else if enabled {
                "on"
            } else {
                "waiting/blocked by host"
            }
        };
        if let Some(surface) = &self.surface {
            surface.window.set_title(&format!(
                "{} | Clipboard {} ({}) | Mouse {} ({}) | Settings Ctrl+Alt+S{}",
                self.title,
                status(self.settings.clipboard, self.clipboard_enabled()),
                self.settings.clipboard_shortcut.label(),
                status(self.settings.mouse, self.mouse_enabled()),
                self.settings.mouse_shortcut.label(),
                self.notice
                    .as_ref()
                    .map(|n| format!(" | {n}"))
                    .unwrap_or_default()
            ));
        }
    }

    fn toggle(&mut self, clipboard: bool) {
        if clipboard {
            self.settings.clipboard = !self.settings.clipboard;
        } else {
            self.settings.mouse = !self.settings.mouse;
        }
        self.notice = self
            .settings
            .save()
            .err()
            .map(|e| format!("Could not save: {e}"));
        self.configure();
    }

    fn open_settings(&mut self) {
        if self
            .settings_window
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        {
            return;
        }
        let result = std::env::current_exe().and_then(|exe| {
            let mut command = Command::new(exe);
            command
                .arg("--settings")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            command.spawn()
        });
        match result {
            Ok(child) => self.settings_window = Some(child),
            Err(e) => self.notice = Some(format!("Cannot open settings: {e}")),
        }
        self.update_title();
    }

    fn host_message(&mut self, message: ServerMessage) {
        match message {
            ServerMessage::Cursor(position) => {
                // Display only: do not release controls, send input, or warp the OS cursor.
                self.host_cursor = Some(position);
                if let Some(surface) = &self.surface {
                    surface.window.request_redraw();
                }
            }
            ServerMessage::Sharing(sharing) if sharing.request == self.sharing_request => {
                self.sharing = sharing;
                self.pending_clipboard = None;
                self.clipboard.set_enabled(false);
                self.clipboard.set_enabled(self.clipboard_enabled());
                let _ = self.clipboard.poll();
                self.release_mouse();
                self.update_title();
            }
            ServerMessage::Clipboard { generation, text }
                if self.clipboard_enabled() && self.sharing.accepts_clipboard(generation) =>
            {
                self.pending_clipboard = if self.clipboard.receive(&text) {
                    None
                } else {
                    Some((generation, text))
                };
            }
            ServerMessage::Pointer(position) => {
                self.release_mouse();
                self.notice =
                    (!position.inside).then(|| "Host pointer is outside the shared screen".into());
                self.update_title();
            }
            ServerMessage::PointerAnchor { request, position }
                if self.focused && self.mouse_enabled() =>
            {
                self.last_cursor = self.cursor_position();
                if !self
                    .last_cursor
                    .is_some_and(|(x, y)| self.placement.contains(x, y))
                {
                    self.release_mouse();
                    return;
                }
                if let Some((x, y)) =
                    self.pointer
                        .anchor(request, position, self.placement, self.last_cursor)
                {
                    let result = self
                        .surface
                        .as_ref()
                        .unwrap()
                        .window
                        .set_cursor_position(PhysicalPosition::new(x, y));
                    if let Err(e) = result {
                        self.release_mouse();
                        self.notice = Some(format!("Cannot align mouse: {e}"));
                    } else {
                        self.pointer.warp_completed();
                    }
                }
                if self.pointer.epoch().is_some() {
                    self.handoff_started = None;
                }
                self.update_title();
            }
            _ => {}
        }
    }

    fn release_keys(&mut self) {
        for scancode in std::mem::take(&mut self.held_keys) {
            self.send(InputEvent::Key {
                scancode,
                pressed: false,
            });
        }
    }

    fn redraw(&mut self) {
        let Some(s) = &mut self.surface else { return };
        let size = s.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return; // minimised
        };
        if s.surface.resize(w, h).is_err() {
            return;
        }
        let pic = self.picture.lock().unwrap();
        let placement = Placement::fit(pic.width, pic.height, size.width, size.height);
        if placement != self.placement {
            self.pointer.invalidate();
        }
        self.placement = placement;
        let Ok(mut buffer) = s.surface.buffer_mut() else {
            return;
        };
        layout::blit(
            &pic.pixels,
            pic.width,
            pic.height,
            &mut buffer,
            size.width,
            self.placement,
        );
        drop(pic);
        if let Some(cursor) = self.host_cursor {
            layout::draw_host_cursor(
                &mut buffer,
                size.width,
                self.placement,
                cursor,
                s.window.scale_factor(),
            );
        }
        let _ = buffer.present();
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_cursor(CursorIcon::Crosshair);
        let window = match self
            .window_memory
            .create_window(event_loop, attrs, self.remote_size)
        {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.exit_message = Some(format!("cannot open window: {e}"));
                event_loop.exit();
                return;
            }
        };
        self.window_memory.observe(&window);
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|ctx| softbuffer::Surface::new(&ctx, window.clone()));
        match surface {
            Ok(surface) => {
                self.placement = Placement::fit(
                    self.remote_size.0,
                    self.remote_size.1,
                    window.inner_size().width,
                    window.inner_size().height,
                );
                self.surface = Some(Surface { window, surface });
                self.configure();
            }
            Err(e) => {
                self.exit_message = Some(format!("cannot create drawing surface: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Control(message) => self.host_message(message),
            UiEvent::NewPicture => {
                if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
            }
            UiEvent::Disconnected(reason) => {
                self.exit_message = Some(reason);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                // Several NewPicture events can collapse into one redraw; a
                // resize or expose needs a redraw even with no new picture.
                self.redraw();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if !focused {
                    self.release_keys();
                    self.suppressed_keys.clear();
                    self.modifiers = ModifiersState::empty();
                }
                self.release_mouse();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorLeft { .. } => {
                self.last_cursor = None;
                self.release_mouse();
            }
            WindowEvent::Resized(_) => {
                self.release_mouse();
                if let Some(surface) = &self.surface {
                    self.window_memory.observe(&surface.window);
                    surface.window.request_redraw();
                }
            }
            WindowEvent::Moved(_) => {
                if let Some(surface) = &self.surface {
                    self.window_memory.observe(&surface.window);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Some((position.x, position.y));
                if !self.focused || !self.mouse_enabled() {
                    return;
                }
                let Some((x, y)) = self.cursor_position() else {
                    self.release_mouse();
                    return;
                };
                self.last_cursor = Some((x, y));
                if !self.placement.contains(x, y) {
                    self.release_mouse();
                    return;
                }
                match self.pointer.moved(x, y, self.placement) {
                    Motion::None => {}
                    Motion::Request(request) => {
                        self.handoff_started = Some(Instant::now());
                        let _ = self.control.send(ClientMessage::PointerSync { request });
                    }
                    Motion::Move { epoch, x, y } => {
                        self.handoff_started = None;
                        let _ = self.control.send(ClientMessage::MouseInput {
                            epoch,
                            event: InputEvent::MouseMove { x, y },
                        });
                    }
                }
                if self.pointer.epoch().is_some() {
                    self.handoff_started = None;
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !self.focused || !self.mouse_enabled() {
                    return;
                }
                let Some(epoch) = self.pointer.epoch() else {
                    return;
                };
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Left,
                    winit::event::MouseButton::Right => MouseButton::Right,
                    winit::event::MouseButton::Middle => MouseButton::Middle,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(_) => return,
                };
                let _ = self.control.send(ClientMessage::MouseInput {
                    epoch,
                    event: InputEvent::MouseButton {
                        button,
                        pressed: state == ElementState::Pressed,
                    },
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !self.focused || !self.mouse_enabled() {
                    return;
                }
                let Some(epoch) = self.pointer.epoch() else {
                    return;
                };
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                let _ = self.control.send(ClientMessage::MouseInput {
                    epoch,
                    event: InputEvent::MouseWheel { dx, dy },
                });
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if is_synthetic {
                    return;
                }
                let Some(scancode) = event.physical_key.to_scancode() else {
                    return;
                };
                let scancode = scancode as u16;
                let pressed = event.state == ElementState::Pressed;
                if !self.focused {
                    return;
                }
                if self.suppressed_keys.contains(&scancode) {
                    if !pressed {
                        self.suppressed_keys.remove(&scancode);
                    }
                    return;
                }
                if event.repeat && !self.held_keys.contains(&scancode) {
                    return;
                }
                if pressed && !event.repeat {
                    let clipboard = self
                        .settings
                        .clipboard_shortcut
                        .matches(self.modifiers, event.physical_key);
                    let mouse = self
                        .settings
                        .mouse_shortcut
                        .matches(self.modifiers, event.physical_key);
                    let settings =
                        settings::settings_shortcut().matches(self.modifiers, event.physical_key);
                    if clipboard || mouse || settings {
                        self.release_keys();
                        self.suppressed_keys.insert(scancode);
                        if settings {
                            self.open_settings();
                        } else {
                            self.toggle(clipboard);
                        }
                        return;
                    }
                }
                if pressed {
                    self.held_keys.insert(scancode);
                } else if !self.held_keys.remove(&scancode) {
                    return;
                }
                // Repeats are forwarded too: injected keys don't auto-repeat.
                self.send(InputEvent::Key { scancode, pressed });
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(surface) = &self.surface {
            self.window_memory.observe(&surface.window);
        }
        if let Err(e) = self.window_memory.save() {
            tracing::warn!("could not save viewer window position: {e:#}");
        }
        self.release_keys();
        self.release_mouse();
        self.clipboard.set_enabled(false);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if Instant::now() >= self.next_tick {
            self.next_tick = Instant::now() + Duration::from_millis(200);
            if let Ok(settings) = ViewerSettings::load()
                && settings != self.settings
            {
                self.settings = settings;
                self.configure();
            }
            if self
                .handoff_started
                .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
            {
                self.release_mouse();
            }
            if self.clipboard_enabled() {
                if let Some((generation, text)) = &self.pending_clipboard {
                    if !self.sharing.accepts_clipboard(*generation) || self.clipboard.receive(text)
                    {
                        self.pending_clipboard = None;
                    }
                } else if let Some(text) = self.clipboard.poll() {
                    let _ = self.control.send(ClientMessage::Clipboard {
                        generation: self.sharing.generation,
                        text,
                    });
                }
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_tick));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_telemetry_is_display_only_with_control_off_or_on() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            "test".into(),
            (100, 100),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
        );
        app.settings = ViewerSettings::default();
        app.settings.mouse = false;
        let position = PointerPosition {
            epoch: 7,
            x: 30000,
            y: 40000,
            inside: true,
        };
        app.last_cursor = Some((5.0, 6.0));
        app.host_message(ServerMessage::Cursor(position));
        assert_eq!(app.host_cursor, Some(position));
        assert_eq!(app.last_cursor, Some((5.0, 6.0)));
        assert!(app.pointer.epoch().is_none());
        assert!(rx.try_recv().is_err());

        app.settings.mouse = true;
        app.sharing.mouse = true;
        app.placement = Placement::fit(100, 100, 100, 100);
        let Motion::Request(request) = app.pointer.moved(5.0, 6.0, app.placement) else {
            panic!("expected handoff");
        };
        app.pointer
            .anchor(request, position, app.placement, app.last_cursor);
        app.pointer.warp_completed();
        assert_eq!(app.pointer.epoch(), Some(7));
        app.host_message(ServerMessage::Cursor(PointerPosition {
            x: 35000,
            ..position
        }));
        assert_eq!(app.pointer.epoch(), Some(7));
        assert_eq!(app.last_cursor, Some((5.0, 6.0)));
        assert!(rx.try_recv().is_err());
    }
}
