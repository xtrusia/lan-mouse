# Local dependency patch

webrtc-util 0.11.0 is copied from the existing crates.io archive, verified against Cargo.lock SHA256 `64bfb10dbe6d762f80169ae07cf252bafa1f764b9594d140008a0231c0cdce58`.

The only code change is in `src/conn/conn_udp_listener.rs`: keep the Windows UDP listener alive after a ConnectionReset error (WSAECONNRESET 10054). A closed peer port must not stop reception for every peer. Other errors retain their original behavior. License files are included unchanged.

Regression: `listen::tests::udp_listener_accepts_after_peer_port_closes` in the root library, executed on Windows. It closes a peer port, sends a reply to provoke ICMP Port Unreachable, and verifies that subsequent peers can connect and deliver data without restarting the listener.

The upstream changelog's extra final blank line was removed to pass repository whitespace checks.
