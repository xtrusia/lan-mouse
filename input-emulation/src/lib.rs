use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
};

use input_event::{Event, KeyboardEvent, PointerEvent};

pub use self::error::{EmulationCreationError, EmulationError, InputEmulationError};

#[cfg(windows)]
mod windows;

#[cfg(x11)]
mod x11;

#[cfg(wlroots)]
mod wlroots;

#[cfg(rdp)]
mod xdg_desktop_portal;

#[cfg(libei)]
mod libei;

#[cfg(target_os = "macos")]
mod macos;

/// fallback input emulation (logs events)
mod dummy;
mod error;

pub type EmulationHandle = u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    #[cfg(wlroots)]
    Wlroots,
    #[cfg(libei)]
    Libei,
    #[cfg(rdp)]
    Xdp,
    #[cfg(x11)]
    X11,
    #[cfg(windows)]
    Windows,
    #[cfg(windows)]
    WindowsFakerInput,
    #[cfg(target_os = "macos")]
    MacOs,
    Dummy,
}

impl Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(wlroots)]
            Backend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei)]
            Backend::Libei => write!(f, "libei"),
            #[cfg(rdp)]
            Backend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11)]
            Backend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            Backend::Windows => write!(f, "windows"),
            #[cfg(windows)]
            Backend::WindowsFakerInput => write!(f, "windows-fakerinput"),
            #[cfg(target_os = "macos")]
            Backend::MacOs => write!(f, "macos"),
            Backend::Dummy => write!(f, "dummy"),
        }
    }
}

pub struct InputEmulation {
    emulation: Box<dyn Emulation>,
    handles: HashSet<EmulationHandle>,
    pressed_keys: HashMap<EmulationHandle, HashSet<u32>>,
    pressed_buttons: HashMap<EmulationHandle, HashSet<u32>>,
}

impl InputEmulation {
    async fn with_backend(backend: Backend) -> Result<InputEmulation, EmulationCreationError> {
        let emulation: Box<dyn Emulation> = match backend {
            #[cfg(wlroots)]
            Backend::Wlroots => Box::new(wlroots::WlrootsEmulation::new()?),
            #[cfg(libei)]
            Backend::Libei => Box::new(libei::LibeiEmulation::new().await?),
            #[cfg(x11)]
            Backend::X11 => Box::new(x11::X11Emulation::new()?),
            #[cfg(rdp)]
            Backend::Xdp => Box::new(xdg_desktop_portal::DesktopPortalEmulation::new().await?),
            #[cfg(windows)]
            Backend::Windows => Box::new(windows::WindowsEmulation::new(false)?),
            #[cfg(windows)]
            Backend::WindowsFakerInput => Box::new(windows::WindowsEmulation::new(true)?),
            #[cfg(target_os = "macos")]
            Backend::MacOs => Box::new(macos::MacOSEmulation::new()?),
            Backend::Dummy => Box::new(dummy::DummyEmulation::new()),
        };
        Ok(Self {
            emulation,
            handles: HashSet::new(),
            pressed_keys: HashMap::new(),
            pressed_buttons: HashMap::new(),
        })
    }

    pub async fn new(backend: Option<Backend>) -> Result<InputEmulation, EmulationCreationError> {
        if let Some(backend) = backend {
            let b = Self::with_backend(backend).await;
            if b.is_ok() {
                log::info!("using emulation backend: {backend}");
            }
            return b;
        }

        for backend in [
            #[cfg(wlroots)]
            Backend::Wlroots,
            #[cfg(libei)]
            Backend::Libei,
            #[cfg(rdp)]
            Backend::Xdp,
            #[cfg(x11)]
            Backend::X11,
            #[cfg(windows)]
            Backend::Windows,
            #[cfg(target_os = "macos")]
            Backend::MacOs,
            Backend::Dummy,
        ] {
            match Self::with_backend(backend).await {
                Ok(b) => {
                    log::info!("using emulation backend: {backend}");
                    return Ok(b);
                }
                Err(e) if e.cancelled_by_user() => return Err(e),
                Err(e) => log::warn!("{e}"),
            }
        }

        Err(EmulationCreationError::NoAvailableBackend)
    }

    pub async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError> {
        let forward = match event {
            Event::Keyboard(KeyboardEvent::Key { key, state, .. }) => {
                Self::update_pressed(&mut self.pressed_keys, handle, key, state != 0)
            }
            Event::Pointer(PointerEvent::Button { button, state, .. }) => {
                if state <= 1 {
                    Self::update_pressed(&mut self.pressed_buttons, handle, button, state == 1);
                }
                true
            }
            _ => true,
        };
        if forward {
            self.emulation.consume(event, handle).await?;
        }
        Ok(())
    }

    pub async fn create(&mut self, handle: EmulationHandle) -> bool {
        if self.handles.insert(handle) {
            self.pressed_keys.insert(handle, HashSet::new());
            self.pressed_buttons.insert(handle, HashSet::new());
            self.emulation.create(handle).await;
            true
        } else {
            false
        }
    }

