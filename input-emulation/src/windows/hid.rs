use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::mem::size_of;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::AsRawHandle;

use input_event::{
    BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KeyboardEvent, PointerEvent,
    scancode::Linux,
};
use keycode::{KeyMap, KeyMapping};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, HDEVINFO, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidP_GetCaps, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, HANDLE};
use windows::core::{HRESULT, PCWSTR};

// FakerInput v0.1.1: fakerinputcommon.h and fakerinputclient.cpp.
const REPORT_SIZE: usize = 65;
const VENDOR: u16 = 0xfe0f;
const PRODUCT: u16 = 0x00ff;

pub(super) struct HidInput {
    control: File,
    _method: File,
    buttons: u8,
    wheel_remainder: [i32; 2],
    keyboard: KeyboardState,
}

impl HidInput {
    pub(super) fn open() -> io::Result<Self> {
        let control = open_collection(1)?;
        let mut method = open_collection(2)?;
        let mut version = [0; REPORT_SIZE];
        version[0] = 0x41;
        version[4..8].copy_from_slice(&1u32.to_le_bytes());
        let _ = method.write(&version)?;
        let mut mouse = Self {
            control,
            _method: method,
            buttons: 0,
            wheel_remainder: [0; 2],
            keyboard: KeyboardState::default(),
        };
        mouse.release_all()?;
        Ok(mouse)
    }

    pub(super) fn consume_pointer(&mut self, event: PointerEvent) -> io::Result<()> {
        match event {
            PointerEvent::Motion { dx, dy, .. } => {
                self.send(self.buttons, motion(dx)?, motion(dy)?, 0, 0)?;
            }
            PointerEvent::Button { button, state, .. } => {
                let bit = match button {
                    BTN_LEFT => 1,
                    BTN_RIGHT => 2,
                    BTN_MIDDLE => 4,
                    BTN_BACK => 8,
                    BTN_FORWARD => 16,
                    _ => return Ok(()),
                };
                let buttons = match state {
                    0 => self.buttons & !bit,
                    1 => self.buttons | bit,
                    _ => return Ok(()),
                };
                self.send(buttons, 0, 0, 0, 0)?;
                self.buttons = buttons;
            }
            PointerEvent::Axis { axis, value, .. } => self.scroll(axis, value as i32)?,
            PointerEvent::AxisDiscrete120 { axis, value } => self.scroll(axis, value)?,
        }
        Ok(())
    }

    pub(super) fn consume_keyboard(&mut self, event: KeyboardEvent) -> io::Result<bool> {
        let KeyboardEvent::Key { key, state, .. } = event else {
            return Ok(true);
        };
        let Some(key) = hid_key(key) else {
            return Ok(false);
        };
        // Windows repeats held HID keys; synthetic repeat events are unnecessary.
        if state > 1 {
            return Ok(true);
        }
        let mut next = self.keyboard.clone();
        next.update(key, state == 1);
        let packet = match key {
            HidKey::Consumer(_) => next.consumer_report(),
            _ => next.report(),
        };
        self.write_report(packet)?;
        self.keyboard = next;
        Ok(true)
    }

    pub(super) fn release_all(&mut self) -> io::Result<()> {
        self.send(0, 0, 0, 0, 0)?;
        self.write_report(KeyboardState::default().report())?;
        self.write_report(KeyboardState::default().consumer_report())?;
        self.buttons = 0;
        self.keyboard = KeyboardState::default();
        self.wheel_remainder = [0; 2];
        Ok(())
    }

    fn scroll(&mut self, axis: u8, value: i32) -> io::Result<()> {
        if axis > 1 {
            return Ok(());
        }
        let value = if axis == 0 {
            value.wrapping_neg()
        } else {
            value
        };
        let (ticks, remainder) = wheel_ticks(self.wheel_remainder[axis as usize], value)?;
        if ticks != 0 {
            let (vertical, horizontal) = if axis == 0 { (ticks, 0) } else { (0, ticks) };
            self.send(self.buttons, 0, 0, vertical, horizontal)?;
        }
        self.wheel_remainder[axis as usize] = remainder;
        Ok(())
    }

