// Windows service backport based on jonstelly/lan-mouse PR #373,
// commit 9ddc443021b3bc5dedd085129546e83f3a6bc648 (GPL-3.0-or-later).
use std::ffi::OsString;
use std::ptr;
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TOKEN_DUPLICATE, TOKEN_QUERY,
    TokenPrimary,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, OpenProcess,
    OpenProcessToken, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};
use windows::core::{HSTRING, PWSTR};
use windows_service::{
    define_windows_service,
    service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceStatus, ServiceType},
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

define_windows_service!(ffi_service_main, lan_mouse_service_main);

/// Starts the Windows service dispatcher.
/// This should only be called when the process is started by the Service Control Manager.
pub fn run_dispatch() -> Result<(), windows_service::Error> {
    service_dispatcher::start("lan-mouse", ffi_service_main)
}

fn lan_mouse_service_main(_arguments: Vec<OsString>) {
    log::info!("lan-mouse Windows service starting");

    if let Err(e) = run_win_service() {
        log::error!("Windows service error: {:?}", e);
    }
}

fn run_win_service() -> Result<(), windows_service::Error> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                log::info!("Received stop/shutdown signal");
                tx.send(()).unwrap_or_default();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register("lan-mouse", event_handler)?;

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::StartPending,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(5),
        process_id: None,
    })?;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create tokio runtime: {:?}", e);
            return Err(windows_service::Error::Winapi(std::io::Error::other(
                "Failed to create tokio runtime",
            )));
        }
    };

    let local = tokio::task::LocalSet::new();
    rt.block_on(local.run_until(async {
        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: windows_service::service::ServiceState::Running,
                controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            })
            .unwrap();

        log::info!("Windows service running - monitoring sessions and managing daemons");

        if let Err(e) = session_manager_loop(rx).await {
            log::error!("Windows service main loop error: {:?}", e);
        }

        status_handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: windows_service::service::ServiceState::StopPending,
                controls_accepted: ServiceControlAccept::empty(),
                exit_code: ServiceExitCode::Win32(0),
                checkpoint: 0,
                wait_hint: Duration::from_secs(5),
                process_id: None,
            })
            .unwrap();
    }));

    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: windows_service::service::ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;

    log::info!("Windows service stopped");
    Ok(())
}

async fn session_manager_loop(
    mut shutdown_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) -> Result<(), std::io::Error> {
    log::info!("Windows service main loop started - monitoring console sessions");

    let mut current_session_id: Option<u32> = None;
    let mut session_daemon: Option<SessionDaemonHandle> = None;
    let mut crash_count = 0u32;
    let mut last_crash_time = std::time::Instant::now();

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::info!("Windows service received shutdown signal");

                // Terminate session daemon if running
                if let Some(daemon) = session_daemon.take() {
                    log::info!("Terminating session daemon (PID={})", daemon.process_id);
                    daemon.terminate();
                }

                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                // Check active console session
                let active_session = get_active_console_session();

                // 0xFFFFFFFF means no active session (e.g., no user logged in)
                if active_session == 0xFFFFFFFF {
                    if current_session_id.is_some() {
                        log::info!("No active console session (user logged out or switching sessions)");

                        // Terminate session daemon
                        if let Some(daemon) = session_daemon.take() {
                            log::info!("Terminating session daemon (PID={})", daemon.process_id);
                            daemon.terminate();
                        }

                        current_session_id = None;
                    }
                    continue;
                }

                // Check if daemon crashed (if we think we have one but it's not running)
                if let Some(ref daemon) = session_daemon {
                    if !daemon.is_running() {
                        let now = std::time::Instant::now();
                        let time_since_last_crash = now.duration_since(last_crash_time);

                        // Reset crash count if it's been more than 60 seconds since last crash
                        if time_since_last_crash.as_secs() > 60 {
                            crash_count = 0;
                        }

                        crash_count = crash_count.saturating_add(1);
                        last_crash_time = now;

                        log::error!("Session daemon crashed (PID={}, crash #{}) - will respawn after backoff",
                                   daemon.process_id, crash_count);

                        // Exponential backoff: 1s, 2s, 4s, 8s, max 30s
                        let backoff_secs = restart_delay_secs(crash_count);
                        log::info!("Waiting {}s before respawn attempt...", backoff_secs);

                        // Remain responsive to shutdown during backoff wait
                        let shutdown = tokio::select! {
                            _ = shutdown_rx.recv() => true,
                            _ = tokio::time::sleep(Duration::from_secs(backoff_secs)) => false,
                        };
                        if shutdown {
                            log::info!("Shutdown received during crash backoff");
                            if let Some(d) = session_daemon.take() {
                                d.terminate();
                            }
                            break;
                        }

                        session_daemon = None;
                        // Will respawn below
                    }
                }

                // Check if session changed
                let session_changed = current_session_id != Some(active_session);
                let need_spawn = session_changed || (session_daemon.is_none() && current_session_id.is_some());

                if session_changed {
                    log::info!("Console session changed: {:?} -> {}", current_session_id, active_session);

                    // Terminate old session daemon
                    if let Some(daemon) = session_daemon.take() {
                        log::info!("Terminating session daemon in old session (PID={})", daemon.process_id);
                        daemon.terminate();
                    }

                    current_session_id = Some(active_session);
                }

                // Spawn daemon in active session if needed
                if need_spawn && session_daemon.is_none() {
                    match get_session_token(active_session) {
                        Ok(token) => {
                            match spawn_session_daemon(active_session, token) {
                                Ok(daemon) => {
                                    log::info!("Successfully spawned session daemon in session {} (PID={})",
                                               active_session, daemon.process_id);
                                    session_daemon = Some(daemon);
                                }
                                Err(e) => {
                                    log::error!("Failed to spawn session daemon: {}", e);
                                }
                            }

                            // Clean up token
                            unsafe { let _ = CloseHandle(token); }
                        }
                        Err(e) => {
                            log::warn!("Failed to get session token for session {}: {} (will retry)", active_session, e);
                        }
                    }
                }
            }
        }
    }

    log::info!("Windows service main loop shutting down");
    Ok(())
}

