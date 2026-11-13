#!/usr/bin/env python3
"""Run input and clipboard LaunchAgents while their peer is reachable."""

import argparse
import datetime
import os
import plistlib
import shutil
import signal
import socket
import subprocess
import threading
from pathlib import Path


LABEL = "local.lan-mouse-peer"
SHARING_LABELS = ("local.lan-mouse", "local.lan-mouse-clipboard")


def log(message):
    print(f"{datetime.datetime.now().astimezone().isoformat()} {message}", flush=True)


def launchctl(*arguments, check=True):
    return subprocess.run(
        ["/bin/launchctl", *arguments],
        check=check,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=10,
    )


def service_loaded(domain, label):
    return launchctl("print", f"{domain}/{label}", check=False).returncode == 0


def set_services(domain, agents, running):
    for label, path in agents:
        loaded = service_loaded(domain, label)
        if running and not loaded:
            launchctl("bootstrap", domain, str(path))
        elif not running and loaded:
            launchctl("bootout", f"{domain}/{label}")


def reachable(host, port, timeout):
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


class PeerPresence:
    def __init__(self, host, port, timeout, domain, paths):
        self.host = host
        self.port = port
        self.timeout = timeout
        self.domain = domain
        self.agents = []
        for path in paths:
            with path.open("rb") as stream:
                self.agents.append((plistlib.load(stream)["Label"], path))
        self.running = None

    def check(self):
        online = reachable(self.host, self.port, self.timeout)
        if online != self.running:
            set_services(self.domain, self.agents, online)
            self.running = online
            log("Peer reachable; sharing started" if online else
                "Peer unreachable; sharing stopped")


def watch(args):
    domain = f"gui/{os.getuid()}"
    monitor = PeerPresence(args.host, args.port, args.timeout, domain, args.agents)
    stopped = threading.Event()
    for number in (signal.SIGTERM, signal.SIGINT):
        signal.signal(number, lambda *_: stopped.set())
    try:
        while not stopped.is_set():
            try:
                monitor.check()
            except (OSError, subprocess.SubprocessError) as error:
                log(f"ERROR: could not update sharing: {error}")
            stopped.wait(args.interval)
    finally:
        set_services(domain, monitor.agents, False)


def install(args):
    library = Path.home() / "Library"
    login = library / "LaunchAgents"
    runtime = library / "Application Support" / "LanMouseRestore"
    managed = runtime / "managed-launchagents"
    guardian = login / f"{LABEL}.plist"
    domain = f"gui/{os.getuid()}"
    moves = []
    for label in SHARING_LABELS:
        source = login / f"{label}.plist"
        destination = managed / source.name
        if source.exists() and destination.exists():
            raise RuntimeError(f"Both launch definitions exist: {source}, {destination}")
        current = source if source.exists() else destination
        with current.open("rb") as stream:
            definition = plistlib.load(stream)
        if definition["Label"] != label:
            raise RuntimeError(f"Unexpected launch definition: {current}")
        moves.append((source, destination))

    if service_loaded(domain, LABEL):
        launchctl("bootout", f"{domain}/{LABEL}")
    set_services(domain, [(label, source) for label, (source, _) in
                          zip(SHARING_LABELS, moves)], False)
    managed.mkdir(parents=True, exist_ok=True)
    for source, destination in moves:
        if source.exists():
            source.rename(destination)
    script = runtime / "peer_presence.py"
    shutil.copy2(Path(__file__).resolve(), script)
    log_path = str(library / "Logs" / f"{LABEL}.log")
    definition = {
        "Label": LABEL,
        "ProgramArguments": [
            "/usr/bin/python3", str(script), "watch", "--host", args.host,
            *[str(destination) for _, destination in moves],
        ],
        "RunAtLoad": True,
        "KeepAlive": True,
        "ProcessType": "Background",
        "ThrottleInterval": 10,
        "StandardOutPath": log_path,
        "StandardErrorPath": log_path,
    }
    with guardian.open("wb") as stream:
        plistlib.dump(definition, stream)
    launchctl("enable", f"{domain}/{LABEL}")
    launchctl("bootstrap", domain, str(guardian))
    log(f"Installed {LABEL}; existing sharing launch definitions are in {managed}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    setup = commands.add_parser("install", help="gate the existing sharing agents")
    setup.add_argument("--host", required=True)
    setup.set_defaults(run=install)
    monitor = commands.add_parser("watch", help="monitor a peer and control launch agents")
    monitor.add_argument("--host", required=True)
    monitor.add_argument("--port", type=int, default=22)
    monitor.add_argument("--timeout", type=float, default=2)
    monitor.add_argument("--interval", type=float, default=5)
    monitor.add_argument("agents", nargs="+", type=Path)
    monitor.set_defaults(run=watch)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
