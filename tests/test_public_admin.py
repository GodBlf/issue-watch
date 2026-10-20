"""Deployment CLI boundary; no production server or CA is contacted."""
import os
import grp
from pathlib import Path
import subprocess
import sys
import tempfile
import sqlite3
import json
import unittest

SCRIPT=Path(__file__).resolve().parents[1]/"deploy/public-admin.py"

class PublicAdminTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix="public-admin-")
        self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)
        self.prefix=self.root/"system"
        self.app=self.root/"app"
        (self.app/"data").mkdir(parents=True)
        (self.app/".env.local").write_text("ISSUE_WATCH_ADMIN_ORIGIN=https://8.8.8.8\n")
        with sqlite3.connect(self.app/"data/issue-watch.sqlite3") as db:
            db.execute("CREATE TABLE admin_credentials(id INTEGER PRIMARY KEY,password_hash TEXT)")
            db.execute("INSERT INTO admin_credentials VALUES(1,'hashed')")
        self.log=self.root/"commands.jsonl"
        binaries=self.root/"bin";binaries.mkdir()
        fake=binaries/"fake"
        fake.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
tool=pathlib.Path(sys.argv[0]).name
with open(os.environ['COMMAND_LOG'],'a') as log:log.write(json.dumps([tool]+sys.argv[1:])+'\\n')
if tool=='certbot' and '--version' in sys.argv:print('certbot 5.4.0')
if tool=='curl':print('401')
if tool=='nginx' and '-T' in sys.argv:print('# configuration file /etc/nginx/nginx.conf:\\nevents {} http {}')
if tool=='nginx' and '-t' in sys.argv and os.environ.get('FAIL_TLS'):
    site=pathlib.Path(os.environ['SYSTEM_PREFIX'])/'etc/nginx/conf.d/issue-watch-admin.conf'
    if site.exists() and 'listen 443 ssl' in site.read_text():sys.exit(1)
if tool=='certbot' and '--dry-run' in sys.argv and os.environ.get('FAIL_STAGING'):sys.exit(1)
if tool=='certbot' and 'certonly' in sys.argv and '--dry-run' not in sys.argv:
    cert=pathlib.Path(os.environ['SYSTEM_PREFIX'])/'etc/letsencrypt/live/issue-watch-ip'
    cert.mkdir(parents=True,exist_ok=True)
    for name in ('fullchain.pem','privkey.pem'):(cert/name).write_text('new-'+name)
if tool=='openssl' and os.environ.get('FAIL_CERTIFICATE'):sys.exit(1)
if tool=='systemctl' and 'enable' in sys.argv and os.environ.get('FAIL_TIMER'):sys.exit(1)

