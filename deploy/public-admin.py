#!/usr/bin/env python3
"""Install the public admin proxy. Requires Nginx or Caddy and Certbot >=5.4.

`render` is read-only. `install` checks local prerequisites, tests ACME staging,
then provisions production TLS; it never configures cloud firewall rules.
"""
import argparse
from contextlib import contextmanager
import fcntl
import grp
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
    certificate=prefix/"var/lib/issue-watch-admin-tls/current"
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

def render_caddy(ip,prefix=Path("/"),tls=True,http_port=80,https_port=443):
    render(ip,prefix)  # Apply the same administrative path validation.
    host=f"[{ip}]" if ":" in ip else ip
    certificate=prefix/"var/lib/issue-watch-admin-tls/current"
    config=MARKER+f'''http://{host}:{http_port} {{
    handle /.well-known/acme-challenge/* {{
        root * {prefix}/var/lib/issue-watch-acme
        file_server
    }}
    handle {{
        respond 404
    }}
}}
'''
    if tls:
        config+=f'''https://{host}:{https_port} {{
    tls {certificate}/fullchain.pem {certificate}/privkey.pem
    handle /internal/* {{
        respond 404
    }}
    handle {{
        request_body {{
            max_size 16KB
        }}
        reverse_proxy 127.0.0.1:8081 {{
            header_up Host {host}
            header_up X-Real-IP {{remote_host}}
            header_up X-Forwarded-For {{remote_host}}
            header_up X-Forwarded-Proto https
        }}
    }}
}}
'''
    return config

def caddy_main(original,site,ip):
    # Browsers omit SNI for literal IPs. Select the IP certificate for those
    # connections while preserving explicit-SNI selection for existing domains.
    defaults=re.findall(r"(?m)^\s*default_sni\s+(\S+)\s*$",original)
    if defaults and defaults!=[ip]:
        raise ValueError("existing Caddy default_sni requires manual review")
    if not defaults:
        start=re.match(r"(?:\s|#[^\n]*(?:\n|$))*",original).end()
        if original[start:start+1]=="{":
            original=original[:start+1]+f"\n    default_sni {ip}\n"+original[start+1:]
        else:
            original=f"{{\n    default_sni {ip}\n}}\n"+original
    directive=f"import {site}"
    if directive not in original.splitlines():original=original.rstrip()+"\n"+directive+"\n"
    return original

def proxy_config(args,tls=True):
    renderer=render_caddy if args.proxy=="caddy" else render
    return renderer(args.ip,args.prefix,tls,args.http_port,args.https_port)

def validate_proxy(args):
    if args.proxy=="caddy":run("caddy","validate","--config",args.prefix/"etc/caddy/Caddyfile")
    else:run("nginx","-t")

def reload_proxy(args,initial=False):
    run("systemctl","reload-or-restart" if initial else "reload",args.proxy)

def run(*args):
    return subprocess.run([str(arg) for arg in args],check=True,capture_output=True,text=True,timeout=180).stdout.strip()

