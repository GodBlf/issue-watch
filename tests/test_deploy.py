"""Exercise the deployment CLI, with external commands replaced at the OS boundary.

Run on Linux: python3 -m unittest discover -s tests -p test_deploy.py -v
"""
import os
import json
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "deploy" / "deploy.py"
COMMIT = "a" * 40
IMAGE = "ghcr.io/godblf/issue-watch@sha256:" + "b" * 64
OLD_IMAGE = "ghcr.io/godblf/issue-watch@sha256:" + "c" * 64

# These executables model external services, not private deployment functions.
COMMAND = r'''#!/usr/bin/env python3
import json, os, pathlib, sqlite3, sys
path = pathlib.Path(os.environ["TEST_STATE"])
state = json.loads(path.read_text())
tool, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
code, output = 0, ""
if tool == "git":
    output = state.get("master", "a" * 40) + "\trefs/heads/master"
elif tool == "systemctl":
    action = args[0]
    if action == "is-active":
        code = 0 if state["legacy"] else 3
    elif action == "is-enabled":
        code = 0 if state["enabled"] else 1
    elif action == "stop": state["legacy"] = False
    elif action == "start": state["legacy"] = True
    elif action == "disable": state["enabled"] = False
    elif action == "enable": state["enabled"] = True
elif tool == "curl":
    code = 22 if state.get("health_fail") else 0
    output = json.dumps({"status": "unknown", "uptime_seconds": 0, "started_at": "now"})
elif tool == "docker":
    if args[0] == "pull": code = 1 if state.get("pull_fail") else 0
    elif args[:2] == ["image", "inspect"]:
        output = json.dumps([{"Config": {"Labels": {"org.opencontainers.image.revision": "a" * 40}}}])
    elif args[0] == "ps": output = "instance" if state["container"] else ""
    elif args[0] == "inspect":
        output = json.dumps([{"Config": {"Image": state["image"]}, "State": {"Running": state["container"]}}])
    elif args[0] == "stop": state["container"] = False
    elif args[0] == "start": state["container"] = True
    elif args[0] == "update": state["restart"] = "no"
    elif args[0] == "logs": output = "test container log"
    elif args[0] == "compose":
        if "up" in args:
            state["container"] = True
            state["image"] = os.environ["ISSUE_WATCH_IMAGE"]
            state["restart"] = "unless-stopped"
            if state.get("mutate"):
                db = sqlite3.connect(path.parent / "data/issue-watch.sqlite3")
                db.execute("INSERT INTO records VALUES ('after-start')")
                db.commit()
                db.close()
            if state.get("start_fail"): code = 1
        elif "config" in args: code = 1 if state.get("config_fail") else 0
else:
    code = 99
path.write_text(json.dumps(state))
if output: print(output)
sys.exit(code)
'''


class DeploymentTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for folder in ("config", "data", "deploy", "bin"):
            (self.root / folder).mkdir()
        (self.root / "config/config.toml").write_text('database_path = "data/issue-watch.sqlite3"\n')
        (self.root / ".env.local").write_text("QQ_APP_ID=test\nQQ_APP_SECRET=not-a-real-secret\n")
        (self.root / "deploy/compose.yml").write_text("services: {}\n")
        with sqlite3.connect(self.root / "data/issue-watch.sqlite3") as db:
            db.execute("CREATE TABLE records (value TEXT)")
            db.execute("INSERT INTO records VALUES ('before-update')")
        self.state_path = self.root / "state.json"
        self.write_state(legacy=True, enabled=True, container=False, image=OLD_IMAGE)
        for name in ("docker", "systemctl", "git", "curl"):
            executable = self.root / "bin" / name
            executable.write_text(COMMAND)
            executable.chmod(0o755)

    def write_state(self, **changes):
        current = json.loads(self.state_path.read_text()) if self.state_path.exists() else {}
        current.update(changes)
        self.state_path.write_text(json.dumps(current))

    def state(self):
        return json.loads(self.state_path.read_text())

    def deploy(self, request=None):
        env = dict(os.environ, PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"], TEST_STATE=str(self.state_path))
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--root", str(self.root), "--health-timeout", "0", request or f"deploy {IMAGE} {COMMIT}"],
            env=env, capture_output=True, text=True,
        )

    def test_arbitrary_ssh_command_is_rejected_without_touching_service(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                ["python3", str(SCRIPT), "--root", directory, "deploy image; touch /tmp/unsafe"],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("invalid deployment request", result.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_image_pull_failure_keeps_existing_service_and_database(self):
        self.write_state(pull_fail=True)
        result = self.deploy()
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertTrue(self.state()["legacy"])
        self.assertFalse(self.state()["container"])
        self.assertFalse((self.root / "backups").exists())

    def test_successful_migration_preserves_data_and_disables_old_service(self):
        result = self.deploy()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        state = self.state()
        self.assertFalse(state["legacy"])
        self.assertFalse(state["enabled"])
        self.assertTrue(state["container"])
        self.assertEqual(state["image"], IMAGE)
        backups = list((self.root / "backups").glob("*/issue-watch.sqlite3"))
        self.assertEqual(len(backups), 1)
        for database in (backups[0], self.root / "data/issue-watch.sqlite3"):
            with sqlite3.connect(database) as db:
                self.assertEqual(db.execute("SELECT value FROM records").fetchall(), [("before-update",)])
        self.assertEqual(json.loads((self.root / "deploy/current.json").read_text())["image"], IMAGE)

    def test_backup_failure_restores_legacy_without_starting_new_version(self):
        (self.root / "data/issue-watch.sqlite3").write_bytes(b"not a database")
        result = self.deploy()
        self.assertEqual(result.returncode, 1)
        self.assertTrue(self.state()["legacy"])
        self.assertTrue(self.state()["enabled"])
        self.assertFalse(self.state()["container"])

    def test_failed_start_keeps_migrated_data_and_requires_manual_recovery(self):
        self.write_state(start_fail=True, mutate=True)
        result = self.deploy()
        self.assertEqual(result.returncode, 1)
        state = self.state()
        self.assertFalse(state["legacy"])
        self.assertFalse(state["enabled"])
        self.assertFalse(state["container"])
        self.assertEqual(state["restart"], "no")
        with sqlite3.connect(self.root / "data/issue-watch.sqlite3") as db:
            self.assertEqual(db.execute("SELECT value FROM records").fetchall(), [("before-update",), ("after-start",)])
        self.assertTrue((self.root / "deploy/recovery-required.json").exists())
        self.write_state(start_fail=False)
        retry = self.deploy()
        self.assertEqual(retry.returncode, 1)
        self.assertIn("manual recovery required", retry.stderr)
        self.assertFalse(self.state()["container"])

    def test_health_failure_stops_new_container_without_rolling_back(self):
        self.write_state(health_fail=True)
        result = self.deploy()
        self.assertEqual(result.returncode, 1)
        self.assertFalse(self.state()["container"])
        self.assertFalse(self.state()["legacy"])
        self.assertEqual(self.state()["restart"], "no")

    def test_superseded_master_commit_does_not_replace_current_service(self):
        self.write_state(master="d" * 40)
        result = self.deploy()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("skipped", result.stdout)
        self.assertTrue(self.state()["legacy"])
        self.assertFalse((self.root / "backups").exists())

    def test_update_of_container_preserves_database_and_takes_backup(self):
        self.write_state(legacy=False, enabled=False, container=True)
        result = self.deploy()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(self.state()["container"])
        self.assertEqual(self.state()["image"], IMAGE)
        self.assertEqual(len(list((self.root / "backups").glob("*/issue-watch.sqlite3"))), 1)

    def test_two_running_instances_are_rejected_before_modification(self):
        self.write_state(container=True)
        result = self.deploy()
        self.assertEqual(result.returncode, 1)
        self.assertTrue(self.state()["legacy"])
        self.assertTrue(self.state()["container"])

    def test_missing_config_keeps_service_running(self):
        (self.root / "config/config.toml").unlink()
        result = self.deploy()
        self.assertEqual(result.returncode, 1)
        self.assertTrue(self.state()["legacy"])

    def test_invalid_digest_or_repository_is_rejected_before_any_changes(self):
        for request in (f"deploy ghcr.io/other/image@sha256:{'b' * 64} {COMMIT}", f"deploy {IMAGE} master", f"deploy {IMAGE} {COMMIT} --root /tmp"):
            result = self.deploy(request)
            self.assertEqual(result.returncode, 1)
            self.assertTrue(self.state()["legacy"])
            self.assertFalse((self.root / "deploy/.lock").exists())


if __name__ == "__main__":
    unittest.main()