fn get_active_console_session() -> u32 {
    unsafe { WTSGetActiveConsoleSessionId() }
}

/// Holds process information for a spawned session daemon
struct SessionDaemonHandle {
    process_handle: HANDLE,
    thread_handle: HANDLE,
    process_id: u32,
}

impl SessionDaemonHandle {
    fn is_running(&self) -> bool {
        unsafe {
            // WAIT_TIMEOUT = 0x00000102, means process still running
            WaitForSingleObject(self.process_handle, 0) == WAIT_TIMEOUT
        }
    }

    fn terminate(&self) {
        unsafe {
            let _ = TerminateProcess(self.process_handle, 1);
            // Wait up to 5 seconds for process to exit
            let _ = WaitForSingleObject(self.process_handle, 5000);
        }
    }
}

impl Drop for SessionDaemonHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.process_handle);
            let _ = CloseHandle(self.thread_handle);
        }
    }
}

/// Find a process by name in a specific session
fn find_process_in_session(process_name: &str, session_id: u32) -> Result<u32, std::io::Error> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(|e| {
            std::io::Error::other(format!("CreateToolhelp32Snapshot failed: {}", e))
        })?;

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_err() {
            let _ = CloseHandle(snapshot);
            return Err(std::io::Error::other("Process32FirstW failed"));
        }

        let target_name_lower = process_name.to_lowercase();

        loop {
            // Convert process name from wide string
            let name_len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let process_name_str = String::from_utf16_lossy(&entry.szExeFile[..name_len]);

            if process_name_str.to_lowercase() == target_name_lower {
                // Check if process is in the target session
                let mut proc_session_id = 0u32;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut proc_session_id).is_ok()
                    && proc_session_id == session_id
                {
                    let pid = entry.th32ProcessID;
                    let _ = CloseHandle(snapshot);
                    return Ok(pid);
                }
            }

            if Process32NextW(snapshot, &mut entry).is_err() {
                break;
            }
        }

        let _ = CloseHandle(snapshot);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Process '{}' not found in session {}",
                process_name, session_id
            ),
        ))
    }
}

