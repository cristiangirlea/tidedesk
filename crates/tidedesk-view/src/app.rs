//! The viewer window: presents pictures and turns local input into events.

use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use tidedesk_core::protocol::{ClientMessage, InputEvent, MouseButton};
use tokio::sync::mpsc::UnboundedSender;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::platform::scancode::PhysicalKeyExtScancode;
use winit::window::{Window, WindowId};

use crate::layout::{self, Placement};
use crate::stream::{Picture, UiEvent};

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
    placement: Placement,
    held_keys: HashSet<u16>,
    pub exit_message: Option<String>,
}

impl App {
    pub fn new(
        title: String,
        remote_size: (u32, u32),
        picture: Arc<Mutex<Picture>>,
        control: UnboundedSender<ClientMessage>,
    ) -> Self {
        Self {
            title,
            remote_size,
            picture,
            control,
            surface: None,
            placement: Placement::fit(0, 0, 0, 0),
            held_keys: HashSet::new(),
            exit_message: None,
        }
    }

    fn send(&self, event: InputEvent) {
        let _ = self.control.send(ClientMessage::Input(event));
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
        self.placement = Placement::fit(pic.width, pic.height, size.width, size.height);
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
        let _ = buffer.present();
    }
}

impl ApplicationHandler<UiEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        // Open at the remote resolution, shrunk to fit the local screen.
        let (mut w, mut h) = self.remote_size;
        if let Some(monitor) = event_loop.primary_monitor() {
            let m = monitor.size();
            let p = Placement::fit(w, h, m.width * 9 / 10, m.height * 9 / 10);
            if p.width < w {
                (w, h) = (p.width, p.height);
            }
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(PhysicalSize::new(w.max(320), h.max(200)));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.exit_message = Some(format!("cannot open window: {e}"));
                event_loop.exit();
                return;
            }
        };
        let surface = softbuffer::Context::new(window.clone())
            .and_then(|ctx| softbuffer::Surface::new(&ctx, window.clone()));
        match surface {
            Ok(surface) => self.surface = Some(Surface { window, surface }),
            Err(e) => {
                self.exit_message = Some(format!("cannot create drawing surface: {e}"));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UiEvent) {
        match event {
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
            WindowEvent::Focused(false) => self.release_keys(),
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = self.placement.remote_coords(position.x, position.y);
                self.send(InputEvent::MouseMove { x, y });
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
                self.send(InputEvent::MouseButton {
                    button,
                    pressed: state == ElementState::Pressed,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x * 120.0) as i32, (y * 120.0) as i32),
                    MouseScrollDelta::PixelDelta(p) => (p.x as i32, p.y as i32),
                };
                self.send(InputEvent::MouseWheel { dx, dy });
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let Some(scancode) = event.physical_key.to_scancode() else {
                    return;
                };
                let scancode = scancode as u16;
                let pressed = event.state == ElementState::Pressed;
                if pressed {
                    self.held_keys.insert(scancode);
                } else {
                    self.held_keys.remove(&scancode);
                }
                // Repeats are forwarded too: injected keys don't auto-repeat.
                self.send(InputEvent::Key { scancode, pressed });
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.release_keys();
    }
}
