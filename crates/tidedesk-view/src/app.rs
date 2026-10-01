//! The viewer window: presents pictures and turns local input into events.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tidedesk_core::clipboard::ClipboardBridge;
use tidedesk_core::protocol::{ClientMessage, InputEvent, MouseButton, ServerMessage};
use tidedesk_core::sharing::{PointerPosition, SharingState};
use tidedesk_core::streaming::StreamingStatus;
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::control;
use crate::layout::{self, Placement};
use crate::pointer::{Motion, PointerFlow, Press};
use crate::settings::ViewerSettings;
use crate::stream::{Picture, UiEvent};
use crate::window_placement::WindowMemory;

/// Test control ([`crate::control`]): commands are done in the order they
/// came, each answered before the next is begun.
#[derive(Default)]
pub struct TestControl {
    /// Commands that wait for the one before them.
    commands: std::collections::VecDeque<Result<control::Command, String>>,
    /// The mouse command that waits for the host to say where its pointer
    /// is: the events need its epoch.
    waiting: Option<Waiting>,
    /// How many times the host was asked.
    asked: u64,
    /// The answers, to be printed in this order.
    pub answers: Vec<String>,
    /// `quit` was done: the viewer ends, and nothing after it is done.
    pub quit: bool,
    /// The network path in words, from the connection.
    pub path: Option<Box<dyn Fn() -> String>>,
}

struct Waiting {
    request: u64,
    since: Instant,
    events: Vec<InputEvent>,
    /// The answer once they are sent.
    done: String,
}

/// What the window is told of a key.
struct KeyPress<'a> {
    physical: PhysicalKey,
    logical: &'a Key,
    /// What it types, when pressed.
    text: Option<&'a str>,
    pressed: bool,
    repeat: bool,
}

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
    game_boost: Arc<AtomicBool>,
    boost_request: u64,
    boost_status: Option<StreamingStatus>,
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
    pub test: TestControl,
    /// For how long the host has not answered.
    silent: Option<Duration>,
    pub exit_message: Option<String>,
}

