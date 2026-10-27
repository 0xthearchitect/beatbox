"""Real SCRAM login/SQL checks against an isolated temporary PostgreSQL."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'src/postgresql_probe.py'
spec = importlib.util.spec_from_file_location('postgresql_probe', SOURCE)
client = importlib.util.module_from_spec(spec)
spec.loader.exec_module(client)


class PostgreSQLProbe(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if os.geteuid() == 0 or not all(shutil.which(binary) for binary in ['initdb', 'pg_ctl', 'psql']):
            raise unittest.SkipTest('requires PostgreSQL binaries and an unprivileged Unix user')
        cls.temporary = tempfile.TemporaryDirectory(prefix='beatbox-pg-probe-')
        cls.root = Path(cls.temporary.name)
        cls.password = "pässword ' ; $(false)"
        cls.password_file = cls.root / 'password'
        cls.password_file.write_text(cls.password + '\n', encoding='utf-8')
        cls.password_file.chmod(0o600)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            cls.port = listener.getsockname()[1]
        cls.data = cls.root / 'data'
        subprocess.run(['initdb', '-D', str(cls.data), '--username=probe', '--auth=scram-sha-256', '--pwfile=' + str(cls.password_file)], capture_output=True, check=True)
        subprocess.run(['pg_ctl', '-D', str(cls.data), '-l', str(cls.root / 'log'), '-o', '-h 127.0.0.1 -p %s -k %s' % (cls.port, cls.root), '-w', 'start'], capture_output=True, check=True)
        cls.config = dict(address='127.0.0.1', port=cls.port, username='probe', database='postgres', password=cls.password)
        subprocess.run(['psql', '-h', '127.0.0.1', '-p', str(cls.port), '-U', 'probe', '-d', 'postgres', '-c',
                        "CREATE ROLE sasl LOGIN PASSWORD 'IX IX'; CREATE ROLE raw LOGIN PASSWORD E'a\\x07b'; CREATE ROLE modern LOGIN PASSWORD 'a\U0001F130b';"],
                       env=dict(os.environ, PGPASSWORD=cls.password), capture_output=True, check=True)

    @classmethod
    def tearDownClass(cls):
        subprocess.run(['pg_ctl', '-D', str(cls.data), '-m', 'immediate', '-w', 'stop'], capture_output=True, check=True)
        cls.temporary.cleanup()

    def test_login_and_read_only_query(self):
        self.assertEqual(client.probe(self.config), 'query_ok')

    def test_postgresql_saslprep_mapping_and_raw_fallback(self):
        self.assertEqual(client.scram_password('I\u00ADX\u00A0\u2168'), b'IX IX')
        self.assertEqual(client.scram_password('a\u0007b'), b'a\x07b')
        self.assertEqual(client.scram_password('a\u0627'), 'a\u0627'.encode())
        self.assertEqual(client.probe(dict(self.config, username='sasl', password='I\u00ADX\u00A0\u2168')), 'query_ok')
        self.assertEqual(client.probe(dict(self.config, username='raw', password='a\u0007b')), 'query_ok')
        self.assertEqual(client.probe(dict(self.config, username='modern', password='a\U0001F130b')), 'query_ok')

    def test_wrong_password_is_not_a_successful_port_test(self):
        self.assertEqual(client.probe(dict(self.config, password='wrong')), 'authentication_failed')

    def test_missing_database_and_unknown_user_have_distinct_results(self):
        self.assertEqual(client.probe(dict(self.config, database='missing')), 'database_missing')
        self.assertEqual(client.probe(dict(self.config, username='missing')), 'authentication_failed')

    def test_script_output_never_contains_credentials(self):
        done = subprocess.run(['python3', str(SOURCE), json.dumps(dict(self.config, password='private-invalid')).encode().hex()], capture_output=True, text=True, check=True)
        self.assertIn('authentication_failed', done.stdout)
        self.assertNotIn('private-invalid', done.stdout + done.stderr)

    def test_powershell_client_when_available(self):
        if not shutil.which('pwsh'):
            self.skipTest('PowerShell not installed')
        cases = [('probe', self.password, 'query_ok'), ('probe', 'wrong', 'authentication_failed'),
                 ('sasl', 'I\u00ADX\u00A0\u2168', 'query_ok'), ('raw', 'a\u0007b', 'query_ok'), ('modern', 'a\U0001F130b', 'query_ok')]
        for username, password, status in cases:
            encoded = json.dumps(dict(self.config, username=username, password=password)).encode().hex()
            script = "& {\n" + SOURCE.with_suffix('.ps1').read_text() + "\n} '" + encoded + "'"
            done = subprocess.run(['pwsh', '-NoProfile', '-NonInteractive', '-Command', script], capture_output=True, text=True, timeout=20, check=True)
            self.assertEqual(done.stdout.strip().split()[-1], status, done.stdout + done.stderr)


if __name__ == '__main__':
    unittest.main()