/// Get a session token for spawning the daemon in the target session.
///
/// Uses the winlogon.exe token because it runs with SYSTEM privileges and has
/// access to the Secure Desktop (UAC prompts). A winlogon-derived token lets
/// the spawned daemon call OpenInputDesktop successfully.
///
/// WTSQueryUserToken only yields a limited user token which cannot access the
/// Secure Desktop even with TokenUIAccess set.
fn get_session_token(session_id: u32) -> Result<HANDLE, std::io::Error> {
    unsafe {
        // Find winlogon.exe in the target session
        let winlogon_pid = find_process_in_session("winlogon.exe", session_id)?;

        // Open the winlogon process
        let process_handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, winlogon_pid)
            .map_err(|e| {
                std::io::Error::other(format!(
                    "OpenProcess failed for winlogon PID {}: {}",
                    winlogon_pid, e
                ))
            })?;

        // Get the winlogon process token
        let mut source_token = HANDLE::default();
        let result = OpenProcessToken(
            process_handle,
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut source_token,
        );
        let _ = CloseHandle(process_handle);

        result.map_err(|e| {
            std::io::Error::other(format!("OpenProcessToken failed for winlogon: {}", e))
        })?;

        // Duplicate the token as a primary token
        let mut new_token = HANDLE::default();
        DuplicateTokenEx(
            source_token,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut new_token,
        )
        .map_err(|e| {
            let _ = CloseHandle(source_token);
            std::io::Error::other(format!("DuplicateTokenEx failed: {}", e))
        })?;

        let _ = CloseHandle(source_token);

        log::info!(
            "Acquired winlogon token for session {} (PID={})",
            session_id,
            winlogon_pid
        );
        Ok(new_token)
    }
}

/// Spawn session daemon in the specified session with the given token
fn spawn_session_daemon(
    session_id: u32,
    token: HANDLE,
) -> Result<SessionDaemonHandle, std::io::Error> {
    unsafe {
        let exe_path = std::env::current_exe()?;

        // Build command line with explicit config path pointing to ProgramData
        // This ensures the session daemon uses the machine-wide config regardless of
        // which user token (or winlogon token) is used to spawn it
        let config_path = r"C:\ProgramData\lan-mouse\config.toml";
        let command = format!(
            r#""{}" --config "{}" --cert-path "C:\ProgramData\lan-mouse\lan-mouse.pem" service-daemon"#,
            exe_path.display(),
            config_path
        );

        log::info!(
            "Spawning session daemon in session {} with command: {}",
            session_id,
            command
        );

        let mut command_wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
        let mut desktop: Vec<u16> = "winsta0\\Default"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let startup_info = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };

        // Create environment block for the user session
        let mut env_block = ptr::null_mut();
        CreateEnvironmentBlock(&mut env_block, Some(token), false)
            .map_err(|e| std::io::Error::other(format!("CreateEnvironmentBlock failed: {}", e)))?;

        let mut proc_info = PROCESS_INFORMATION::default();

        let result = CreateProcessAsUserW(
            Some(token),
            &HSTRING::from(exe_path.as_os_str()),
            Some(PWSTR(command_wide.as_mut_ptr())),
            None,  // Process security
            None,  // Thread security
            false, // Do not leak service handles into the session process
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(env_block as *const _),
            &HSTRING::from(exe_path.parent().expect("executable directory").as_os_str()),
            &startup_info,
            &mut proc_info,
        );

        DestroyEnvironmentBlock(env_block)
            .map_err(|e| log::warn!("DestroyEnvironmentBlock failed: {}", e))
            .ok();

        result.map_err(|e| std::io::Error::other(format!("CreateProcessAsUserW failed: {}", e)))?;

        log::info!(
            "Session daemon spawned successfully: PID={}",
            proc_info.dwProcessId
        );

        Ok(SessionDaemonHandle {
            process_handle: proc_info.hProcess,
            thread_handle: proc_info.hThread,
            process_id: proc_info.dwProcessId,
        })
    }
}

fn restart_delay_secs(crash_count: u32) -> u64 {
    (1u64 << crash_count.saturating_sub(1).min(5)).min(30)
}

#[cfg(test)]
mod tests {
    use super::restart_delay_secs;

    #[test]
    fn repeated_crashes_cannot_overflow_restart_delay() {
        assert_eq!(
            (1..=7).map(restart_delay_secs).collect::<Vec<_>>(),
            [1, 2, 4, 8, 16, 30, 30]
        );
        for count in [0, 64, 65, u32::MAX] {
            assert!((1..=30).contains(&restart_delay_secs(count)));
        }
    }
}
