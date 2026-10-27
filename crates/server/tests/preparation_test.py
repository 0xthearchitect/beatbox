"""Preparation refuses incompatible data and preserves a working config."""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
import threading
import unittest
from urllib.request import urlopen

DEFINITIONS = Path(__file__).resolve().parents[1] / 'definitions'


class Preparation(unittest.TestCase):
    def test_mailpit_health_checks_smtp_and_the_web_inbox(self):
        definition = json.loads((DEFINITIONS / 'smtp.json').read_text())
        command = definition['health']['command']
        for greeting, status, expected in [('220 Ready', '200 OK', 0), ('220 Ready', '503 Unavailable', 1),
                                           ('500 Wrong service', '200 OK', 1)]:
            with socket.socket() as smtp, socket.socket() as web:
                for listener in [smtp, web]:
                    listener.bind(('127.0.0.1', 0)); listener.listen(); listener.settimeout(2)
                def answer(listener, response, receive=False):
                    try:
                        connection, _ = listener.accept()
                        with connection:
                            if receive:
                                connection.settimeout(2); connection.recv(4096)
                            connection.sendall(response.encode() + b'\r\n')
                    except (TimeoutError, OSError):
                        pass
                threads = [threading.Thread(target=answer, args=(smtp, greeting)),
                           threading.Thread(target=answer, args=(web, 'HTTP/1.1 ' + status, True))]
                for thread in threads:
                    thread.start()
                argv = [part.replace('{port}', str(smtp.getsockname()[1])).replace('{web_port}', str(web.getsockname()[1])) for part in command]
                done = subprocess.run(argv, capture_output=True, timeout=3)
                for thread in threads:
                    thread.join(timeout=3)
                self.assertEqual(done.returncode, expected)

    @unittest.skipUnless(shutil.which('nginx'), 'nginx is not installed')
    def test_real_nginx_serves_spaced_path_and_stops_its_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); root.chmod(0o755)
            website = root / 'site with spaces; #'; website.mkdir()
            (website / 'index.html').write_text('nginx preparation verified')
            config = root / 'instance/nginx.conf'
            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0)); port = listener.getsockname()[1]
            command = ['python3', str(DEFINITIONS / 'nginx-run.py'), 'serve', 'folder', str(website), '', str(port), '0', str(config)]
            child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 5
                while True:
                    try:
                        with urlopen('http://127.0.0.1:%d/' % port, timeout=1) as reply:
                            self.assertEqual(reply.read(), b'nginx preparation verified')
                        break
                    except OSError:
                        if child.poll() is not None or time.monotonic() > deadline:
                            self.fail('nginx failed to serve the website')
                        time.sleep(0.02)
                self.assertTrue((config.parent / 'nginx.pid').is_file())
            finally:
                child.terminate()
                output = child.communicate(timeout=5)
            self.assertEqual(child.returncode, 0, output)
            self.assertFalse((config.parent / 'nginx.pid').exists())

    def test_postgresql_selects_cluster_version_and_refuses_explicit_mismatch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / 'bin'; binaries.mkdir()
            for name in ['initdb', 'psql', 'pg_isready']:
                file = binaries / name; file.write_text('#!/bin/sh\nexit 0\n'); file.chmod(0o755)
            postgres = binaries / 'postgres'
            postgres.write_text('#!/bin/sh\necho "postgres (PostgreSQL) 99.4"\n'); postgres.chmod(0o755)
            data = root / 'data'; data.mkdir()
            (data / 'PG_VERSION').write_text('99\n')
            argv = ['bash', str(DEFINITIONS / 'postgresql-run.sh'), 'check', '', str(data), '5544']
            done = subprocess.run(argv, env=dict(os.environ, PATH=str(binaries) + ':' + os.environ['PATH']), capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            postgres.write_text('#!/bin/sh\necho "postgres (PostgreSQL) 98.4"\n')
            argv[3] = str(binaries)
            done = subprocess.run(argv, capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0)
            self.assertEqual(json.loads(done.stdout)['code'], 'server.pg_version_missing')
            self.assertEqual((data / 'PG_VERSION').read_text(), '99\n')

    def test_nginx_quotes_paths_and_keeps_active_config_when_validation_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            website = root / 'site with spaces; #'; website.mkdir()
            binaries = root / 'bin'; binaries.mkdir()
            nginx = binaries / 'nginx'
            nginx.write_text('#!/bin/sh\nexit 0\n'); nginx.chmod(0o755)
            config = root / 'instance/nginx.conf'
            argv = ['python3', str(DEFINITIONS / 'nginx-run.py'), 'check', 'folder', str(website), '', '8080', '0', str(config)]
            env = dict(os.environ, PATH=str(binaries) + ':' + os.environ['PATH'])
            done = subprocess.run(argv, env=env, capture_output=True, text=True)
            self.assertEqual(json.loads(done.stdout)['code'], 'server.web_index_missing')
            self.assertFalse(config.exists())
            (website / 'index.html').write_text('home')
            done = subprocess.run(argv, env=env, capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            active = config.read_text()
            self.assertIn('root "%s";' % website, active)
            self.assertIn('pid "%s/nginx.pid";' % config.parent, active)
            nginx.write_text('#!/bin/sh\necho "nginx: [emerg] unknown directive \\"bogus\\"" >&2\nexit 1\n')
            done = subprocess.run(argv, env=env, capture_output=True, text=True)
            self.assertNotEqual(done.returncode, 0)
            refused = json.loads(done.stdout)
            self.assertEqual(refused['code'], 'server.nginx_invalid_config')
            self.assertEqual(refused['values']['said'], 'nginx: [emerg] unknown directive "bogus"')
            self.assertEqual(config.read_text(), active)

    def test_nginx_proxies_to_an_application_address_and_refuses_anything_else(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binaries = root / 'bin'; binaries.mkdir()
            nginx = binaries / 'nginx'
            nginx.write_text('#!/bin/sh\nexit 0\n'); nginx.chmod(0o755)
            config = root / 'instance/nginx.conf'
            env = dict(os.environ, PATH=str(binaries) + ':' + os.environ['PATH'])
            def check(upstream):
                argv = ['python3', str(DEFINITIONS / 'nginx-run.py'), 'check', 'proxy', '/nowhere', upstream, '8080', '0', str(config)]
                return subprocess.run(argv, env=env, capture_output=True, text=True)
            done = check('127.0.0.1:3000')
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn('proxy_pass http://127.0.0.1:3000;', config.read_text())
            self.assertNotIn('root ', config.read_text())
            for bad in ['x; include /etc/passwd', '127.0.0.1:3000 ', 'host:${port}', '']:
                with self.subTest(upstream=bad):
                    done = check(bad)
                    self.assertEqual(json.loads(done.stdout)['code'], 'server.nginx_bad_upstream')


if __name__ == '__main__':
    unittest.main()
