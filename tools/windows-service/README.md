# Windows secure desktop build

Experimental backport of [Lan Mouse PR #373](https://github.com/feschber/lan-mouse/pull/373), pinned to `9ddc443021b3bc5dedd085129546e83f3a6bc648`, on v0.11.0. Only the service/session orchestrator and input-desktop switching are retained. Existing macOS scrolling and the separate clipboard helper remain independent.

The LocalSystem service launches one SYSTEM daemon in the active console session. Input injection temporarily attaches its windowless thread to the input desktop. The service daemon disables frontend TCP IPC (5252): unauthenticated frontend configuration must not control a SYSTEM process. Configuration and the existing DTLS identity are copied to an administrators/SYSTEM-only directory. Only the configured Mac peer is allowed through the new UDP firewall rule.

`Deploy.ps1 -Action Install -SourcePath <candidate.exe> -ExpectedSha256 <digest>` installs to `C:\Program Files\Lan Mouse UAC`. It preserves the existing `LanMouse-Interactive` task as rollback and restores it automatically if startup checks fail. `-Action Rollback` stops the service, changes it to manual startup and starts the original task. The clipboard task is unchanged. No UAC policy is modified.

Installation requires an elevated shell. On a machine with a Restricted PowerShell execution policy, send the reviewed script text through `-EncodedCommand`; do not change the execution policy. Keep a working SSH connection during installation and UAC verification. Ctrl+Alt+Del generation is not implemented. Login/unlock and reboot require separate interactive verification; the service architecture alone does not establish that they work.

Builds use the existing lockfile plus `windows-service = 0.7.0` from Mullvad. The local cross-build uses the installed Microsoft SDK/MSVC libraries and a sandboxed Rust/Clang toolchain on macOS with network and home credential access denied. No prebuilt PR executable is used.

For an already installed, stopped candidate service, use `Deploy.ps1 -Action Update` with the new binary and digest. The service path and account are checked before replacement. Startup failure resumes the original interactive task.

`desktop-probe` is a Windows-only diagnostic example run through `Probe.ps1` while the candidate service is stopped. It temporarily starts a SYSTEM child in the console session and compares desktop access masks using zero relative mouse motion (no clicks or keystrokes); the previous binary is restored in `finally`. The original input-sharing task can remain running. On this host, `CREATEWINDOW | WRITEOBJECTS` and adding `READOBJECTS` returned SendInput error 5; the upstream GENERIC_WRITE mask accepted the event. The input backend therefore requests GENERIC_WRITE without enabling cross-account hooks or handle inheritance.
