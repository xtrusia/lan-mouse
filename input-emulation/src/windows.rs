use super::error::{EmulationError, WindowsEmulationCreationError};
use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Event, KeyboardEvent, PointerEvent,
    scancode,
};

use async_trait::async_trait;
use std::ops::BitOrAssign;
use std::time::Duration;
use tokio::task::AbortHandle;
use windows::Win32::Foundation::GENERIC_WRITE;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetThreadDesktop, OpenInputDesktop,
    SetThreadDesktop,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT_0, KEYEVENTF_EXTENDEDKEY, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{XBUTTON1, XBUTTON2};

use super::{Emulation, EmulationHandle};

mod hid;

const DEFAULT_REPEAT_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_REPEAT_INTERVAL: Duration = Duration::from_millis(32);

pub(crate) struct WindowsEmulation {
    repeat_task: Option<AbortHandle>,
    hid_input: Option<hid::HidInput>,
}

impl WindowsEmulation {
    pub(crate) fn new(fakerinput: bool) -> Result<Self, WindowsEmulationCreationError> {
        let hid_input = if fakerinput {
            let input = hid::HidInput::open()?;
            log::info!("FakerInput HID mouse and keyboard active (VID FE0F, PID 00FF)");
            Some(input)
        } else {
            None
        };
        Ok(Self {
            repeat_task: None,
            hid_input,
        })
    }
}

#[async_trait]
impl Emulation for WindowsEmulation {
    async fn consume(&mut self, event: Event, _: EmulationHandle) -> Result<(), EmulationError> {
        if let Some(input) = &mut self.hid_input {
            match event {
                Event::Pointer(pointer) => {
                    input.consume_pointer(pointer)?;
                    return Ok(());
                }
                Event::Keyboard(keyboard) if input.consume_keyboard(keyboard)? => {
                    self.kill_repeat_task();
                    return Ok(());
                }
                _ => {}
            }
        }
        match event {
            Event::Pointer(pointer_event) => match pointer_event {
                PointerEvent::Motion { time: _, dx, dy } => {
                    rel_mouse(dx as i32, dy as i32);
                }
                PointerEvent::Button {
                    time: _,
                    button,
                    state,
                } => mouse_button(button, state),
                PointerEvent::Axis {
                    time: _,
                    axis,
                    value,
                } => scroll(axis, value as i32),
                PointerEvent::AxisDiscrete120 { axis, value } => scroll(axis, value),
            },
            Event::Keyboard(keyboard_event) => match keyboard_event {
                KeyboardEvent::Key {
                    time: _,
                    key,
                    state,
                } => {
                    match state {
                        // pressed
                        0 => self.kill_repeat_task(),
                        1 => self.spawn_repeat_task(key).await,
                        _ => {}
                    }
                    key_event(key, state)
                }
                KeyboardEvent::Modifiers { .. } => {}
            },
        }
        // FIXME
        Ok(())
    }

    async fn create(&mut self, _handle: EmulationHandle) {}

    async fn destroy(&mut self, _handle: EmulationHandle) {}

    async fn terminate(&mut self) {
        self.kill_repeat_task();
        if let Some(input) = &mut self.hid_input {
            if let Err(error) = input.release_all() {
                log::warn!("FakerInput release on termination: {error}");
            }
        }
    }
}

impl WindowsEmulation {
    async fn spawn_repeat_task(&mut self, key: u32) {
        // there can only be one repeating key and it's
        // always the last to be pressed
        self.kill_repeat_task();
        let repeat_task = tokio::task::spawn_local(async move {
            tokio::time::sleep(DEFAULT_REPEAT_DELAY).await;
            loop {
                key_event(key, 1);
                tokio::time::sleep(DEFAULT_REPEAT_INTERVAL).await;
            }
        });
        self.repeat_task = Some(repeat_task.abort_handle());
    }
    fn kill_repeat_task(&mut self) {
        if let Some(task) = self.repeat_task.take() {
            task.abort();
        }
    }
}

