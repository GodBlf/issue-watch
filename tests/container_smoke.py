"""Verify the built image through production Compose, without external network access."""
import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image")
    args = parser.parse_args()
    compose = Path(__file__).resolve().parents[1] / "deploy/compose.yml"
    with tempfile.TemporaryDirectory(prefix="issue-watch-smoke-") as directory:
        root = Path(directory)
        (root / "config").mkdir()
        (root / "data").mkdir()
        config = root / "config/config.toml"
        config.write_text('poll_interval_seconds = 60\ndatabase_path = "data/issue-watch.sqlite3"\nrepositories = ["owner/repository"]\n')
        (root / ".env.local").write_text("QQ_APP_ID=test\nQQ_APP_SECRET=smoke-secret\nQQ_ACCESS_TOKEN=smoke-token\nQQ_GATEWAY_URL=ws://127.0.0.1:9\n")
        override = root / "network.yml"
        override.write_text("networks:\n  default:\n    internal: true\n")
        env = dict(os.environ, ISSUE_WATCH_ROOT=str(root), ISSUE_WATCH_IMAGE=args.image)
        command = ["docker", "compose", "--project-name", "issue-watch-smoke", "--env-file", str(root / ".env.local"), "-f", str(compose), "-f", str(override)]

        def run(*arguments):
            return subprocess.run(command + list(arguments), env=env, capture_output=True, text=True, check=True, timeout=60).stdout

        def snapshot():
            return api("/api/status")[1]

        def api(path, method="GET", body=None, origin="http://127.0.0.1:8080"):
            identifier = run("ps", "-q", "issue-watch").strip()
            payload = json.dumps(body).encode() if body is not None else b""
            request = f"{method} {path} HTTP/1.0\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\nOrigin: {origin}\r\nX-Issue-Watch-Admin: 1\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n" + payload.decode()
            # Docker's internal network also isolates host access on some platforms.
            response = subprocess.run([
                "docker", "exec", identifier, "bash", "-c",
                'exec 3<>/dev/tcp/127.0.0.1/8080; printf "%s" "$1" >&3; cat <&3', "request", request,
            ], capture_output=True, text=True, check=True, timeout=5).stdout
            return int(response.split()[1]), json.loads(response.split("\n\n", 1)[1])

        def wait_for(predicate, read=snapshot):
            for _ in range(120):
                try:
                    status = read()
                    if predicate(status):
                        return status
                except (subprocess.SubprocessError, AssertionError, ValueError, IndexError):
                    pass
                time.sleep(0.5)
            raise AssertionError("container state did not become observable")

        try:
            rendered = json.loads(run("config", "--format", "json"))
            service = rendered["services"]["issue-watch"]
            assert service["ports"][0]["host_ip"] == "127.0.0.1"
            assert str(service["ports"][0]["published"]) == "8081"
            assert {v["target"] for v in service["volumes"]} == {"/app/config", "/app/data"}
            assert not next(v for v in service["volumes"] if v["target"] == "/app/config").get("read_only", False)
            run("up", "-d", "--no-build", "--pull", "never")
            wait_for(lambda status: "qq_binding" in status["components"])
            database = root / "data/issue-watch.sqlite3"
            assert database.exists()
            replacement = config.with_suffix(".tmp")
            replacement.write_text(config.read_text().replace("owner/repository", "owner/another"))
            replacement.replace(config)
            wait_for(lambda status: "owner/another" in json.dumps(status))

            assert api("/api/admin/subscriptions", "POST", {"user_openid": "smoke-subscriber", "qq_number_note": "123456"})[0] == 201
            assert api("/api/admin/subscriptions", "POST", {"user_openid": "smoke-subscriber", "qq_number_note": "unchanged"})[0] == 200
            rows = api("/api/admin/subscriptions")[1]
            assert rows[0]["qq_number_note"] == "123456"
            old_id = rows[0]["id"]
            assert api(f"/api/admin/subscriptions/{old_id}", "PATCH", {"qq_number_note": ""})[0] == 200
            assert api("/api/admin/subscriptions")[1][0]["qq_number_note"] is None
            assert api(f"/api/admin/subscriptions/{old_id}", "DELETE")[0] == 200
            assert api("/api/admin/subscriptions", "POST", {"user_openid": "smoke-subscriber", "qq_number_note": "persisted"})[0] == 201
            assert api(f"/api/admin/subscriptions/{old_id}", "DELETE")[0] == 404
            assert api("/api/admin/subscriptions", "POST", {"user_openid": "untrusted"}, origin="https://example.com")[0] == 403
            settings = api("/api/admin/config")[1]
            assert api("/api/admin/config", "PATCH", {"version": settings["version"], "poll_interval_seconds": 5})[0] == 200
            assert api("/api/admin/config", "PATCH", {"version": settings["version"], "poll_interval_seconds": 10})[0] == 409
            wait_for(lambda settings: settings["stage"] == "applied" and settings["applied"]["poll_interval_seconds"] == 5, lambda: api("/api/admin/config")[1])
            assert "poll_interval_seconds = 5" in config.read_text()

            run("stop")
            # A stopped deployment fixture with existing subscriptions and delivery progress.
            with closing(sqlite3.connect(database)) as db, db:
                db.execute("UPDATE monitored_repositories SET cursor='2020-01-01T00:00:00+00:00' WHERE name='owner/another'")
                subscription = db.execute("SELECT id FROM broadcast_subscriptions").fetchone()[0]
                for number, state in ((1, "sent"), (2, "retry")):
                    db.execute("INSERT INTO issue_notifications(repository,number,title,author,created_at,url,state) VALUES ('owner/another',?,'test','test','2020-01-01T00:00:00+00:00','https://example.com',?)", (number, state))
                    db.execute("INSERT INTO notification_deliveries(subscription_id,repository,number,state,next_attempt_at) VALUES (?,'owner/another',?,?,'2099-01-01T00:00:00+00:00')", (subscription, number, state))
            run("start")
            status = wait_for(lambda status: status["components"]["qq_binding"]["details"]["subscriber_count"] == 1)
            assert api("/api/admin/subscriptions")[1][0]["qq_number_note"] == "persisted"
            restarted = api("/api/admin/config")[1]
            assert restarted["applied"]["poll_interval_seconds"] == 5
            assert api("/api/admin/config", "PATCH", {"version": settings["version"], "poll_interval_seconds": 10})[0] == 409
            assert "smoke-secret" not in json.dumps(status)
            assert "smoke-token" not in json.dumps(status)
            run("stop")
            with closing(sqlite3.connect(database)) as db:
                assert db.execute("SELECT user_openid FROM broadcast_subscriptions").fetchall() == [("smoke-subscriber",)]
                assert db.execute("SELECT cursor FROM monitored_repositories WHERE name='owner/another'").fetchone() == ("2020-01-01T00:00:00+00:00",)
                assert db.execute("SELECT number,state FROM notification_deliveries ORDER BY number").fetchall() == [(1, "sent"), (2, "retry")]
            print("Production Compose smoke passed: loopback port, writable atomic config, management API and origins, reload/application, subscription/note/cursor/delivery persistence, redacted status; external network blocked.")
        finally:
            run("down")


if __name__ == "__main__":
    main()
