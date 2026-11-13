# Start sharing only while Windows is reachable

`peer_presence.py` controls the existing macOS `local.lan-mouse` and
`local.lan-mouse-clipboard` LaunchAgents. It checks the Windows SSH endpoint
every five seconds, with a two-second connection timeout. A failed connection
stops both services; a successful connection starts them again. Network loss
and Windows sleep are treated like shutdown. The monitor does not wake Windows.

Install with the existing Windows SSH address:

```sh
/usr/bin/python3 tools/peer-presence/peer_presence.py install --host WINDOWS_IP
```

Installation moves the original sharing plists from `~/Library/LaunchAgents/`
to `~/Library/Application Support/LanMouseRestore/managed-launchagents/` and
registers `local.lan-mouse-peer` at login. The existing signed sharing binaries,
keys, tokens, and Windows tasks stay in place. Only the small monitor runs while
Windows is offline. Clipboard sharing still requires the Windows login helper.
Its normal startup baseline prevents replaying clipboard changes made offline.

Logs: `~/Library/Logs/local.lan-mouse-peer.log`. A network transition is logged
once; unchanged availability does not restart services. Failed launchctl updates
are retried at the next check.

To restore unconditional startup, disable and boot out `local.lan-mouse-peer`
in `gui/$(id -u)`, move the two original plists back to `~/Library/LaunchAgents/`,
then bootstrap them in the same GUI domain.

Tests:

```sh
/usr/bin/python3 -m unittest discover -s tools/peer-presence -v
```
