"""Verify the built image through production Compose, without external network access."""
import argparse
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
            identifier = run("ps", "-q", "issue-watch").strip()
            # Docker's internal network also isolates host access on some platforms.
            response = subprocess.run([
                "docker", "exec", identifier, "bash", "-c",
                'exec 3<>/dev/tcp/127.0.0.1/8080; printf "GET /api/status HTTP/1.0\\r\\n\\r\\n" >&3; cat <&3',
            ], capture_output=True, text=True, check=True, timeout=5).stdout
            assert response.startswith("HTTP/1.0 200")
            return json.loads(response.split("\n\n", 1)[1])

        def wait_for(predicate):
            for _ in range(40):
                try:
                    status = snapshot()
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
            run("up", "-d", "--no-build", "--pull", "never")
            wait_for(lambda _: True)
            database = root / "data/issue-watch.sqlite3"
            assert database.exists()
            replacement = config.with_suffix(".tmp")
            replacement.write_text(config.read_text().replace("owner/repository", "owner/another"))
            replacement.replace(config)
            wait_for(lambda status: "owner/another" in json.dumps(status))

            run("stop")
            # A stopped deployment fixture with existing subscriptions and delivery progress.
            with sqlite3.connect(database) as db:
                db.execute("UPDATE monitored_repositories SET cursor='2020-01-01T00:00:00+00:00' WHERE name='owner/another'")
                db.execute("INSERT INTO broadcast_subscriptions(user_openid) VALUES ('smoke-subscriber')")
                subscription = db.execute("SELECT id FROM broadcast_subscriptions").fetchone()[0]
                for number, state in ((1, "sent"), (2, "retry")):
                    db.execute("INSERT INTO issue_notifications(repository,number,title,author,created_at,url,state) VALUES ('owner/another',?,'test','test','2020-01-01T00:00:00+00:00','https://example.com',?)", (number, state))
                    db.execute("INSERT INTO notification_deliveries(subscription_id,repository,number,state,next_attempt_at) VALUES (?,'owner/another',?,?,'2099-01-01T00:00:00+00:00')", (subscription, number, state))
            run("start")
            status = wait_for(lambda status: status["components"]["qq_binding"]["details"]["subscriber_count"] == 1)
            assert "smoke-secret" not in json.dumps(status)
            assert "smoke-token" not in json.dumps(status)
            run("stop")
            with sqlite3.connect(database) as db:
                assert db.execute("SELECT user_openid FROM broadcast_subscriptions").fetchall() == [("smoke-subscriber",)]
                assert db.execute("SELECT cursor FROM monitored_repositories WHERE name='owner/another'").fetchone() == ("2020-01-01T00:00:00+00:00",)
                assert db.execute("SELECT number,state FROM notification_deliveries ORDER BY number").fetchall() == [(1, "sent"), (2, "retry")]
            print("Production Compose smoke passed: loopback port, atomic config reload, subscription/cursor/delivery persistence, redacted status; external network blocked.")
        finally:
            run("down")


if __name__ == "__main__":
    main()
