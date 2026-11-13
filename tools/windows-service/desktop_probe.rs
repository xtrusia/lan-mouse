#[cfg(windows)]
fn main() {
    use std::{fs::OpenOptions, io::Write, mem::size_of, time::Duration};
    use windows::Win32::{
        Foundation::{GetLastError, HANDLE, SetLastError, WIN32_ERROR},
        System::{
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetThreadDesktop,
                GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
            },
            Threading::GetCurrentThreadId,
        },
        UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
        },
    };
    if std::env::args().nth(1).as_deref() == Some("win-svc") {
        lan_mouse::windows_service::run_dispatch().unwrap();
        return;
    }
    let mut out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(r"C:\ProgramData\lan-mouse\desktop-probe.log")
        .unwrap();
    unsafe {
        let old = GetThreadDesktop(GetCurrentThreadId()).unwrap();
        for access in [0x82, 0x83, 0x40000000, 0x4000000a, 0x4000000b, 0x10000000] {
            match OpenInputDesktop(
                DESKTOP_CONTROL_FLAGS(0),
                false,
                DESKTOP_ACCESS_FLAGS(access),
            ) {
                Err(e) => {
                    writeln!(out, "access={access:#x}: open={e}").unwrap();
                }
                Ok(desktop) => {
                    let mut name = [0u16; 128];
                    let _ = GetUserObjectInformationW(
                        HANDLE(desktop.0),
                        UOI_NAME,
                        Some(name.as_mut_ptr().cast()),
                        size_of::<[u16; 128]>() as u32,
                        None,
                    );
                    let name = String::from_utf16_lossy(
                        &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
                    );
                    match SetThreadDesktop(desktop) {
                        Err(e) => {
                            writeln!(out, "access={access:#x}, desktop={name}: attach={e}")
                                .unwrap();
                        }
                        Ok(()) => {
                            // Zero relative motion tests acceptance without moving or clicking.
                            let input = INPUT {
                                r#type: INPUT_MOUSE,
                                Anonymous: INPUT_0 {
                                    mi: MOUSEINPUT {
                                        dwFlags: MOUSEEVENTF_MOVE,
                                        ..Default::default()
                                    },
                                },
                            };
                            SetLastError(WIN32_ERROR(0));
                            let count = SendInput(&[input], size_of::<INPUT>() as i32);
                            writeln!(
                                out,
                                "access={access:#x}, desktop={name}: sent={count}, error={:?}",
                                GetLastError()
                            )
                            .unwrap();
                            SetThreadDesktop(old).unwrap();
                        }
                    }
                    let _ = CloseDesktop(desktop);
                }
            }
        }
    }
    out.flush().unwrap();
    std::thread::sleep(Duration::from_secs(60));
}
#[cfg(not(windows))]
fn main() {}
