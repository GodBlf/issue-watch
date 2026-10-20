"""Isolated HTTPS smoke: real Nginx/Caddy, real Rust HTTP fixture, and a fresh Chrome.

Uses an ephemeral local certificate pinned only in the test browser and trusted
explicitly by the HTTP client. Does not contact a CA, GitHub, QQ or production.
Requires Playwright as an external test tool, not a runtime dependency.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

REPO=Path(__file__).resolve().parents[1]

def run(*args,**kwargs):
    return subprocess.run(list(map(str,args)),check=True,capture_output=True,**kwargs)

def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1",0));return listener.getsockname()[1]

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    proxy_options=parser.add_mutually_exclusive_group(required=True)
    proxy_options.add_argument('--nginx')
    proxy_options.add_argument('--caddy')
    parser.add_argument('--chrome',required=True)
    parser.add_argument('--playwright',default='playwright')
    args=parser.parse_args()
    # Never displace a developer's existing local service.
    with socket.socket() as probe:probe.bind(('127.0.0.1',8081))
    artifacts=run('cargo','+1.90.0','test','--locked','--test','admin_authentication','--no-run','--message-format=json',cwd=REPO,text=True)
    fixture=next(item['executable'] for line in artifacts.stdout.splitlines() if (item:=json.loads(line)).get('executable') and item.get('target',{}).get('name')=='admin_authentication')
    run('cargo','+1.90.0','build','--locked',cwd=REPO)
    with tempfile.TemporaryDirectory(prefix='issue-watch-https-') as directory:
        root=Path(directory);http_port,https_port=port(),port()
        base=f'https://127.0.0.1:{https_port}'
        cert=root/'var/lib/issue-watch-admin-tls/current';cert.mkdir(parents=True)
        run('openssl','req','-x509','-newkey','rsa:2048','-nodes','-days','2','-subj','/CN=127.0.0.1','-addext','subjectAltName=IP:127.0.0.1','-keyout',cert/'privkey.pem','-out',cert/'fullchain.pem')
        public=run('openssl','x509','-in',cert/'fullchain.pem','-pubkey','-noout').stdout
        der=run('openssl','pkey','-pubin','-outform','DER',input=public).stdout
        pin=base64.b64encode(hashlib.sha256(der).digest()).decode()
        config=run('python3',REPO/'deploy/public-admin.py','render','--ip','127.0.0.1','--prefix',root,'--http-port',http_port,'--https-port',https_port,'--proxy','caddy' if args.caddy else 'nginx',text=True).stdout
        # Production listens publicly; the smoke fixture is restricted to loopback.
        for number in (http_port,https_port):
            config=config.replace(f'listen {number}',f'listen 127.0.0.1:{number}').replace(f'listen [::]:{number}',f'listen [::1]:{number}')
        if args.caddy:
            # Caddy, like Chrome, must support IP connections without TLS SNI.
            # Add bind only to site blocks, leaving nested handlers unchanged.
            config=config.replace(f'http://127.0.0.1:{http_port} {{',f'http://127.0.0.1:{http_port} {{\n    bind 127.0.0.1').replace(f'https://127.0.0.1:{https_port} {{',f'https://127.0.0.1:{https_port} {{\n    bind 127.0.0.1')
            config='{\n    admin off\n    auto_https disable_redirects\n    default_sni 127.0.0.1\n}\n'+config
            (root/'Caddyfile').write_text(config)
        (root/'site.conf').write_text(config)
        (root/'nginx.conf').write_text(f'pid {root}/nginx.pid; error_log {root}/error.log; events {{}} http {{ access_log off; include {root}/site.conf; }}')
        env=dict(os.environ,ISSUE_WATCH_BROWSER_FIXTURE_ROOT=str(root),ISSUE_WATCH_ADMIN_ORIGIN=base)
        log=(root/'fixture.log').open('w')
        backend=subprocess.Popen([fixture,'--ignored','--exact','https_browser_fixture','--nocapture'],env=env,stdout=log,stderr=log)
        proxy=None
        try:
            for _ in range(100):
                if backend.poll() is not None:raise RuntimeError((root/'fixture.log').read_text())
                try:
                    urllib.request.urlopen('http://127.0.0.1:8081/internal/status',timeout=1).close();break
                except OSError:time.sleep(.1)
            else:raise RuntimeError('HTTP fixture did not start')
            if args.caddy:
                run(args.caddy,'validate','--config',root/'Caddyfile','--adapter','caddyfile')
                command=[args.caddy,'run','--config',str(root/'Caddyfile'),'--adapter','caddyfile']
            else:
                run(args.nginx,'-t','-p',root,'-c',root/'nginx.conf')
                command=[args.nginx,'-p',str(root),'-c',str(root/'nginx.conf'),'-g','daemon off;']
            proxy=subprocess.Popen(command,stdout=log,stderr=log)
            context=ssl.create_default_context(cafile=str(cert/'fullchain.pem'))
            def status(url,**headers):
                try:
                    with urllib.request.urlopen(urllib.request.Request(url,headers=headers),context=context,timeout=3) as response:return response.status
                except urllib.error.HTTPError as error:return error.code
            for _ in range(100):
                try:
                    if status(base+'/api/auth/session')==401:break
                except OSError:time.sleep(.1)
            else:raise RuntimeError('HTTPS proxy did not start')
            assert status(base+'/internal/status')==404
            assert status(base+'/%69nternal/status',**{'X-Real-IP':'127.0.0.1','X-Forwarded-For':'127.0.0.1'})==404
            assert status(f'http://127.0.0.1:{http_port}/login')==404
            challenge=root/'var/lib/issue-watch-acme/.well-known/acme-challenge/probe'
            challenge.parent.mkdir(parents=True);challenge.write_text('challenge')
            assert status(f'http://127.0.0.1:{http_port}/.well-known/acme-challenge/probe')==200
            browser_env=dict(env,SMOKE_ORIGIN=base,CHROME_BIN=args.chrome,PLAYWRIGHT_MODULE=args.playwright,SMOKE_SPKI=pin)
            result=run('node',REPO/'tests/public_admin_https.cjs',env=browser_env,cwd=REPO,text=True)
            print(result.stdout.strip())
            print('TLS chain/IP verified with test trust; HTTP isolation and encoded internal-path isolation verified.')
        finally:
            for child in (proxy,backend):
                if child is not None:
                    child.terminate()
                    try:child.wait(timeout=10)
                    except subprocess.TimeoutExpired:child.kill();child.wait()
            log.close()

if __name__=='__main__':main()