impl App {
    pub fn new(
        title: String,
        remote_size: (u32, u32),
        picture: Arc<Mutex<Picture>>,
        control: UnboundedSender<ClientMessage>,
        window_memory: WindowMemory,
        game_boost: Arc<AtomicBool>,
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
            game_boost,
            boost_request: 0,
            boost_status: None,
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
            test: TestControl::default(),
            silent: None,
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
            // Without a window, as in tests, the last position it was told.
            let Some(surface) = &self.surface else {
                return self.last_cursor;
            };
            let mut point = POINT::default();
            unsafe {
                GetCursorPos(&mut point).ok()?;
            }
            let origin = surface.window.inner_position().ok()?;
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

    /// A key pressed or let go of: the viewer's own shortcuts, and the rest
    /// for the host.
    fn key(&mut self, key: KeyPress<'_>) {
        let KeyPress {
            physical,
            pressed,
            repeat,
            ..
        } = key;
        if !self.focused {
            return;
        }
        let Some(scancode) = crate::keys::scancode(physical, key.logical) else {
            // No key, but what one would have typed, as tools do.
            if let (true, Some(text)) = (pressed, key.text) {
                let typed = |character| crate::keys::typed(character, &self.held_keys);
                for key in text.chars().flat_map(typed) {
                    self.send(key);
                }
            }
            return;
        };
        if self.suppressed_keys.contains(&scancode) {
            if !pressed {
                self.suppressed_keys.remove(&scancode);
            }
            return;
        }
        if repeat && !self.held_keys.contains(&scancode) {
            return;
        }
        if pressed && !repeat {
            let clipboard = self
                .settings
                .clipboard_shortcut
                .matches(self.modifiers, physical);
            let mouse = self
                .settings
                .mouse_shortcut
                .matches(self.modifiers, physical);
            let settings = self
                .settings
                .settings_shortcut
                .matches(self.modifiers, physical);
            let boost = self
                .settings
                .game_boost_shortcut
                .matches(self.modifiers, physical);
            if clipboard || mouse || settings || boost {
                self.release_keys();
                self.suppressed_keys.insert(scancode);
                if boost {
                    self.toggle_boost();
                } else if settings {
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

    /// What the session and test control tell the window.
    fn handle(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
            UiEvent::Control(message) => self.host_message(message),
            UiEvent::NewPicture => {
                if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
            }
            UiEvent::Silent(silent) => self.host_silent(silent),
            UiEvent::Command(command) => self.command(command),
            UiEvent::Disconnected(reason) => {
                self.game_boost.store(false, Ordering::Relaxed);
                self.release_keys();
                self.release_mouse();
                self.exit_message = Some(reason);
                event_loop.exit();
            }
        }
    }

    /// A line of test control. It is done once those before it are, and
    /// answered in `test.answers`.
    pub fn command(&mut self, command: Result<control::Command, String>) {
        self.test.commands.push_back(command);
        self.run_commands();
    }

    fn run_commands(&mut self) {
        while self.test.waiting.is_none()
            && !self.test.quit
            && let Some(command) = self.test.commands.pop_front()
        {
            let answer = command.and_then(|command| self.run(command));
            match answer {
                Ok(None) => {}
                Ok(Some(answer)) => self.test.answers.push(format!("ok {answer}")),
                Err(why) => self.test.answers.push(format!("error: {why}")),
            }
        }
    }

    /// Gives up on a host that does not say where its pointer is.
    fn give_up(&mut self, now: Instant) {
        const LONG: Duration = Duration::from_secs(2);
        if let Some(waiting) = &self.test.waiting
            && now.saturating_duration_since(waiting.since) >= LONG
        {
            self.test.waiting = None;
            let why = "the host did not say where its pointer is: is mouse control allowed there?";
            self.test.answers.push(format!("error: {why}"));
            self.run_commands();
        }
    }

    /// The remote screen's size: the last picture's, else as the host said.
    fn screen(&self) -> (u32, u32) {
        let picture = self.picture.lock().unwrap();
        match (picture.width, picture.height) {
            (0, _) | (_, 0) => self.remote_size,
            size => size,
        }
    }

    /// Does a command: its answer, or none yet for one that waits for the
    /// host.
    fn run(&mut self, command: control::Command) -> Result<Option<String>, String> {
        let frames = |picture: &Picture| match picture.decoded {
            Some(decoded) => format!(
                "{} frames, the last {} ms ago",
                picture.frames,
                decoded.elapsed().as_millis()
            ),
            None => "0 frames".into(),
        };
        let button = |button, pressed| InputEvent::MouseButton { button, pressed };
        let (wide, high) = self.screen();
        let at = |x: u32, y: u32| match x < wide && y < high {
            true => Ok(InputEvent::MouseMove {
                x: control::place(x, wide),
                y: control::place(y, high),
            }),
            false => Err(format!("{x},{y} is outside: the screen is {wide}x{high}")),
        };
        let (events, done) = match command {
            control::Command::Size => return Ok(Some(format!("{wide}x{high}"))),
            control::Command::Frames => return Ok(Some(frames(&self.picture.lock().unwrap()))),
            control::Command::Stats => {
                let picture = frames(&self.picture.lock().unwrap());
                let path = self.test.path.as_ref().map(|path| path());
                let path = path.unwrap_or_else(|| "no connection".into());
                return Ok(Some(format!("{wide}x{high}, {picture}, {path}")));
            }
            control::Command::Crop { area, file, scale } => {
                let cropped = control::crop(&self.picture.lock().unwrap(), area, scale)?;
                let (width, height) = (cropped.0, cropped.1);
                control::save(&file, cropped)?;
                return Ok(Some(format!("{width}x{height} {}", file.display())));
            }
            control::Command::Key { scancode, pressed } => {
                for pressed in pressed.map_or(vec![true, false], |pressed| vec![pressed]) {
                    self.send(InputEvent::Key { scancode, pressed });
                }
                let which = match pressed {
                    Some(true) => " down",
                    Some(false) => " up",
                    None => "",
                };
                return Ok(Some(format!("key {scancode:X}{which}")));
            }
            control::Command::Type { text } => {
                let mut keys = Vec::new();
                for character in text.chars() {
                    let typed = crate::keys::typed(character, &self.held_keys);
                    if typed.is_empty() {
                        return Err(format!("no key of this keyboard makes {character:?}"));
                    }
                    keys.extend(typed);
                }
                keys.into_iter().for_each(|key| self.send(key));
                return Ok(Some(format!("type {} characters", text.chars().count())));
            }
            control::Command::Quit => {
                self.test.quit = true;
                return Ok(Some("quit".into()));
            }
            control::Command::Move { x, y } => (vec![at(x, y)?], format!("move {x} {y}")),
            control::Command::Click { x, y, button: b } => (
                vec![at(x, y)?, button(b, true), button(b, false)],
                format!("click {x} {y}"),
            ),
            control::Command::Press { x, y, button: b } => {
                (vec![at(x, y)?, button(b, true)], format!("press {x} {y}"))
            }
            control::Command::Release { button: b } => (vec![button(b, false)], "release".into()),
            control::Command::Wheel { lines } => (
                vec![InputEvent::MouseWheel {
                    dx: 0,
                    dy: lines * 120,
                }],
                format!("wheel {lines}"),
            ),
        };
        if !self.mouse_enabled() {
            return Err("mouse control is off, here or at the host".into());
        }
        // The host takes mouse events with its pointer's epoch only, which
        // it tells with where its pointer is. Counted down from the top, so
        // as not to be taken for the person's own handoff.
        self.test.asked += 1;
        let request = u64::MAX - self.test.asked;
        self.test.waiting = Some(Waiting {
            request,
            since: Instant::now(),
            events,
            done,
        });
        let _ = self.control.send(ClientMessage::PointerSync { request });
        Ok(None)
    }

    /// The host said where its pointer is, for a command that waited.
    fn anchored(&mut self, position: PointerPosition) {
        let Some(waiting) = self.test.waiting.take() else {
            return;
        };
        if position.inside {
            for event in waiting.events {
                let epoch = position.epoch;
                let _ = self
                    .control
                    .send(ClientMessage::MouseInput { epoch, event });
            }
            self.test.answers.push(format!("ok {}", waiting.done));
        } else {
            let why = "the host's pointer is on a screen that is not shared: move it onto this one";
            self.test.answers.push(format!("error: {why}"));
        }
        self.run_commands();
    }

    /// Prints the answers of test control.
    fn answer(&mut self) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        for answer in self.test.answers.drain(..) {
            let _ = writeln!(out, "{answer}");
        }
        let _ = out.flush();
    }

    /// A mouse button pressed or let go of where the viewer's pointer is.
    fn button(&mut self, button: MouseButton, pressed: bool) {
        if !self.focused || !self.mouse_enabled() {
            return;
        }
        let event = InputEvent::MouseButton { button, pressed };
        let to_host = |epoch, event| ClientMessage::MouseInput { epoch, event };
        if !pressed {
            if let Some(epoch) = self.pointer.released(event) {
                let _ = self.control.send(to_host(epoch, event));
            }
            return;
        }
        let Some((x, y)) = self.cursor_position() else {
            return;
        };
        if !self.placement.contains(x, y) {
            return;
        }
        match self.pointer.pressed(x, y, self.placement, event) {
            Press::Send { epoch, to } => {
                if let Some((x, y)) = to {
                    let moved = InputEvent::MouseMove { x, y };
                    let _ = self.control.send(to_host(epoch, moved));
                }
                let _ = self.control.send(to_host(epoch, event));
            }
            Press::Request(request) => {
                self.handoff_started = Some(Instant::now());
                let _ = self.control.send(ClientMessage::PointerSync { request });
            }
            Press::Wait => {}
        }
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

    fn configure_boost(&mut self) {
        self.release_keys();
        self.release_mouse();
        self.boost_request = self.boost_request.wrapping_add(1);
        self.boost_status = None;
        self.game_boost.store(false, Ordering::Relaxed);
        let _ = self.control.send(ClientMessage::SetGameBoost {
            request: self.boost_request,
            enabled: self.settings.game_boost,
        });
        self.update_title();
    }

    fn toggle_boost(&mut self) {
        self.settings.game_boost = !self.settings.game_boost;
        self.notice = self
            .settings
            .save()
            .err()
            .map(|e| format!("Could not save: {e}"));
        self.configure_boost();
    }

    fn host_silent(&mut self, silent: Option<Duration>) {
        self.silent = silent;
        self.update_title();
    }

    fn update_title(&self) {
        if let Some(surface) = &self.surface {
            surface.window.set_title(&self.window_title());
        }
    }

    fn window_title(&self) -> String {
        let status = |wanted: bool, enabled: bool| {
            if !wanted {
                "off"
            } else if enabled {
                "on"
            } else {
                "waiting/blocked by host"
            }
        };
        format!(
            "{}{} | Game Boost {} ({}) | Clipboard {} ({}) | Mouse {} ({}) | Settings {}{}",
            self.title,
            // First, where it shows in a title cut short.
            self.silent
                .map(|s| format!(" | No answer from the host for {} s", s.as_secs()))
                .unwrap_or_default(),
            self.boost_status
                .map(|s| {
                    format!(
                        "{} ({} FPS target)",
                        if s.game_boost { "on" } else { "off" },
                        s.fps
                    )
                })
                .unwrap_or_else(|| "applying".into()),
            self.settings.game_boost_shortcut.label(),
            status(self.settings.clipboard, self.clipboard_enabled()),
            self.settings.clipboard_shortcut.label(),
            status(self.settings.mouse, self.mouse_enabled()),
            self.settings.mouse_shortcut.label(),
            self.settings.settings_shortcut.label(),
            self.notice
                .as_ref()
                .map(|n| format!(" | {n}"))
                .unwrap_or_default()
        )
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
            command.args(crate::self_prefix()).arg("--settings");
            // On the session's monitor, in front of it, not where Windows
            // puts new windows.
            if let Some(surface) = &self.surface {
                let window = &surface.window;
                if let Ok(corner) = window.outer_position() {
                    let size = window.outer_size();
                    let scale = window.scale_factor();
                    let x = (f64::from(corner.x) + f64::from(size.width) / 2.0) / scale;
                    let y = (f64::from(corner.y) + f64::from(size.height) / 2.0) / scale;
                    command.arg("--settings-near").arg(format!("{x:.0},{y:.0}"));
                }
            }
            command
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
            ServerMessage::Streaming(status)
                if status.request == self.boost_request
                    && status.game_boost == self.settings.game_boost =>
            {
                self.boost_status = Some(status);
                self.game_boost.store(status.game_boost, Ordering::Relaxed);
                self.update_title();
            }
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
                if self.test.waiting.as_ref().map(|w| w.request) == Some(request) =>
            {
                self.anchored(position);
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
                // Buttons pressed meanwhile: the host's pointer goes to them.
                if let Some((epoch, waited)) = self.pointer.claim() {
                    for event in waited {
                        let _ = self
                            .control
                            .send(ClientMessage::MouseInput { epoch, event });
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
        let mut pic = self.picture.lock().unwrap();
        pic.redraw_pending = false;
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
                self.configure_boost();
            }
            Err(e) => {
                self.exit_message = Some(format!("cannot create drawing surface: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        self.handle(event_loop, event);
        self.answer();
        if self.test.quit {
            event_loop.exit();
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
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Left,
                    winit::event::MouseButton::Right => MouseButton::Right,
                    winit::event::MouseButton::Middle => MouseButton::Middle,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(_) => return,
                };
                self.button(button, state == ElementState::Pressed);
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
                self.key(KeyPress {
                    physical: event.physical_key,
                    logical: &event.logical_key,
                    text: event.text.as_deref(),
                    pressed: event.state == ElementState::Pressed,
                    repeat: event.repeat,
                });
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
                let sharing_changed = settings.clipboard != self.settings.clipboard
                    || settings.mouse != self.settings.mouse;
                let boost_changed = settings.game_boost != self.settings.game_boost;
                self.settings = settings;
                if sharing_changed {
                    self.configure();
                }
                if boost_changed {
                    self.configure_boost();
                }
                self.update_title();
            }
            if self
                .handoff_started
                .is_some_and(|t| t.elapsed() > Duration::from_secs(1))
            {
                self.release_mouse();
            }
            self.give_up(Instant::now());
            self.answer();
            if self.test.quit {
                event_loop.exit();
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
    use crate::control::Command;
    use winit::keyboard::{KeyCode, NamedKey, NativeKey, NativeKeyCode};

    fn app() -> (App, tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            "test".into(),
            (100, 100),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
            Arc::new(AtomicBool::new(false)),
        );
        app.settings = ViewerSettings::default();
        app.focused = true;
        (app, rx)
    }

    /// A character as a tool types it: no key, only what one would type.
    fn typed(text: &str, pressed: bool) -> KeyPress<'_> {
        KeyPress {
            physical: PhysicalKey::Unidentified(NativeKeyCode::Windows(0)),
            logical: &Key::Unidentified(NativeKey::Windows(0xE7)),
            text: pressed.then_some(text),
            pressed,
            repeat: false,
        }
    }

    fn sent(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) -> Vec<ClientMessage> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn with_mouse() -> (App, tokio::sync::mpsc::UnboundedReceiver<ClientMessage>) {
        let (mut app, rx) = app();
        app.settings.mouse = true;
        app.sharing.mouse = true;
        // Test control needs neither the focus nor the pointer in the window.
        app.focused = false;
        (app, rx)
    }

    const ANCHOR: PointerPosition = PointerPosition {
        epoch: 7,
        x: 60000,
        y: 60000,
        inside: true,
    };

    /// As the host turns a place back into a pixel.
    fn denormalize(v: u16, len: u32) -> u32 {
        (u64::from(v) * u64::from(len.max(1) - 1) / 65535) as u32
    }

    #[test]
    fn a_pixel_named_is_the_pixel_the_host_gets() {
        for len in [2, 100, 1080, 1366, 1600, 1920, 2560, 3840, 5120] {
            for pixel in 0..len {
                let place = crate::control::place(pixel, len);
                assert_eq!(denormalize(place, len), pixel, "of {len}");
            }
        }
        assert_eq!(crate::control::place(0, 1), 0);
    }

    #[test]
    fn a_click_by_command_lands_on_the_pixel_named() {
        let (mut app, mut rx) = with_mouse();
        let button = MouseButton::Right;
        app.command(Ok(Command::Click {
            x: 30,
            y: 40,
            button,
        }));
        // Asked later, answered later.
        app.command(Ok(Command::Size));
        let request = u64::MAX - 1;
        assert_eq!(sent(&mut rx), [ClientMessage::PointerSync { request }]);
        assert!(app.test.answers.is_empty());

        let position = ANCHOR;
        app.host_message(ServerMessage::PointerAnchor { request, position });
        let (x, y) = (
            crate::control::place(30, 100),
            crate::control::place(40, 100),
        );
        let events = [
            InputEvent::MouseMove { x, y },
            InputEvent::MouseButton {
                button,
                pressed: true,
            },
            InputEvent::MouseButton {
                button,
                pressed: false,
            },
        ];
        let events = events.map(|event| ClientMessage::MouseInput { epoch: 7, event });
        assert_eq!(sent(&mut rx), events);
        assert_eq!(app.test.answers, ["ok click 30 40", "ok 100x100"]);
        // The person's own pointer is where it was: no handoff of theirs.
        assert_eq!(app.pointer.epoch(), None);
    }

    #[test]
    fn a_drag_by_command_is_press_move_release() {
        let (mut app, mut rx) = with_mouse();
        let button = MouseButton::Left;
        app.command(Ok(Command::Press {
            x: 10,
            y: 10,
            button,
        }));
        app.command(Ok(Command::Move { x: 50, y: 10 }));
        app.command(Ok(Command::Release { button }));
        app.command(Ok(Command::Wheel { lines: -2 }));
        // One at a time: each asks the host once the one before is done.
        let mut events = Vec::new();
        for asked in 1..=4 {
            let request = u64::MAX - asked;
            let mut now = sent(&mut rx);
            assert_eq!(now.pop(), Some(ClientMessage::PointerSync { request }));
            events.extend(now);
            let position = ANCHOR;
            app.host_message(ServerMessage::PointerAnchor { request, position });
        }
        events.extend(sent(&mut rx));
        let place = |pixel| crate::control::place(pixel, 100);
        let expected = [
            InputEvent::MouseMove {
                x: place(10),
                y: place(10),
            },
            InputEvent::MouseButton {
                button,
                pressed: true,
            },
            InputEvent::MouseMove {
                x: place(50),
                y: place(10),
            },
            InputEvent::MouseButton {
                button,
                pressed: false,
            },
            InputEvent::MouseWheel { dx: 0, dy: -240 },
        ];
        let expected = expected.map(|event| ClientMessage::MouseInput { epoch: 7, event });
        assert_eq!(events, expected);
        let answers = [
            "ok press 10 10",
            "ok move 50 10",
            "ok release",
            "ok wheel -2",
        ];
        assert_eq!(app.test.answers, answers);
    }

    #[test]
    fn a_mouse_command_that_cannot_be_done_says_why() {
        let (mut app, mut rx) = with_mouse();
        let button = MouseButton::Left;
        app.command(Ok(Command::Click {
            x: 100,
            y: 40,
            button,
        }));
        assert_eq!(
            app.test.answers,
            ["error: 100,40 is outside: the screen is 100x100"]
        );

        // The host's pointer is on a screen that is not shared.
        app.test.answers.clear();
        app.command(Ok(Command::Click { x: 1, y: 1, button }));
        let request = u64::MAX - 1;
        let position = PointerPosition {
            inside: false,
            ..ANCHOR
        };
        app.host_message(ServerMessage::PointerAnchor { request, position });
        assert!(app.test.answers[0].starts_with("error: the host's pointer is"));

        // The host does not answer: mouse control is not allowed there.
        app.test.answers.clear();
        app.command(Ok(Command::Click { x: 1, y: 1, button }));
        app.command(Ok(Command::Size));
        let asked = Instant::now();
        app.give_up(asked + Duration::from_millis(1900));
        assert!(app.test.answers.is_empty());
        app.give_up(asked + Duration::from_millis(2100));
        assert!(app.test.answers[0].starts_with("error: the host did not say"));
        assert_eq!(app.test.answers[1], "ok 100x100");
        // An answer that comes after all is for nobody.
        let _ = sent(&mut rx);
        let (request, position) = (u64::MAX - 2, ANCHOR);
        app.host_message(ServerMessage::PointerAnchor { request, position });
        assert_eq!(sent(&mut rx), []);

        // Turned off at the viewer.
        app.test.answers.clear();
        app.settings.mouse = false;
        app.command(Ok(Command::Move { x: 1, y: 1 }));
        assert!(app.test.answers[0].starts_with("error: mouse control is off"));
        assert_eq!(sent(&mut rx), []);
    }

    #[test]
    fn quit_waits_its_turn() {
        let (mut app, mut rx) = with_mouse();
        let button = MouseButton::Left;
        app.command(Ok(Command::Click { x: 1, y: 1, button }));
        app.command(Ok(Command::Quit));
        assert!(!app.test.quit, "the click is not done yet");
        let (request, position) = (u64::MAX - 1, ANCHOR);
        app.host_message(ServerMessage::PointerAnchor { request, position });
        assert!(app.test.quit);
        assert_eq!(app.test.answers, ["ok click 1 1", "ok quit"]);
        assert_eq!(sent(&mut rx).len(), 4);
        // What comes after it is not done.
        app.command(Ok(Command::Size));
        assert_eq!(app.test.answers.len(), 2);
    }

    #[test]
    fn keys_by_command_go_by_scan_code() {
        let (mut app, mut rx) = with_mouse();
        let scancode = 0xE04D;
        app.command(Ok(Command::Key {
            scancode,
            pressed: None,
        }));
        app.command(Ok(Command::Key {
            scancode: 0x2A,
            pressed: Some(true),
        }));
        app.command(Err("unknown command \"fly\"".into()));
        let keys = [(scancode, true), (scancode, false), (0x2A, true)];
        let keys = keys.map(|(scancode, pressed)| InputEvent::Key { scancode, pressed });
        assert_eq!(sent(&mut rx), keys.map(ClientMessage::Input));
        let answers = [
            "ok key E04D",
            "ok key 2A down",
            "error: unknown command \"fly\"",
        ];
        assert_eq!(app.test.answers, answers);
    }

    #[test]
    fn the_picture_is_told_of_and_cropped_by_command() {
        let (mut app, _rx) = with_mouse();
        app.command(Ok(Command::Frames));
        assert_eq!(app.test.answers, ["ok 0 frames"]);
        {
            let mut picture = app.picture.lock().unwrap();
            (picture.width, picture.height) = (4, 2);
            picture.pixels = vec![0x00FF8040; 8];
            picture.frames = 12;
            picture.decoded = Some(Instant::now());
        }
        app.test.answers.clear();
        app.command(Ok(Command::Size));
        app.command(Ok(Command::Frames));
        let file = std::env::temp_dir().join(format!("tidedesk-app-{}.png", std::process::id()));
        let area = crate::control::Area {
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let scale = crate::control::Scale::Times(3);
        app.command(Ok(Command::Crop {
            area,
            file: file.clone(),
            scale,
        }));
        assert_eq!(app.test.answers[0], "ok 4x2");
        assert!(app.test.answers[1].starts_with("ok 12 frames, the last "));
        assert_eq!(app.test.answers[2], format!("ok 6x6 {}", file.display()));
        assert!(std::fs::metadata(&file).unwrap().len() > 0);
        let _ = std::fs::remove_file(&file);
    }

    #[cfg(windows)]
    #[test]
    fn characters_from_tools_are_typed_on_the_host() {
        let (mut app, mut rx) = app();
        let none = HashSet::new();
        // Whatever this computer's layout is, it has keys for these.
        for character in ['a', 'Q', '7', ' ', '\r'] {
            let keys = crate::keys::typed(character, &none);
            assert!(!keys.is_empty(), "{character:?}");
            app.key(typed(&character.to_string(), true));
            app.key(typed(&character.to_string(), false));
            for key in keys {
                let sent = rx.try_recv().unwrap();
                assert_eq!(sent, ClientMessage::Input(key), "{character:?}");
            }
            assert!(rx.try_recv().is_err(), "{character:?}");
        }
        assert!(app.held_keys.is_empty());

        // A Shift that is held on the keyboard stays held on the host.
        const SHIFT: u16 = 0x2A;
        app.held_keys.insert(SHIFT);
        app.key(typed("Q", true));
        let is_shift = |sent: &ClientMessage| matches!(sent, ClientMessage::Input(InputEvent::Key { scancode, .. }) if *scancode == SHIFT);
        let sent: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(!sent.iter().any(is_shift), "{sent:?}");

        // Not into a window that has not got the focus.
        app.focused = false;
        app.key(typed("a", true));
        assert!(rx.try_recv().is_err());
    }

    /// With the Settings shortcut moved, Ctrl+Alt+S goes to the host like any
    /// other keys, and the title names the shortcut as set.
    #[test]
    fn a_changed_settings_shortcut_frees_ctrl_alt_s_for_the_host() {
        let (mut app, mut rx) = app();
        app.settings.settings_shortcut.key = "KeyO".into();
        assert!(
            app.window_title().contains("Settings Ctrl+Alt+O"),
            "{}",
            app.window_title()
        );
        app.modifiers = ModifiersState::CONTROL | ModifiersState::ALT;
        for pressed in [true, false] {
            app.key(KeyPress {
                physical: PhysicalKey::Code(KeyCode::KeyS),
                logical: &Key::Character("s".into()),
                text: None,
                pressed,
                repeat: false,
            });
        }
        let s = |pressed| {
            ClientMessage::Input(InputEvent::Key {
                scancode: 0x1F,
                pressed,
            })
        };
        assert_eq!(sent(&mut rx), [s(true), s(false)]);
        assert!(app.settings_window.is_none());
    }

    #[test]
    fn an_arrow_from_a_tool_is_an_arrow_on_the_host() {
        let (mut app, mut rx) = app();
        for pressed in [true, false] {
            // As Windows names the key of a right arrow without a scan code.
            app.key(KeyPress {
                physical: PhysicalKey::Code(KeyCode::Numpad6),
                logical: &Key::Named(NamedKey::ArrowRight),
                text: None,
                pressed,
                repeat: false,
            });
            let scancode = 0xE04D;
            let key = InputEvent::Key { scancode, pressed };
            assert_eq!(rx.try_recv().unwrap(), ClientMessage::Input(key));
        }
        assert!(rx.try_recv().is_err());
    }

    /// The picture stands still when the host has gone, as it does on a
    /// screen where nothing moves: the window says which it is.
    #[test]
    fn the_window_says_when_the_host_does_not_answer() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            "office | TideDesk".into(),
            (1920, 1080),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
            Arc::new(AtomicBool::new(false)),
        );
        let usual = app.window_title();
        assert!(
            usual.starts_with("office | TideDesk | Game Boost"),
            "{usual}"
        );
        app.host_silent(Some(Duration::from_millis(5400)));
        let silent = app.window_title();
        assert!(
            silent.starts_with("office | TideDesk | No answer from the host for 5 s | Game Boost"),
            "{silent}"
        );
        app.host_silent(None);
        assert_eq!(app.window_title(), usual);
    }

    /// The pointer put somewhere in one step and clicked at once, as
    /// tablets, pens and tools do: the host gets the click, where it was
    /// made.
    #[test]
    fn a_click_made_at_once_reaches_the_host_where_it_was_made() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            "test".into(),
            (100, 100),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
            Arc::new(AtomicBool::new(false)),
        );
        app.settings = ViewerSettings::default();
        app.settings.mouse = true;
        app.sharing.mouse = true;
        app.focused = true;
        app.placement = Placement::fit(100, 100, 100, 100);
        app.last_cursor = Some((30.0, 40.0));
        app.button(MouseButton::Left, true);
        app.button(MouseButton::Left, false);
        assert_eq!(
            rx.try_recv().unwrap(),
            ClientMessage::PointerSync { request: 1 }
        );
        assert!(rx.try_recv().is_err());

        let position = PointerPosition {
            epoch: 7,
            x: 60000,
            y: 60000,
            inside: true,
        };
        let request = 1;
        app.host_message(ServerMessage::PointerAnchor { request, position });
        let (x, y) = app.placement.remote_coords(30.0, 40.0);
        let button = MouseButton::Left;
        for event in [
            InputEvent::MouseMove { x, y },
            InputEvent::MouseButton {
                button,
                pressed: true,
            },
            InputEvent::MouseButton {
                button,
                pressed: false,
            },
        ] {
            let sent = rx.try_recv().unwrap();
            assert_eq!(sent, ClientMessage::MouseInput { epoch: 7, event });
        }
        assert!(rx.try_recv().is_err());
        // The next click there needs no movement.
        app.button(MouseButton::Right, true);
        let event = InputEvent::MouseButton {
            button: MouseButton::Right,
            pressed: true,
        };
        let sent = rx.try_recv().unwrap();
        assert_eq!(sent, ClientMessage::MouseInput { epoch: 7, event });
        assert!(rx.try_recv().is_err());

        // A click that waited is forgotten when the mouse is let go of.
        app.release_mouse();
        assert_eq!(rx.try_recv().unwrap(), ClientMessage::ReleaseMouse);
        app.button(MouseButton::Left, true);
        let request = 2;
        assert_eq!(
            rx.try_recv().unwrap(),
            ClientMessage::PointerSync { request }
        );
        app.release_mouse();
        assert_eq!(rx.try_recv().unwrap(), ClientMessage::ReleaseMouse);
        app.host_message(ServerMessage::PointerAnchor { request, position });
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn boost_waits_for_matching_ack_and_never_enables_mouse_or_clipboard() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let boost = Arc::new(AtomicBool::new(false));
        let mut app = App::new(
            "test".into(),
            (2560, 1600),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
            boost.clone(),
        );
        app.settings = ViewerSettings {
            mouse: false,
            clipboard: false,
            game_boost: true,
            ..ViewerSettings::default()
        };
        app.held_keys.insert(17); // held W must not remain stuck across switching
        app.configure_boost();
        assert_eq!(
            rx.try_recv().unwrap(),
            ClientMessage::Input(InputEvent::Key {
                scancode: 17,
                pressed: false,
            })
        );
        assert_eq!(rx.try_recv().unwrap(), ClientMessage::ReleaseMouse);
        assert_eq!(
            rx.try_recv().unwrap(),
            ClientMessage::SetGameBoost {
                request: 1,
                enabled: true,
            }
        );
        assert!(rx.try_recv().is_err());
        assert!(!boost.load(Ordering::Relaxed));
        let status = StreamingStatus::requested(1, true, 30, 8_000_000);
        app.host_message(ServerMessage::Streaming(StreamingStatus {
            request: 0,
            ..status
        }));
        assert!(!boost.load(Ordering::Relaxed));
        app.host_message(ServerMessage::Streaming(status));
        assert!(boost.load(Ordering::Relaxed));
        assert!(!app.mouse_enabled());
        assert!(!app.clipboard_enabled());
        assert_eq!(app.remote_size, (2560, 1600));
        app.settings.game_boost = false;
        app.configure_boost();
        assert!(!boost.load(Ordering::Relaxed));
        app.host_message(ServerMessage::Streaming(status)); // late enable acknowledgement
        assert!(!boost.load(Ordering::Relaxed));
        assert!(app.boost_status.is_none());
        let desktop = StreamingStatus::requested(2, false, 30, 8_000_000);
        app.host_message(ServerMessage::Streaming(desktop));
        assert_eq!(app.boost_status, Some(desktop));
    }

    #[test]
    fn cursor_telemetry_is_display_only_with_control_off_or_on() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            "test".into(),
            (100, 100),
            Arc::new(Mutex::new(Picture::default())),
            tx,
            WindowMemory::default(),
            Arc::new(AtomicBool::new(false)),
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
