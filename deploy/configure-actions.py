#!/usr/bin/env python3
"""Provision a restricted deployment key and repository secrets from a trusted admin machine.

Requires gh login and an existing, verified SSH connection. Never prints secrets.
"""
from pathlib import Path
import shlex
import subprocess
import tempfile

HOST = "172.197.176.142"
TARGET = "godblf@" + HOST
REPO = "GodBlf/issue-watch"
SSH = ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes"]


def run(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def main():
    known_file = Path.home() / ".ssh/known_hosts"
    lookup = run(["ssh-keygen", "-F", HOST, "-f", str(known_file)], capture_output=True, text=True).stdout
    known = "\n".join(line for line in lookup.splitlines() if line and not line.startswith("#")) + "\n"
    if not known.strip():
        raise RuntimeError("a previously verified server host key is required")
    run(SSH + [TARGET, "test -x /usr/local/lib/issue-watch/ssh-entry"])
    with tempfile.TemporaryDirectory(prefix="issue-watch-key-") as directory:
        key = Path(directory) / "key"
        run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "issue-watch-actions", "-f", str(key)])
        public = key.with_suffix(".pub").read_text().strip()
        entry = 'restrict,command="/usr/local/lib/issue-watch/ssh-entry" ' + public + "\n"
        update = """
import os, pathlib, sys
os.umask(0o077)
folder = pathlib.Path.home() / '.ssh'
folder.mkdir(mode=0o700, exist_ok=True)
path = folder / 'authorized_keys'
lines = path.read_text().splitlines() if path.exists() else []
lines = [line for line in lines if not line.endswith(' issue-watch-actions')]
path.write_text('\\n'.join(lines) + '\\n' + sys.stdin.read())
path.chmod(0o600)
"""
        run(SSH + [TARGET, "python3 -c " + shlex.quote(update)], input=entry, text=True)
        restricted = SSH + ["-o", "IdentitiesOnly=yes", "-i", str(key), TARGET]
        denied = subprocess.run(restricted + ["true"], capture_output=True, text=True)
        if denied.returncode != 1 or "invalid deployment request" not in denied.stderr:
            raise RuntimeError("restricted key did not reject arbitrary commands")
        forwarding = subprocess.run(restricted[:-1] + ["-W", "127.0.0.1:8081", TARGET], capture_output=True, timeout=20)
        if forwarding.returncode == 0:
            raise RuntimeError("restricted key allowed TCP forwarding")
        run(["gh", "secret", "set", "DEPLOY_SSH_KEY", "--repo", REPO], input=key.read_bytes())
        run(["gh", "secret", "set", "DEPLOY_KNOWN_HOSTS", "--repo", REPO], input=known, text=True)
    print("Deployment secrets configured; arbitrary commands and forwarding rejected.")


if __name__ == "__main__":
    main()
