import plistlib
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import peer_presence as peer


class PresenceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.paths = []
        for label in peer.SHARING_LABELS:
            path = self.root / f"{label}.plist"
            with path.open("wb") as stream:
                plistlib.dump({"Label": label}, stream)
            self.paths.append(path)
        self.monitor = peer.PeerPresence("127.0.0.1", 22, 2, "gui/501", self.paths)

    @patch.object(peer, "log")
    @patch.object(peer, "set_services")
    @patch.object(peer, "reachable")
    def test_offline_start_and_power_transitions(self, reachable, services, _log):
        reachable.side_effect = [False, False, True, True, False, False, True]
        for _ in range(7):
            self.monitor.check()
        self.assertEqual([call.args[2] for call in services.call_args_list],
                         [False, True, False, True])
        self.assertEqual(services.call_args.args[:2],
                         ("gui/501", list(zip(peer.SHARING_LABELS, self.paths))))

    @patch.object(peer, "log")
    @patch.object(peer, "set_services")
    @patch.object(peer, "reachable", return_value=True)
    def test_failed_start_is_retried(self, _reachable, services, _log):
        services.side_effect = [subprocess.CalledProcessError(1, "launchctl"), None]
        with self.assertRaises(subprocess.CalledProcessError):
            self.monitor.check()
        self.assertIsNone(self.monitor.running)
        self.monitor.check()
        self.assertTrue(self.monitor.running)
        self.assertEqual(services.call_count, 2)

    def test_reachability_uses_tcp_and_closed_port_is_offline(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            host, port = listener.getsockname()
            self.assertTrue(peer.reachable(host, port, 0.1))
            connection, _ = listener.accept()
            connection.close()
        self.assertFalse(peer.reachable(host, port, 0.1))

    @patch.object(peer.socket, "create_connection", side_effect=socket.timeout)
    def test_timeout_is_offline(self, connect):
        self.assertFalse(peer.reachable("192.0.2.1", 22, 2))
        connect.assert_called_once_with(("192.0.2.1", 22), timeout=2)

    @patch.object(peer, "launchctl")
    @patch.object(peer, "service_loaded")
    def test_only_missing_services_start(self, loaded, command):
        loaded.side_effect = [True, False]
        peer.set_services("gui/501", self.monitor.agents, True)
        command.assert_called_once_with("bootstrap", "gui/501", str(self.paths[1]))

    @patch.object(peer, "launchctl")
    @patch.object(peer, "service_loaded")
    def test_only_loaded_services_stop(self, loaded, command):
        loaded.side_effect = [True, False]
        peer.set_services("gui/501", self.monitor.agents, False)
        command.assert_called_once_with("bootout", "gui/501/local.lan-mouse")

    @patch.object(peer, "launchctl")
    def test_service_query_uses_exit_status(self, command):
        for status, expected in [(0, True), (113, False)]:
            command.return_value.returncode = status
            self.assertEqual(peer.service_loaded("gui/501", "example"), expected)
        command.assert_called_with("print", "gui/501/example", check=False)

    @patch.object(peer, "log")
    @patch.object(peer, "launchctl")
    @patch.object(peer, "service_loaded", return_value=False)
    def test_install_preserves_launch_definitions_and_can_repeat(
        self, _loaded, _command, _log
    ):
        login = self.root / "Library" / "LaunchAgents"
        login.mkdir(parents=True)
        originals = {}
        for label in peer.SHARING_LABELS:
            path = login / f"{label}.plist"
            with path.open("wb") as stream:
                plistlib.dump({"Label": label, "ProgramArguments": ["/bin/sleep", "60"],
                              "KeepAlive": True, "RunAtLoad": True}, stream)
            originals[label] = path.read_bytes()
        with patch.object(peer.Path, "home", return_value=self.root):
            for _ in range(2):
                peer.install(SimpleNamespace(host="192.0.2.1"))
        runtime = self.root / "Library" / "Application Support" / "LanMouseRestore"
        for label in peer.SHARING_LABELS:
            self.assertFalse((login / f"{label}.plist").exists())
            self.assertEqual((runtime / "managed-launchagents" / f"{label}.plist")
                             .read_bytes(), originals[label])
        with (login / f"{peer.LABEL}.plist").open("rb") as stream:
            guardian = plistlib.load(stream)
        self.assertEqual(guardian["Label"], peer.LABEL)
        self.assertEqual(guardian["ProgramArguments"][2:5],
                         ["watch", "--host", "192.0.2.1"])


if __name__ == "__main__":
    unittest.main()