fn send_input_safe(input: INPUT) {
    unsafe {
        // The service runs in the console session as SYSTEM. Only this input
        // thread moves to the secure desktop; it owns no windows or hooks.
        let old_desktop = GetThreadDesktop(GetCurrentThreadId());
        let input_desktop = OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(GENERIC_WRITE.0),
        );
        if let (Ok(old), Ok(desktop)) = (&old_desktop, &input_desktop) {
            if let Err(e) = SetThreadDesktop(*desktop) {
                log::debug!("cannot attach input desktop: {e}");
            } else {
                let count = SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
                if count == 0 {
                    log::debug!("SendInput rejected event during desktop transition");
                }
                if let Err(e) = SetThreadDesktop(*old) {
                    log::error!("cannot restore input desktop: {e}");
                }
                let _ = CloseDesktop(*desktop);
                return;
            }
        }
        if let Ok(desktop) = input_desktop {
            let _ = CloseDesktop(desktop);
        }
        // A desktop transition must never spin and block the network event loop.
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

fn send_mouse_input(mi: MOUSEINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi },
    });
}

fn send_keyboard_input(ki: KEYBDINPUT) {
    send_input_safe(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki },
    });
}
fn rel_mouse(dx: i32, dy: i32) {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: 0,
        dwFlags: MOUSEEVENTF_MOVE,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn mouse_button(button: u32, state: u32) {
    let dw_flags = match state {
        0 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTUP,
            BTN_RIGHT => MOUSEEVENTF_RIGHTUP,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEUP,
            BTN_BACK => MOUSEEVENTF_XUP,
            BTN_FORWARD => MOUSEEVENTF_XUP,
            _ => return,
        },
        1 => match button {
            BTN_LEFT => MOUSEEVENTF_LEFTDOWN,
            BTN_RIGHT => MOUSEEVENTF_RIGHTDOWN,
            BTN_MIDDLE => MOUSEEVENTF_MIDDLEDOWN,
            BTN_BACK => MOUSEEVENTF_XDOWN,
            BTN_FORWARD => MOUSEEVENTF_XDOWN,
            _ => return,
        },
        _ => return,
    };
    let mouse_data = match button {
        BTN_BACK => XBUTTON1 as u32,
        BTN_FORWARD => XBUTTON2 as u32,
        _ => 0,
    };
    let mi = MOUSEINPUT {
        dx: 0,
        dy: 0, // no movement
        mouseData: mouse_data,
        dwFlags: dw_flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_mouse_input(mi);
}

fn scroll_input(axis: u8, value: i32) -> Option<MOUSEINPUT> {
    let (event_type, delta) = match axis {
        0 => (MOUSEEVENTF_WHEEL, value.wrapping_neg()),
        1 => (MOUSEEVENTF_HWHEEL, value),
        _ => return None,
    };
    Some(MOUSEINPUT {
        dx: 0,
        dy: 0,
        mouseData: delta as u32,
        dwFlags: event_type,
        time: 0,
        dwExtraInfo: 0,
    })
}

fn scroll(axis: u8, value: i32) {
    if let Some(input) = scroll_input(axis, value) {
        send_mouse_input(input);
    }
}

#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn scroll_axes_match_windows_direction_conventions() {
        for value in [-120, 120] {
            let vertical = scroll_input(0, value).unwrap();
            assert_eq!(vertical.dwFlags, MOUSEEVENTF_WHEEL);
            assert_eq!(vertical.mouseData as i32, -value);
            let horizontal = scroll_input(1, value).unwrap();
            assert_eq!(horizontal.dwFlags, MOUSEEVENTF_HWHEEL);
            assert_eq!(horizontal.mouseData as i32, value);
        }
        assert!(scroll_input(2, 120).is_none());
    }
}

fn key_event(key: u32, state: u8) {
    let scancode = match linux_keycode_to_windows_scancode(key) {
        Some(code) => code,
        None => return,
    };
    let extended = scancode > 0xff;
    let scancode = scancode & 0xff;
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags.bitor_assign(KEYEVENTF_EXTENDEDKEY);
    }
    if state == 0 {
        flags.bitor_assign(KEYEVENTF_KEYUP);
    }
    let ki = KEYBDINPUT {
        wVk: Default::default(),
        wScan: scancode,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    send_keyboard_input(ki);
}

fn linux_keycode_to_windows_scancode(linux_keycode: u32) -> Option<u16> {
    let linux_scancode = match scancode::Linux::try_from(linux_keycode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("unknown keycode: {linux_keycode}");
            return None;
        }
    };
    log::trace!("linux code: {linux_scancode:?}");
    let windows_scancode = match scancode::Windows::try_from(linux_scancode) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("failed to translate linux code into windows scancode: {linux_scancode:?}");
            return None;
        }
    };
    log::trace!("windows code: {windows_scancode:?}");
    Some(windows_scancode as u16)
}