def write(path,text,mode=0o644,owner=None):
    path.parent.mkdir(parents=True,exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w",dir=path.parent,delete=False) as temporary:
        name=Path(temporary.name)
        temporary.write(text)
    try:
        if owner is not None:os.chown(name,*owner)
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
        raise ValueError("refusing to replace an unmanaged proxy configuration")
    if args.proxy=="nginx":
        configuration=subprocess.run(["nginx","-T"],capture_output=True,text=True,check=True,timeout=30)
        for part in re.split(r"# configuration file ",configuration.stdout+configuration.stderr)[1:]:
            filename,_,content=part.partition(":\n")
            if Path(filename)!=site and re.search(r"\bserver\s*\{",content):
                raise ValueError(f"existing Nginx site requires manual review: {filename}")
    else:
        main=args.prefix/"etc/caddy/Caddyfile"
        if not main.is_file() or main.is_symlink():
            raise ValueError("a regular existing Caddyfile is required")
        validate_proxy(args)
        grp.getgrnam("caddy")
    for line in run("ss","-ltnp").splitlines():
        if re.search(r":(?:80|443)\s",line) and args.proxy not in line:
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

@contextmanager
def activate_certificate(args):
    # Certbot owns its live symlinks. Nginx uses an independent immutable copy,
    # so a failed issuance/deployment cannot change its certificate on restart.
    directory=args.prefix/"var/lib/issue-watch-admin-tls"
    directory.mkdir(parents=True,exist_ok=True,mode=0o700)
    if args.proxy=="caddy":
        directory.chmod(0o750)
        os.chown(directory,0,grp.getgrnam("caddy").gr_gid)
    current=directory/"current"
    if current.exists() and not current.is_symlink():
        raise ValueError("refusing to replace an unmanaged certificate directory")
    previous=os.readlink(current) if current.is_symlink() else None
    candidate=Path(tempfile.mkdtemp(prefix="release-",dir=directory))
    link=directory/"candidate"
    try:
        source=args.prefix/"etc/letsencrypt/live/issue-watch-ip"
        for name in ("fullchain.pem","privkey.pem"):
            shutil.copyfile(source/name,candidate/name)
            (candidate/name).chmod(0o600)
        if args.proxy=="caddy":
            group=grp.getgrnam("caddy").gr_gid
            candidate.chmod(0o750)
            os.chown(candidate,0,group)
            for name in ("fullchain.pem","privkey.pem"):
                (candidate/name).chmod(0o640)
                os.chown(candidate/name,0,group)
        run("openssl","x509","-in",candidate/"fullchain.pem","-noout","-checkend","3600")
        run("openssl","x509","-in",candidate/"fullchain.pem","-noout","-checkip",args.ip)
        public_key=run("openssl","x509","-in",candidate/"fullchain.pem","-pubkey","-noout")
        if public_key!=run("openssl","pkey","-in",candidate/"privkey.pem","-pubout"):
            raise ValueError("certificate and private key do not match")
        link.unlink(missing_ok=True)
        link.symlink_to(candidate.name)
        link.replace(current)
        try:
            yield
        except Exception:
            if previous is None:current.unlink(missing_ok=True)
            else:
                link.symlink_to(previous)
                link.replace(current)
            raise
    except Exception:
        shutil.rmtree(candidate)
        raise
    finally:
        link.unlink(missing_ok=True)
    # Retain the previous release for inspection/recovery, bound disk growth.
    for release in directory.glob("release-*"):
        if release.name not in (candidate.name,previous):
            try:shutil.rmtree(release)
            except OSError as error:
                print(f"certificate deployed; unable to remove old release {release}: {error}",file=sys.stderr)

def promote(args):
    try:
        with activate_certificate(args):
            validate_proxy(args)
            reload_proxy(args)
    except Exception:
        # The on-disk pointer is restored before any recovery reload.
        validate_proxy(args)
        reload_proxy(args)
        raise

def renewal(args):
    directory=args.prefix/"etc/systemd/system"
    hook=args.prefix/"usr/local/lib/issue-watch/reload-admin-certificate"
    script=args.prefix/"usr/local/lib/issue-watch/public-admin.py"
    write(hook,f'''#!/bin/sh
set -eu
exec /usr/bin/python3 {script} promote --ip {args.ip} --prefix {args.prefix} --proxy {args.proxy}
''',0o755)
    write(directory/"issue-watch-certbot.service",f'''[Unit]
Description=Renew issue-watch public IP certificate
After=network-online.target {args.proxy}.service
Wants=network-online.target
[Service]
Type=oneshot
ExecStart={args.certbot} renew --cert-name issue-watch-ip --server https://acme-v02.api.letsencrypt.org/directory --non-interactive --deploy-hook {hook}
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
    site=args.prefix/("etc/caddy/issue-watch-admin.Caddyfile" if args.proxy=="caddy" else "etc/nginx/conf.d/issue-watch-admin.conf")
    preflight(args,site)
    original=site.read_text() if site.exists() else None
    main=args.prefix/"etc/caddy/Caddyfile" if args.proxy=="caddy" else None
    original_main=main.read_text() if main else None
    main_stat=main.stat() if main else None
    main_attributes=(main_stat.st_mode & 0o7777,(main_stat.st_uid,main_stat.st_gid)) if main_stat else ()
    renewal_files=[args.prefix/"usr/local/lib/issue-watch/reload-admin-certificate",
                   args.prefix/"etc/systemd/system/issue-watch-certbot.service",
                   args.prefix/"etc/systemd/system/issue-watch-certbot.timer"]
    saved_renewal={path:(path.read_text(),path.stat().st_mode & 0o777) if path.exists() else None for path in renewal_files}
    webroot=args.prefix/"var/lib/issue-watch-acme"
    webroot.mkdir(parents=True,exist_ok=True)
    try:
        # First boot serves only challenges. Reinstallation preserves an existing
        # TLS site until a replacement certificate has been obtained and checked.
        write(site,original or proxy_config(args,tls=False))
        if main:
            write(main,caddy_main(original_main,site,args.ip),*main_attributes)
        validate_proxy(args);reload_proxy(args,initial=True)
        command=[args.certbot,"certonly","--webroot","--webroot-path",str(webroot),"--ip-address",args.ip,"--preferred-profile","shortlived","--cert-name","issue-watch-ip","--non-interactive","--agree-tos","--email",args.email]
        run(*command,"--dry-run","--server","https://acme-staging-v02.api.letsencrypt.org/directory")
        run(*command,"--server","https://acme-v02.api.letsencrypt.org/directory")
        with activate_certificate(args):
            renewal(args)
            write(site,proxy_config(args))
            validate_proxy(args);reload_proxy(args)
            run("systemctl","daemon-reload")
            run("systemctl","enable","--now","issue-watch-certbot.timer")
    except Exception:
        for path,saved in saved_renewal.items():
            if saved is None:path.unlink(missing_ok=True)
            else:write(path,*saved)
        if main:write(main,original_main,*main_attributes)
        if original is None:site.unlink(missing_ok=True)
        else:write(site,original)
        try:
            run("systemctl","daemon-reload")
            validate_proxy(args);reload_proxy(args)
        except subprocess.SubprocessError as recovery:
            print(f"Proxy configuration restored on disk but reload failed: {recovery}",file=sys.stderr)
        raise
    print(f"HTTPS configured at {origin(args.ip)}/; run external acceptance checks before declaring it live.")

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action",choices=["render","install","promote"])
    parser.add_argument("--ip",required=True,type=public_ip)
    parser.add_argument("--proxy",choices=["nginx","caddy"],default="nginx")
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
        print(proxy_config(args),end="")
    else:
        args.prefix=args.prefix.resolve()
        # Installation and the renewal hook must not switch certificates together.
        lock=args.prefix/"var/lib/issue-watch-admin-tls.lock"
        lock.parent.mkdir(parents=True,exist_ok=True)
        with lock.open("a") as handle:
            fcntl.flock(handle,fcntl.LOCK_EX)
            if args.action=="install":install(args)
            else:promote(args)

if __name__=="__main__":
    try:main()
    except (OSError,ValueError,sqlite3.Error,subprocess.SubprocessError) as error:
        print(f"public admin setup failed: {error}",file=sys.stderr)
        if isinstance(error,subprocess.CalledProcessError) and error.stderr:
            print(error.stderr[-4000:],file=sys.stderr)
        sys.exit(1)