    fn send(&mut self, buttons: u8, dx: i16, dy: i16, wheel: i8, horizontal: i8) -> io::Result<()> {
        self.write_report(report(buttons, dx, dy, wheel, horizontal))
    }

    fn write_report(&mut self, packet: [u8; REPORT_SIZE]) -> io::Result<()> {
        // The driver returns its payload length, not the 65-byte transport length.
        let _ = self.control.write(&packet)?;
        Ok(())
    }
}

impl Drop for HidInput {
    fn drop(&mut self) {
        if let Err(error) = self.release_all() {
            log::debug!("FakerInput release on close: {error}");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum HidKey {
    Modifier(u8),
    Key(u8),
    Consumer(u32),
}

fn hid_key(key: u32) -> Option<HidKey> {
    let consumer = match Linux::try_from(key).ok()? {
        Linux::KeyNextsong => 1 << 0,
        Linux::KeyPrevioussong => 1 << 1,
        Linux::KeyStopcd => 1 << 2,
        Linux::KeyPlaypause => 1 << 3,
        Linux::KeyMute => 1 << 4,
        Linux::KeyVolumeDown => 1 << 5,
        Linux::KeyVolumeUp => 1 << 6,
        Linux::KeyEjectcd => 1 << 7,
        Linux::KeyComputer => 1 << 8,
        Linux::KeyCalc => 1 << 9,
        Linux::KeyWww => 1 << 10,
        Linux::KeyHomepage => 1 << 11,
        Linux::KeyBack => 1 << 12,
        Linux::KeyForward => 1 << 13,
        Linux::KeyConfig => 1 << 14,
        Linux::KeyMail => 1 << 15,
        Linux::KeyStop => 1 << 16,
        Linux::KeyRefresh => 1 << 17,
        Linux::KeyBookmarks => 1 << 18,
        Linux::KeyFastforward => 1 << 19,
        Linux::KeyRewind => 1 << 20,
        _ => 0,
    };
    if consumer != 0 {
        return Some(HidKey::Consumer(consumer));
    }
    let mapping = KeyMap::from_key_mapping(KeyMapping::Evdev(u16::try_from(key).ok()?)).ok()?;
    if let Some(modifier) = mapping.modifier {
        return Some(HidKey::Modifier(modifier.bits()));
    }
    // The installed driver's keyboard descriptor ends at usage 0x65.
    if !(0x04..=0x65).contains(&mapping.usb)
        || KeyMap::from_usb_code(0x07, mapping.usb).ok()?.id != mapping.id
    {
        return None;
    }
    Some(HidKey::Key(mapping.usb as u8))
}

#[derive(Clone, Default)]
struct KeyboardState {
    modifiers: u8,
    keys: BTreeSet<u8>,
    consumer: u32,
}

impl KeyboardState {
    fn update(&mut self, key: HidKey, down: bool) {
        match key {
            HidKey::Modifier(mask) => {
                if down {
                    self.modifiers |= mask;
                } else {
                    self.modifiers &= !mask;
                }
            }
            HidKey::Consumer(mask) => {
                if down {
                    self.consumer |= mask;
                } else {
                    self.consumer &= !mask;
                }
            }
            HidKey::Key(key) => {
                if down {
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
            }
        }
    }

    fn report(&self) -> [u8; REPORT_SIZE] {
        let mut packet = [0; REPORT_SIZE];
        packet[..4].copy_from_slice(&[0x40, 9, 1, self.modifiers]);
        if self.keys.len() > 6 {
            packet[5..11].fill(1); // HID ErrorRollOver
        } else {
            for (slot, key) in packet[5..11].iter_mut().zip(&self.keys) {
                *slot = *key;
            }
        }
        packet
    }

    fn consumer_report(&self) -> [u8; REPORT_SIZE] {
        let mut packet = [0; REPORT_SIZE];
        packet[..3].copy_from_slice(&[0x40, 4, 2]);
        packet[3..6].copy_from_slice(&self.consumer.to_le_bytes()[..3]);
        packet
    }
}

fn motion(value: f64) -> io::Result<i16> {
    let value = value as i32;
    if !(-32767..=32767).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "motion exceeds HID report range",
        ));
    }
    Ok(value as i16)
}

fn wheel_ticks(remainder: i32, value: i32) -> io::Result<(i8, i32)> {
    let total = remainder
        .checked_add(value)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "wheel overflow"))?;
    let ticks = i8::try_from(total / 120).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "wheel exceeds HID report range",
        )
    })?;
    Ok((ticks, total % 120))
}