    pub async fn destroy(&mut self, handle: EmulationHandle) {
        let _ = self.release_keys(handle).await;
        if self.handles.remove(&handle) {
            self.pressed_keys.remove(&handle);
            self.pressed_buttons.remove(&handle);
            self.emulation.destroy(handle).await
        }
    }

    pub async fn terminate(&mut self) {
        for handle in self.handles.iter().cloned().collect::<Vec<_>>() {
            self.destroy(handle).await
        }
        self.emulation.terminate().await
    }

    pub async fn release_keys(&mut self, handle: EmulationHandle) -> Result<(), EmulationError> {
        let mut released = Vec::new();
        if let Some(keys) = self.pressed_keys.get_mut(&handle) {
            released.extend(keys.drain().map(|key| {
                Event::Keyboard(KeyboardEvent::Key {
                    time: 0,
                    key,
                    state: 0,
                })
            }));
        }
        if let Some(buttons) = self.pressed_buttons.get_mut(&handle) {
            released.extend(buttons.drain().map(|button| {
                Event::Pointer(PointerEvent::Button {
                    time: 0,
                    button,
                    state: 0,
                })
            }));
        }
        for event in released {
            self.emulation.consume(event, handle).await?;
            log::warn!("releasing held input: {event}");
        }

        let event = Event::Keyboard(KeyboardEvent::Modifiers {
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
        });
        self.emulation.consume(event, handle).await?;
        Ok(())
    }

    pub fn has_pressed_keys(&self, handle: EmulationHandle) -> bool {
        self.pressed_keys
            .get(&handle)
            .is_some_and(|p| !p.is_empty())
    }

    /// Update held keys or buttons for a client.
    /// returns whether the event should be processed
    fn update_pressed(
        inputs: &mut HashMap<EmulationHandle, HashSet<u32>>,
        handle: EmulationHandle,
        code: u32,
        down: bool,
    ) -> bool {
        let Some(pressed) = inputs.get_mut(&handle) else {
            return false;
        };
        if down {
            pressed.insert(code)
        } else {
            pressed.remove(&code)
        }
    }
}

#[async_trait]
trait Emulation: Send {
    async fn consume(
        &mut self,
        event: Event,
        handle: EmulationHandle,
    ) -> Result<(), EmulationError>;
    async fn create(&mut self, handle: EmulationHandle);
    async fn destroy(&mut self, handle: EmulationHandle);
    async fn terminate(&mut self);
}

#[cfg(test)]
mod button_release_tests {
    use super::*;
    use input_event::{BTN_LEFT, BTN_RIGHT, PointerEvent};
    use std::sync::{Arc, Mutex};

    struct Recorder(Arc<Mutex<Vec<Event>>>);

    #[async_trait]
    impl Emulation for Recorder {
        async fn consume(
            &mut self,
            event: Event,
            _: EmulationHandle,
        ) -> Result<(), EmulationError> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
        async fn create(&mut self, _: EmulationHandle) {}
        async fn destroy(&mut self, _: EmulationHandle) {}
        async fn terminate(&mut self) {}
    }

    fn button(button: u32, state: u32) -> Event {
        Event::Pointer(PointerEvent::Button {
            time: 0,
            button,
            state,
        })
    }

    async fn recorder() -> (InputEmulation, Arc<Mutex<Vec<Event>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut emulation = InputEmulation::new(Some(Backend::Dummy)).await.unwrap();
        emulation.emulation = Box::new(Recorder(events.clone()));
        (emulation, events)
    }

    #[tokio::test]
    async fn disconnect_releases_held_mouse_button() {
        let (mut emulation, events) = recorder().await;
        emulation.create(1).await;
        emulation.consume(button(BTN_LEFT, 1), 1).await.unwrap();
        emulation.destroy(1).await;
        assert!(events.lock().unwrap().contains(&button(BTN_LEFT, 0)));
    }

    #[tokio::test]
    async fn already_released_button_is_not_released_again() {
        let (mut emulation, events) = recorder().await;
        emulation.create(1).await;
        emulation.consume(button(BTN_LEFT, 1), 1).await.unwrap();
        emulation.consume(button(BTN_LEFT, 0), 1).await.unwrap();
        emulation.destroy(1).await;
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| **e == button(BTN_LEFT, 0))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn untracked_button_release_is_forwarded() {
        let (mut emulation, events) = recorder().await;
        emulation.create(1).await;
        emulation.consume(button(BTN_LEFT, 0), 1).await.unwrap();
        assert!(events.lock().unwrap().contains(&button(BTN_LEFT, 0)));
    }

    #[tokio::test]
    async fn termination_releases_each_clients_mouse_buttons() {
        let (mut emulation, events) = recorder().await;
        emulation.create(1).await;
        emulation.create(2).await;
        emulation.consume(button(BTN_LEFT, 1), 1).await.unwrap();
        emulation.consume(button(BTN_RIGHT, 1), 2).await.unwrap();
        emulation.terminate().await;
        let events = events.lock().unwrap();
        assert!(events.contains(&button(BTN_LEFT, 0)));
        assert!(events.contains(&button(BTN_RIGHT, 0)));
    }
}
