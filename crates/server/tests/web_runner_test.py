import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import ssl
import time
import unittest
import urllib.error
import urllib.request

RUNNER = Path(__file__).resolve().parents[1] / 'definitions' / 'web-run.py'
spec = importlib.util.spec_from_file_location('web_runner', RUNNER)
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def wait_ready(address, process):
    for _ in range(100):
        if process.poll() is not None:
            raise AssertionError('Runner exited early')
        try:
            return urllib.request.urlopen(address, timeout=0.2).read()
        except (OSError, urllib.error.URLError):
            time.sleep(0.03)
    raise AssertionError('Website did not become ready')


class WebRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='beatbox-web-')
        self.root = Path(self.temporary.name)
        self.process = None

    def tearDown(self):
        if self.process is not None and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=4)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.temporary.cleanup()

    def launch(self, mode, folder, page='', command='', health='/'):
        number = port()
        own = self.root / 'service' / 'page'
        self.process = subprocess.Popen([sys.executable, str(RUNNER), 'serve', mode, str(folder), page, str(number), command, health, str(own), '0', str(self.root / 'tls')],
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return number, 'http://127.0.0.1:' + str(number)

    def test_existing_folder_and_assets_survive_start_and_restart(self):
        original = '<h1>Existing website</h1>'
        (self.root / 'index.html').write_text(original)
        (self.root / 'app.css').write_text('body { color: green; }')
        number, address = self.launch('folder', self.root, '<h1>Must not replace files</h1>')
        self.assertEqual(wait_ready(address, self.process).decode(), original)
        self.assertEqual(urllib.request.urlopen(address + '/app.css').read(), b'body { color: green; }')
        self.process.terminate(); self.process.wait(timeout=4)
        self.assertEqual((self.root / 'index.html').read_text(), original)
        number, address = self.launch('folder', self.root)
        self.assertEqual(wait_ready(address, self.process).decode(), original)

    def test_simple_page_is_literal_and_written_only_in_page_mode(self):
        page = '<h1>Olá</h1> {port} "quotes"; $(false)'
        # A site already in the configured folder is never written over.
        (self.root / 'index.html').write_text('<h1>Someone else\'s site</h1>')
        number, address = self.launch('page', self.root, page)
        self.assertEqual(wait_ready(address, self.process).decode(), page + '\n')
        self.assertEqual((self.root / 'index.html').read_text(), '<h1>Someone else\'s site</h1>')
        self.assertEqual((self.root / 'service' / 'page' / 'index.html').read_text(), page + '\n')

    def test_application_receives_port_cwd_and_quoted_arguments(self):
        (self.root / 'app.py').write_text("""import http.server,os,sys
from pathlib import Path
assert Path.cwd().name.startswith('beatbox-web-')
assert sys.argv[1] == 'two words; $(false)'
assert os.environ['HOST'] == '0.0.0.0'
assert sys.argv[2] == os.environ['PORT']
class Handler(http.server.BaseHTTPRequestHandler):
 def do_GET(self):
  self.send_response(200);self.end_headers();self.wfile.write(b'application ready')
http.server.HTTPServer(('127.0.0.1',int(os.environ['PORT'])),Handler).serve_forever()
""")
        command = shlex.quote(sys.executable) + " app.py 'two words; $(false)' {port}"
        number, address = self.launch('application', self.root, command=command)
        self.assertEqual(wait_ready(address, self.process), b'application ready')
        self.process.terminate();self.process.wait(timeout=4)
        with self.assertRaises(OSError):
            socket.create_connection(('127.0.0.1', number), timeout=0.2)

    def test_stopping_application_stops_descendants(self):
        child_port = port()
        (self.root / 'app.py').write_text("""import os,subprocess,sys,http.server,signal
signal.signal(signal.SIGTERM,signal.SIG_IGN)
subprocess.Popen([sys.executable,'-m','http.server',sys.argv[1],'--bind','127.0.0.1','--directory',os.getcwd()])
http.server.HTTPServer(('127.0.0.1',int(os.environ['PORT'])),http.server.SimpleHTTPRequestHandler).serve_forever()
""")
        (self.root / 'index.html').write_text('ready')
        command = shlex.quote(sys.executable) + ' app.py ' + str(child_port)
        number, address = self.launch('application', self.root, command=command)
        wait_ready(address, self.process)
        wait_ready('http://127.0.0.1:' + str(child_port), self.process)
        self.process.terminate();self.process.wait(timeout=4)
        for closed in (number, child_port):
            with self.assertRaises(OSError):
                socket.create_connection(('127.0.0.1', closed), timeout=0.2)

    def test_an_application_goes_with_a_runner_killed_outright(self):
        (self.root / 'index.html').write_text('ready')
        command = shlex.quote(sys.executable) + ' -m http.server {port} --bind 127.0.0.1'
        number, address = self.launch('application', self.root, command=command)
        wait_ready(address, self.process)
        self.process.kill(); self.process.wait(timeout=4)
        for _ in range(100):
            try:
                socket.create_connection(('127.0.0.1', number), timeout=0.2).close()
                time.sleep(0.03)
            except OSError:
                return
        self.fail('the application kept the port after its runner was killed')

    def test_prerequisite_failures_are_structured_and_do_not_touch_files(self):
        cases = [('folder',self.root/'missing','', '/', 'server.web_folder_missing'),
                 ('folder',Path('relative'),'', '/', 'server.web_folder_absolute'),
                 ('application',self.root,'', '/', 'server.web_command_missing'),
                 ('application',self.root,'"unfinished', '/', 'server.web_command_invalid'),
                 ('application',self.root,'beatbox-no-program', '/', 'server.program_missing'),
                 ('folder',self.root,'', 'bad', 'server.web_health_path_invalid'),
                 ('folder',self.root,'', '/', 'server.web_index_missing')]
        for mode,folder,command,health,expected in cases:
            with self.subTest(expected=expected):
                done = subprocess.run([sys.executable,str(RUNNER),'check',mode,str(folder),'do not write','8000',command,health,str(self.root/'own'),'0',str(self.root/'tls')],text=True,capture_output=True)
                self.assertNotEqual(done.returncode,0)
                self.assertEqual(json.loads(done.stdout)['code'],expected)
        self.assertFalse((self.root/'index.html').exists())

    @unittest.skipUnless(shutil.which('openssl'), 'openssl is not installed')
    def test_page_answers_https_with_a_certificate_kept_across_restarts(self):
        unchecked = ssl.create_default_context()
        unchecked.check_hostname = False
        unchecked.verify_mode = ssl.CERT_NONE
        seen = []
        for _ in range(2):
            number, secure = port(), port()
            self.process = subprocess.Popen([sys.executable, str(RUNNER), 'serve', 'page', '/unused', '<p>tls</p>', str(number), '', '/',
                                             str(self.root / 'service' / 'page'), str(secure), str(self.root / 'tls')],
                                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_ready('http://127.0.0.1:%d' % number, self.process)
            with urllib.request.urlopen('https://127.0.0.1:%d/' % secure, context=unchecked, timeout=2) as reply:
                self.assertEqual(reply.read(), b'<p>tls</p>\n')
            seen.append(ssl.get_server_certificate(('127.0.0.1', secure)))
            self.process.terminate(); self.process.wait(timeout=4)
        self.assertEqual(seen[0], seen[1])

    def test_https_is_refused_for_an_application(self):
        done = subprocess.run([sys.executable, str(RUNNER), 'check', 'application', str(self.root), '', '8000',
                               'python3 app.py', '/', str(self.root / 'own'), '8443', str(self.root / 'tls')],
                              text=True, capture_output=True)
        self.assertEqual(json.loads(done.stdout)['code'], 'server.web_https_application')

    def test_symlinks_cannot_serve_files_outside_root(self):
        public = self.root / 'public';public.mkdir()
        (public/'index.html').write_text('public')
        secret = self.root / 'private.txt';secret.write_text('private')
        (public/'secret.txt').symlink_to(secret)
        number,address = self.launch('folder',public)
        wait_ready(address,self.process)
        with self.assertRaises(urllib.error.HTTPError) as refused:
            urllib.request.urlopen(address+'/secret.txt')
        self.assertEqual(refused.exception.code,404)


if __name__ == '__main__':
    unittest.main()
