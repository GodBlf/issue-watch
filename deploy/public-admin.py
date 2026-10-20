#!/usr/bin/env python3
"""Install the dedicated public admin proxy. Requires Nginx and Certbot >=5.4.

`render` is read-only. `install` checks local prerequisites, tests ACME staging,
then provisions production TLS; it never configures cloud firewall rules.
"""
import argparse
import ipaddress
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile

MARKER="# Managed by issue-watch public-admin\n"

def public_ip(value):
    try:
        ip=ipaddress.ip_address(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("a literal public IP is required") from error
    if ip.is_unspecified or ip.is_multicast or ip.is_link_local:
        raise argparse.ArgumentTypeError("a unicast IP is required")
    return str(ip)

def origin(ip):
    return "https://"+(f"[{ip}]" if ":" in ip else ip)

def render(ip,prefix=Path("/"),tls=True,http_port=80,https_port=443):
    webroot=prefix/"var/lib/issue-watch-acme"
    certificate=prefix/"etc/letsencrypt/live/issue-watch-ip"
    # Prefix is an administrative filesystem root, never a request parameter.
    for path in (webroot,certificate):
        if any(char in str(path) for char in '\n\r";$ {}'):
            raise ValueError("installation prefix cannot contain whitespace or Nginx syntax")
    host=f"[{ip}]" if ":" in ip else ip
    config=MARKER+f'''server {{
    listen {http_port};
    listen [::]:{http_port};
    server_name {host};
    location ^~ /.well-known/acme-challenge/ {{ root {webroot}; try_files $uri =404; }}
    location / {{ return 404; }}
}}
'''
    if tls:
        config+=f'''server {{
    listen {https_port} ssl;
    listen [::]:{https_port} ssl;
    server_name {host};
    ssl_certificate {certificate}/fullchain.pem;
    ssl_certificate_key {certificate}/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    client_max_body_size 16k;
    if ($host != "{ip}") {{ return 444; }}
    location ^~ /internal/ {{ return 404; }}
    location / {{
        proxy_pass http://127.0.0.1:8081;
        proxy_set_header Host {host};
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto https;
        proxy_read_timeout 20s;
        proxy_hide_header X-Powered-By;
    }}
}}
'''
    return config

def run(*args):
    return subprocess.run([str(arg) for arg in args],check=True,capture_output=True,text=True,timeout=180).stdout.strip()

def write(path,text,mode=0o644):
    path.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w",dir=path.parent,delete=False) as temporary:
        name=Path(temporary.name)
        temporary.write(text)
    try:
        name.chmod(mode)
        name.replace(path)
    finally:
        name.unlink(missing_ok=True)

def preflight(args,site):
    if args.prefix==Path("/") and os.geteuid()!=0:
        raise ValueError("install requires root")
    if not ipaddress.ip_address(args.ip).is_global:
        raise ValueError("installation requires a globally routable IP")
    # Validate interpolated paths before running any command or writing config.
    render(args.ip,args.prefix)
    if not args.email or not re.fullmatch(r"[^\s@]+@[^\s@]+",args.email):
        raise ValueError("--email is required for ACME registration")
    if not Path(args.certbot).is_absolute() and args.certbot!="certbot":
        raise ValueError("--certbot must be an absolute executable path")
    if any(char in args.certbot for char in '\n\r\t ;$%"'):
        raise ValueError("invalid certbot executable path")
    match=re.search(r"certbot\s+(\d+)\.(\d+)",run(args.certbot,"--version"))
    if not match or tuple(map(int,match.groups()))<(5,4):
        raise ValueError("Certbot >=5.4 is required for IP webroot certificates")
    if site.is_symlink() or (site.exists() and not site.read_text().startswith(MARKER)):
        raise ValueError("refusing to replace an unmanaged Nginx configuration")
    configuration=subprocess.run(["nginx","-T"],capture_output=True,text=True,check=True,timeout=30)
    for part in re.split(r"# configuration file ",configuration.stdout+configuration.stderr)[1:]:
        filename,_,content=part.partition(":\n")
        if Path(filename)!=site and re.search(r"\bserver\s*\{",content):
            raise ValueError(f"existing Nginx site requires manual review: {filename}")
    for line in run("ss","-ltnp").splitlines():
        if re.search(r":(?:80|443)\s",line) and "nginx" not in line:
            raise ValueError("port 80 or 443 is occupied by another process")
    env_lines=(args.app_root/".env.local").read_text().splitlines()
    values=[line.split("=",1)[1].strip().strip('\"\'') for line in env_lines if line.startswith("ISSUE_WATCH_ADMIN_ORIGIN=")]
    if values!=[origin(args.ip)]:
        raise ValueError("set exactly one ISSUE_WATCH_ADMIN_ORIGIN in the application .env.local, then recreate its container")
    database=args.app_root/"data/issue-watch.sqlite3"
    with sqlite3.connect(database.as_uri()+"?mode=ro",uri=True) as db:
        initialized=db.execute("SELECT EXISTS(SELECT 1 FROM admin_credentials WHERE id=1 AND password_hash!='')").fetchone()[0]
    if not initialized:
        raise ValueError("initialize the administrator password before public installation")
    status=run("curl","--silent","--show-error","--max-time","5","--output","/dev/null","--write-out","%{http_code}","http://127.0.0.1:8081/api/auth/session")
    if status!="401":
        raise ValueError("running application must reject anonymous sessions with HTTP 401")

def renewal(args):
    directory=args.prefix/"etc/systemd/system"
    hook=args.prefix/"usr/local/lib/issue-watch/reload-admin-certificate"
    certificate=args.prefix/"etc/letsencrypt/live/issue-watch-ip/fullchain.pem"
    write(hook,f'''#!/bin/sh
set -eu
/usr/bin/openssl x509 -in {certificate} -noout -checkend 3600
/usr/bin/openssl x509 -in {certificate} -noout -checkip {args.ip}
/usr/sbin/nginx -t
/bin/systemctl reload nginx
''',0o755)
    write(directory/"issue-watch-certbot.service",f'''[Unit]
Description=Renew issue-watch public IP certificate
After=network-online.target nginx.service
Wants=network-online.target
[Service]
Type=oneshot
ExecStart={args.certbot} renew --cert-name issue-watch-ip --non-interactive --deploy-hook {hook}
''')
    write(directory/"issue-watch-certbot.timer",'''[Unit]
Description=Check issue-watch IP certificate renewal hourly
[Timer]
OnCalendar=hourly
RandomizedDelaySec=900
Persistent=true
[Install]
WantedBy=timers.target
''')

def install(args):
    if args.http_port!=80 or args.https_port!=443:
        raise ValueError("installation uses public ports 80 and 443")
    args.prefix=args.prefix.resolve()
    args.app_root=args.app_root.resolve()
    if not Path(args.certbot).is_absolute():
        args.certbot=shutil.which(args.certbot) or args.certbot
    site=args.prefix/"etc/nginx/conf.d/issue-watch-admin.conf"
    preflight(args,site)
    original=site.read_text() if site.exists() else None
    webroot=args.prefix/"var/lib/issue-watch-acme"
    webroot.mkdir(parents=True,exist_ok=True)
    try:
        # First boot serves only challenges. Reinstallation preserves an existing
        # TLS site until a replacement certificate has been obtained and checked.
        write(site,original or render(args.ip,args.prefix,tls=False))
        run("nginx","-t");run("systemctl","reload-or-restart","nginx")
        command=[args.certbot,"certonly","--webroot","--webroot-path",str(webroot),"--ip-address",args.ip,"--preferred-profile","shortlived","--cert-name","issue-watch-ip","--non-interactive","--agree-tos","--email",args.email]
        run(*command,"--dry-run")
        run(*command)
        certificate=args.prefix/"etc/letsencrypt/live/issue-watch-ip/fullchain.pem"
        run("openssl","x509","-in",certificate,"-noout","-checkend","3600")
        run("openssl","x509","-in",certificate,"-noout","-checkip",args.ip)
        renewal(args)
        write(site,render(args.ip,args.prefix))
        run("nginx","-t");run("systemctl","reload","nginx")
        run("systemctl","daemon-reload")
        run("systemctl","enable","--now","issue-watch-certbot.timer")
    except Exception:
        if original is None:site.unlink(missing_ok=True)
        else:write(site,original)
        try:
            run("nginx","-t");run("systemctl","reload","nginx")
        except subprocess.SubprocessError as recovery:
            print(f"Nginx configuration restored on disk but reload failed: {recovery}",file=sys.stderr)
        raise
    print(f"HTTPS configured at {origin(args.ip)}/; run external acceptance checks before declaring it live.")

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action",choices=["render","install"])
    parser.add_argument("--ip",required=True,type=public_ip)
    parser.add_argument("--prefix",type=Path,default=Path("/"))
    parser.add_argument("--http-port",type=int,default=80)
    parser.add_argument("--https-port",type=int,default=443)
    parser.add_argument("--email")
    parser.add_argument("--app-root",type=Path,default=Path("/home/godblf/issue-watch"))
    parser.add_argument("--certbot",default="/opt/issue-watch-certbot/bin/certbot")
    args=parser.parse_args()
    if args.action=="render":
        if not 1<=args.http_port<=65535 or not 1<=args.https_port<=65535:
            raise ValueError("invalid port")
        print(render(args.ip,args.prefix,http_port=args.http_port,https_port=args.https_port),end="")
    else:install(args)

if __name__=="__main__":
    try:main()
    except (OSError,ValueError,sqlite3.Error,subprocess.SubprocessError) as error:
        print(f"public admin setup failed: {error}",file=sys.stderr)
        if isinstance(error,subprocess.CalledProcessError) and error.stderr:
            print(error.stderr[-4000:],file=sys.stderr)
        sys.exit(1)