fn report(buttons: u8, dx: i16, dy: i16, wheel: i8, horizontal: i8) -> [u8; REPORT_SIZE] {
    let mut report = [0; REPORT_SIZE];
    report[..4].copy_from_slice(&[0x40, 8, 3, buttons]);
    report[4..6].copy_from_slice(&dx.to_le_bytes());
    report[6..8].copy_from_slice(&dy.to_le_bytes());
    report[8] = wheel as u8;
    report[9] = horizontal as u8;
    report
}

struct DeviceList(HDEVINFO);
impl Drop for DeviceList {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn open_collection(usage: u16) -> io::Result<File> {
    unsafe {
        let guid = HidD_GetHidGuid();
        let devices = DeviceList(
            SetupDiGetClassDevsW(
                Some(&guid),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
            .map_err(io::Error::other)?,
        );
        for index in 0.. {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if let Err(error) =
                SetupDiEnumDeviceInterfaces(devices.0, None, &guid, index, &mut data)
            {
                if error.code() == HRESULT::from_win32(ERROR_NO_MORE_ITEMS.0) {
                    break;
                }
                return Err(io::Error::other(error));
            }
            let mut size = 0;
            let _ =
                SetupDiGetDeviceInterfaceDetailW(devices.0, &data, None, 0, Some(&mut size), None);
            if size < 8 {
                return Err(io::Error::last_os_error());
            }
            let mut storage = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            let detail = &mut *storage
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            detail.cbSize = if size_of::<usize>() == 8 { 8 } else { 6 };
            SetupDiGetDeviceInterfaceDetailW(devices.0, &data, Some(detail), size, None, None)
                .map_err(io::Error::other)?;
            let path =
                std::slice::from_raw_parts(detail.DevicePath.as_ptr(), (size as usize - 4) / 2);
            let length = path.iter().position(|c| *c == 0).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "unterminated HID path")
            })?;
            let Ok(file) = OpenOptions::new()
                .read(true)
                .write(true)
                .open(OsString::from_wide(&path[..length]))
            else {
                continue;
            };
            let handle = HANDLE(file.as_raw_handle());
            let mut attributes = HIDD_ATTRIBUTES {
                Size: size_of::<HIDD_ATTRIBUTES>() as u32,
                ..Default::default()
            };
            if !HidD_GetAttributes(handle, &mut attributes)
                || attributes.VendorID != VENDOR
                || attributes.ProductID != PRODUCT
            {
                continue;
            }
            let mut preparsed = PHIDP_PREPARSED_DATA::default();
            if !HidD_GetPreparsedData(handle, &mut preparsed) {
                continue;
            }
            let mut caps = HIDP_CAPS::default();
            let status = HidP_GetCaps(preparsed, &mut caps);
            HidD_FreePreparsedData(preparsed);
            if status.0 >= 0 && caps.UsagePage == 0xff00 && caps.Usage == usage {
                return Ok(file);
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("FakerInput collection {usage} not found"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires the installed FakerInput driver"]
    fn connects_to_installed_fakerinput() {
        let mouse = HidInput::open().expect("connect to FakerInput and send neutral reports");
        assert_eq!(mouse.buttons, 0);
    }

    #[test]
    fn signed_motion_and_wheels_use_the_driver_report_layout() {
        let bytes = report(5, -300, 400, -1, 2);
        assert_eq!(&bytes[..10], &[0x40, 8, 3, 5, 0xd4, 0xfe, 0x90, 1, 0xff, 2]);
        assert!(bytes[10..].iter().all(|b| *b == 0));
        assert_eq!(
            &report(0, 0, 0, 0, 0)[..10],
            &[0x40, 8, 3, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn partial_wheel_steps_are_preserved() {
        let (ticks, remainder) = wheel_ticks(0, -60).unwrap();
        assert_eq!((ticks, remainder), (0, -60));
        assert_eq!(wheel_ticks(remainder, -60).unwrap(), (-1, 0));
        assert!(wheel_ticks(0, i32::MAX).is_err());
    }

    #[test]
    fn invalid_motion_cannot_wrap_direction() {
        assert_eq!(motion(-32767.0).unwrap(), -32767);
        assert!(motion(32768.0).is_err());
        assert!(motion(-32768.0).is_err());
    }

    #[test]
    fn keyboard_report_preserves_chords_and_key_releases() {
        let mut keyboard = KeyboardState::default();
        keyboard.update(hid_key(Linux::KeyLeftCtrl as u32).unwrap(), true);
        keyboard.update(hid_key(Linux::KeyRightShift as u32).unwrap(), true);
        keyboard.update(hid_key(Linux::KeyA as u32).unwrap(), true);
        keyboard.update(hid_key(Linux::KeyA as u32).unwrap(), true);
        assert_eq!(
            &keyboard.report()[..11],
            &[0x40, 9, 1, 0x21, 0, 4, 0, 0, 0, 0, 0]
        );
        keyboard.update(hid_key(Linux::KeyA as u32).unwrap(), false);
        assert_eq!(&keyboard.report()[3..11], &[0x21, 0, 0, 0, 0, 0, 0, 0]);
        keyboard.update(hid_key(Linux::KeyLeftCtrl as u32).unwrap(), false);
        keyboard.update(hid_key(Linux::KeyRightShift as u32).unwrap(), false);
        assert_eq!(&keyboard.report()[3..11], &[0; 8]);
    }

    #[test]
    fn keyboard_recovers_after_six_key_rollover() {
        let mut keyboard = KeyboardState::default();
        for key in 4..=10 {
            keyboard.update(HidKey::Key(key), true);
        }
        assert_eq!(&keyboard.report()[5..11], &[1; 6]);
        keyboard.update(HidKey::Key(10), false);
        assert_eq!(&keyboard.report()[5..11], &[4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn consumer_report_uses_the_driver_bit_order() {
        let mut keyboard = KeyboardState::default();
        keyboard.update(hid_key(Linux::KeyVolumeUp as u32).unwrap(), true);
        keyboard.update(hid_key(Linux::KeyBack as u32).unwrap(), true);
        keyboard.update(hid_key(Linux::KeyRewind as u32).unwrap(), true);
        assert_eq!(
            &keyboard.consumer_report()[..6],
            &[0x40, 4, 2, 0x40, 0x10, 0x10]
        );
        keyboard.update(hid_key(Linux::KeyVolumeUp as u32).unwrap(), false);
        assert_eq!(keyboard.consumer_report()[3], 0);
    }

    #[test]
    fn keys_outside_the_driver_descriptor_use_the_existing_backend() {
        assert_eq!(hid_key(Linux::KeyF12 as u32), Some(HidKey::Key(0x45)));
        assert_eq!(hid_key(Linux::KeyF13 as u32), None);
        assert_eq!(hid_key(Linux::KeyHanguel as u32), None);
        assert_eq!(hid_key(Linux::KeyHanja as u32), None);
        assert_eq!(hid_key(u32::MAX), None);
    }
}
