#!/usr/bin/env python3
"""Fixed deployment entry point. SSH may supply only 'deploy IMAGE COMMIT'."""
import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import sys
import subprocess
import tempfile
import sqlite3
import signal
import time
import tomllib
import uuid


REQUEST = re.compile(r"deploy (ghcr\.io/godblf/issue-watch@sha256:[0-9a-f]{64}) ([0-9a-f]{40})")


def command(*args, env=None, timeout=60, check=True):
    result = subprocess.run(args, env=env, timeout=timeout, capture_output=True, text=True)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} {args[1]} failed")
    return result


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


class Deployment:
    def __init__(self, root, image, revision, health_timeout):
        self.root = root.resolve()
        self.image = image
        self.revision = revision
        self.health_timeout = health_timeout
        self.env = dict(os.environ, ISSUE_WATCH_IMAGE=image, ISSUE_WATCH_ROOT=str(self.root))
        self.stopped = False
        self.started = False
        self.legacy = False
        self.legacy_enabled = False
        self.old_container = None
        self.backup = None

    def compose(self, *args):
        return command(
            "docker", "compose", "--project-name", "issue-watch", "--project-directory", str(self.root),
            "--env-file", str(self.root / ".env.local"), "-f", str(self.root / "deploy/compose.yml"),
            *args, env=self.env, timeout=180,
        )

    def container(self):
        identifiers = command(
            "docker", "ps", "-aq", "--filter", "label=com.docker.compose.project=issue-watch",
            "--filter", "label=com.docker.compose.service=issue-watch",
        ).stdout.split()
        if len(identifiers) > 1:
            raise RuntimeError("multiple issue-watch containers found")
        if not identifiers:
            return None
        info = json.loads(command("docker", "inspect", identifiers[0]).stdout)[0]
        return {"id": identifiers[0], "image": info["Config"]["Image"], "running": info["State"]["Running"]}

    def latest_revision(self):
        return command("git", "ls-remote", "https://github.com/GodBlf/issue-watch.git", "refs/heads/master").stdout.split()[0]

    def preflight(self):
        for relative in ("config/config.toml", ".env.local", "data/issue-watch.sqlite3", "deploy/compose.yml"):
            if not (self.root / relative).is_file():
                raise RuntimeError(f"missing runtime file: {relative}")
        if (self.root / "deploy/recovery-required.json").exists():
            raise RuntimeError("manual recovery required before another deployment")
        config = tomllib.loads((self.root / "config/config.toml").read_text())
        if config.get("database_path", "data/issue-watch.sqlite3") not in ("data/issue-watch.sqlite3", "/app/data/issue-watch.sqlite3"):
            raise RuntimeError("database_path must refer to the mounted data directory")
        self.compose("config", "--quiet")
        self.legacy = command("systemctl", "is-active", "--quiet", "issue-watch.service", check=False).returncode == 0
        self.legacy_enabled = command("systemctl", "is-enabled", "--quiet", "issue-watch.service", check=False).returncode == 0
        self.old_container = self.container()
        if self.legacy and self.old_container and self.old_container["running"]:
            raise RuntimeError("legacy service and container are both running; manual intervention required")

    def backup_database(self):
        name = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ") + "-" + uuid.uuid4().hex[:8]
        self.backup = self.root / "backups" / name
        self.backup.mkdir(parents=True, mode=0o700)
        source = self.root / "data/issue-watch.sqlite3"
        with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True, timeout=30) as original:
            with sqlite3.connect(self.backup / "issue-watch.sqlite3") as copy:
                original.backup(copy)
                if copy.execute("PRAGMA integrity_check").fetchone() != ("ok",):
                    raise RuntimeError("database backup integrity check failed")
        write_json(self.backup / "deployment.json", {
            "previous": self.old_container or {"legacy": self.legacy},
            "target_image": self.image, "revision": self.revision,
        })
        current = self.root / "deploy/current.json"
        if current.exists():
            (self.backup / "previous.json").write_bytes(current.read_bytes())

    def wait_for_health(self):
        deadline = time.monotonic() + self.health_timeout
        while True:
            container = self.container()
            if not container or container["image"] != self.image or not container["running"]:
                raise RuntimeError("target container is not running")
            response = command("curl", "--fail", "--silent", "--max-time", "5", "http://127.0.0.1:8081/api/status", check=False)
            try:
                status = json.loads(response.stdout)
                if response.returncode == 0 and status["status"] in ("normal", "unknown", "warning", "error") and isinstance(status["uptime_seconds"], int):
                    return
            except (ValueError, KeyError, TypeError):
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError("target container health check timed out")
            time.sleep(2)

    def run(self):
        self.preflight()
        if self.latest_revision() != self.revision:
            print("skipped: superseded master revision")
            return
        # An empty Docker configuration guarantees anonymous GHCR access.
        with tempfile.TemporaryDirectory() as docker_config:
            command("docker", "pull", self.image, env=dict(os.environ, DOCKER_CONFIG=docker_config), timeout=900)
        labels = json.loads(command("docker", "image", "inspect", self.image).stdout)[0]["Config"].get("Labels") or {}
        if labels.get("org.opencontainers.image.revision") != self.revision:
            raise RuntimeError("image revision does not match deployment request")
        if self.latest_revision() != self.revision:
            print("skipped: superseded master revision")
            return
        self.stopped = True
        if self.legacy:
            command("systemctl", "stop", "issue-watch.service")
        if self.old_container and self.old_container["running"]:
            command("docker", "stop", "--time", "30", self.old_container["id"])
        if command("systemctl", "is-active", "--quiet", "issue-watch.service", check=False).returncode == 0:
            raise RuntimeError("legacy service did not stop")
        container = self.container()
        if container and container["running"]:
            raise RuntimeError("old container did not stop")
        self.backup_database()
        # Disable before launch: after a failed migration, reboot must not start the old binary.
        if self.legacy_enabled:
            command("systemctl", "disable", "issue-watch.service")
        write_json(self.root / "deploy/recovery-required.json", {
            "image": self.image, "revision": self.revision, "backup": str(self.backup), "phase": "starting",
        })
        self.started = True
        self.compose("up", "-d", "--no-build", "--pull", "never", "--force-recreate")
        self.wait_for_health()
        write_json(self.root / "deploy/current.json", {"image": self.image, "revision": self.revision, "backup": str(self.backup)})
        (self.root / "deploy/recovery-required.json").unlink()
        print(f"deployed {self.image}; backup: {self.backup}")

    def recover(self):
        if not self.stopped:
            return
        if not self.started:
            if self.legacy_enabled:
                command("systemctl", "enable", "issue-watch.service")
            if self.legacy:
                command("systemctl", "start", "issue-watch.service")
            if self.old_container and self.old_container["running"]:
                command("docker", "start", self.old_container["id"])
            return
        errors = []

        def attempt(operation):
            try:
                return operation()
            except (RuntimeError, OSError, ValueError, IndexError, subprocess.TimeoutExpired) as error:
                errors.append(type(error).__name__)
                return None

        # Cleanup must not depend on being able to write metadata or logs (e.g. ENOSPC).
        container = attempt(self.container)
        if container:
            attempt(lambda: command("docker", "update", "--restart=no", container["id"]))
            attempt(lambda: command("docker", "stop", "--time", "30", container["id"]))
            logs = attempt(lambda: command("docker", "logs", "--tail", "200", container["id"], check=False))
            if self.backup and logs:
                attempt(lambda: (self.backup / "failed-container.log").write_text(logs.stdout + logs.stderr))
        else:
            attempt(lambda: self.compose("stop", "--timeout", "30"))
        attempt(lambda: write_json(self.root / "deploy/recovery-required.json", {
            "image": self.image, "revision": self.revision, "backup": str(self.backup),
        }))
        print(f"manual recovery required; backup: {self.backup}", file=sys.stderr)
        if errors:
            raise RuntimeError("recovery had errors: " + ", ".join(errors))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("/home/godblf/issue-watch"))
    parser.add_argument("--health-timeout", type=int, default=120)
    parser.add_argument("request")
    args = parser.parse_args()
    request = REQUEST.fullmatch(args.request)
    if not request:
        print("invalid deployment request", file=sys.stderr)
        return 1
    os.umask(0o077)
    def interrupted(signum, _frame):
        raise RuntimeError(f"deployment interrupted by signal {signum}")
    for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupted)
    deployment = Deployment(args.root, request[1], request[2], args.health_timeout)
    try:
        with (args.root / "deploy/.lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                deployment.run()
                return 0
            except (RuntimeError, OSError, ValueError, IndexError, sqlite3.Error, subprocess.TimeoutExpired) as error:
                print(f"deployment failed: {type(error).__name__}: {error}", file=sys.stderr)
                for signum in (signal.SIGHUP, signal.SIGINT, signal.SIGTERM):
                    signal.signal(signum, signal.SIG_IGN)
                deployment.recover()
                return 1
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
        print(f"deployment/recovery failed: {type(error).__name__}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