''')
        fake.chmod(0o755)
        for name in ('nginx','caddy','certbot','systemctl','ss','curl','openssl'):(binaries/name).symlink_to(fake)
        self.env=dict(os.environ,PATH=str(binaries)+os.pathsep+os.environ['PATH'],COMMAND_LOG=str(self.log),SYSTEM_PREFIX=str(self.prefix))
    def install(self,proxy="nginx",**env):
        return subprocess.run([sys.executable,str(SCRIPT),'install','--ip','8.8.8.8','--email','admin@example.com','--prefix',str(self.prefix),'--app-root',str(self.app),'--certbot','certbot','--proxy',proxy],env=dict(self.env,**env),capture_output=True,text=True)
    def test_install_tests_staging_before_production_and_enables_renewal(self):
        result=self.install()
        self.assertEqual(result.returncode,0,result.stderr)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        issuance=[command for command in commands if command[0]=='certbot' and 'certonly' in command]
        self.assertEqual(len(issuance),2)
        self.assertIn('--dry-run',issuance[0]);self.assertNotIn('--dry-run',issuance[1])
        config=(self.prefix/'etc/nginx/conf.d/issue-watch-admin.conf').read_text()
        self.assertIn('listen 443 ssl',config)
        timer=(self.prefix/'etc/systemd/system/issue-watch-certbot.timer').read_text()
        self.assertIn('OnCalendar=hourly',timer)
        self.assertIn(['systemctl','enable','--now','issue-watch-certbot.timer'],commands)
    def test_staging_failure_never_requests_production_or_enables_management(self):
        result=self.install(FAIL_STAGING='1')
        self.assertNotEqual(result.returncode,0)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        issuance=[command for command in commands if command[0]=='certbot' and 'certonly' in command]
        self.assertEqual(len(issuance),1)
        self.assertIn('--dry-run',issuance[0])
        self.assertFalse((self.prefix/'etc/nginx/conf.d/issue-watch-admin.conf').exists())
        self.assertFalse((self.prefix/'etc/systemd/system/issue-watch-certbot.timer').exists())
    def test_invalid_tls_configuration_restores_existing_site_without_reloading_it(self):
        site=self.prefix/'etc/nginx/conf.d/issue-watch-admin.conf'
        site.parent.mkdir(parents=True)
        original='# Managed by issue-watch public-admin\n# previous challenge-only configuration\n'
        site.write_text(original)
        result=self.install(FAIL_TLS='1')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(site.read_text(),original)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertNotIn(['systemctl','enable','--now','issue-watch-certbot.timer'],commands)
    def test_failed_reinstallation_keeps_previous_certificate_after_certbot_replaces_lineage(self):
        self.assertEqual(self.install().returncode,0)
        site=self.prefix/'etc/nginx/conf.d/issue-watch-admin.conf'
        original=site.read_text()
        certificate=next(line.strip().split()[1].rstrip(';') for line in original.splitlines() if line.strip().startswith('ssl_certificate '))
        Path(certificate).write_text('previous-good-certificate')
        result=self.install(FAIL_TIMER='1')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(site.read_text(),original)
        self.assertEqual(Path(certificate).read_text(),'previous-good-certificate')
    def test_issuance_pins_staging_and_production_ca(self):
        result=self.install()
        self.assertEqual(result.returncode,0,result.stderr)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        issuance=[command for command in commands if command[0]=='certbot' and 'certonly' in command]
        self.assertIn('https://acme-staging-v02.api.letsencrypt.org/directory',issuance[0])
        self.assertIn('https://acme-v02.api.letsencrypt.org/directory',issuance[1])
    def test_renewal_keeps_deployed_certificate_when_candidate_is_invalid(self):
        result=self.install()
        self.assertEqual(result.returncode,0,result.stderr)
        current=self.prefix/'var/lib/issue-watch-admin-tls/current'
        old_certificate=(current/'fullchain.pem').read_bytes()
        lineage=self.prefix/'etc/letsencrypt/live/issue-watch-ip'
        (lineage/'fullchain.pem').write_text('invalid renewed certificate')
        result=subprocess.run([sys.executable,str(SCRIPT),'promote','--ip','8.8.8.8','--prefix',str(self.prefix)],env=dict(self.env,FAIL_CERTIFICATE='1'),capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual((current/'fullchain.pem').read_bytes(),old_certificate)
        # A later valid candidate replaces both files via the same renewal entry.
        (lineage/'fullchain.pem').write_text('valid renewed certificate')
        result=subprocess.run([sys.executable,str(SCRIPT),'promote','--ip','8.8.8.8','--prefix',str(self.prefix)],env=self.env,capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual((current/'fullchain.pem').read_text(),'valid renewed certificate')
    def test_uninitialized_password_does_not_enable_challenges_or_request_a_certificate(self):
        with sqlite3.connect(self.app/'data/issue-watch.sqlite3') as db:db.execute('DELETE FROM admin_credentials')
        result=self.install()
        self.assertNotEqual(result.returncode,0)
        self.assertIn('initialize',result.stderr)
        commands=[json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertFalse(any('certonly' in command for command in commands))
        self.assertFalse((self.prefix/'etc/nginx/conf.d/issue-watch-admin.conf').exists())
    def test_rendered_proxy_isolates_internal_health_and_plain_http(self):
        result=subprocess.run([sys.executable,str(SCRIPT),"render","--ip","203.0.113.10"],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn("listen 443 ssl",result.stdout)
        self.assertIn("location ^~ /internal/ { return 404; }",result.stdout)
        self.assertIn("location ^~ /.well-known/acme-challenge/",result.stdout)
        http=result.stdout.split("server {",2)[1]
        self.assertNotIn("proxy_pass",http)
        self.assertIn("location / { return 404; }",http)
        self.assertIn("proxy_set_header X-Real-IP $remote_addr",result.stdout)
    def test_caddy_proxy_uses_ip_tls_and_isolates_http_and_internal_paths(self):
        result=subprocess.run([sys.executable,str(SCRIPT),'render','--proxy','caddy','--ip','203.0.113.10'],capture_output=True,text=True)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn('https://203.0.113.10',result.stdout)
        self.assertIn('http://203.0.113.10',result.stdout)
        self.assertIn('handle /internal/*',result.stdout)
        self.assertIn('handle /.well-known/acme-challenge/*',result.stdout)
        self.assertIn('header_up X-Real-IP {remote_host}',result.stdout)
        self.assertIn('reverse_proxy 127.0.0.1:8081',result.stdout)
    @unittest.skipUnless(os.geteuid()==0 and any(group.gr_name=='caddy' for group in grp.getgrall()),'requires root and Caddy service group')
    def test_caddy_install_preserves_existing_site_and_configuration_permissions_on_failure(self):
        main=self.prefix/'etc/caddy/Caddyfile'
        main.parent.mkdir(parents=True)
        original='existing.example.com {\n    reverse_proxy 127.0.0.1:8080\n}\n'
        main.write_text(original)
        main.chmod(0o640)
        os.chown(main,0,grp.getgrnam('caddy').gr_gid)
        previous=main.stat()
        result=self.install(proxy='caddy',FAIL_STAGING='1')
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(main.read_text(),original)
        self.assertEqual(main.stat().st_mode,previous.st_mode)
        self.assertEqual(main.stat().st_gid,previous.st_gid)
        result=self.install(proxy='caddy')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn(original,main.read_text())
        self.assertIn('default_sni 8.8.8.8',main.read_text())
        self.assertIn('import ',main.read_text())
        self.assertEqual(main.stat().st_mode,previous.st_mode)
        self.assertEqual(main.stat().st_gid,previous.st_gid)
        key=self.prefix/'var/lib/issue-watch-admin-tls/current/privkey.pem'
        self.assertEqual(key.stat().st_mode & 0o777,0o640)
        self.assertEqual(key.stat().st_gid,previous.st_gid)
        hook=(self.prefix/'usr/local/lib/issue-watch/reload-admin-certificate').read_text()
        self.assertIn('--proxy caddy',hook)
    def test_non_ip_and_injection_are_rejected(self):
        for ip in ["example.com","203.0.113.10;include /tmp/evil;","0.0.0.0"]:
            result=subprocess.run([sys.executable,str(SCRIPT),"render","--ip",ip],capture_output=True,text=True)
            self.assertNotEqual(result.returncode,0)

if __name__=="__main__":unittest.main()
