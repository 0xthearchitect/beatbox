"""Runner embedded in the Website definition. No shell evaluates settings."""
import functools
import http.server
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import signal
import ssl
import subprocess
import sys
import threading


def tied(parent):
    """For a child, run before it starts: it is sent SIGTERM when this
    runner goes, even killed outright. An orphan would keep the port, and
    every start after it fail."""
    def preexec():
        import ctypes
        ctypes.CDLL(None).prctl(1, signal.SIGTERM)  # PR_SET_PDEATHSIG
        if os.getppid() != parent:  # the runner went before prctl could see it
            os._exit(1)
    return preexec


class Refused(Exception):
    def __init__(self, code, **values):
        self.reason = dict(code=code, values=dict((k, str(v)) for k, v in values.items()))


def prepare(mode, root, port, command, health_path, https_port=0, tls=None):
    if not health_path.startswith('/') or any(c in health_path for c in '\r\n'):
        raise Refused('server.web_health_path_invalid')
    if https_port:
        if mode == 'application':
            raise Refused('server.web_https_application')
        try:
            self_signed(tls)
        except FileNotFoundError:
            raise Refused('server.program_missing', service='Website', program='openssl') from None
        except subprocess.SubprocessError:
            raise Refused('server.tls_failed') from None
    if mode == 'page':
        return None
    if not root.is_absolute():
        raise Refused('server.web_folder_absolute', folder=root)
    if not root.is_dir():
        raise Refused('server.web_folder_missing', folder=root)
    # Checked at /, a folder without its index.html can only ever answer 404.
    if mode == 'folder' and health_path == '/' and not (root / 'index.html').is_file():
        raise Refused('server.web_index_missing', folder=root)
    if mode == 'application':
        try:
            words = shlex.split(command.replace('{' + 'port' + '}', str(port)))
        except ValueError:
            raise Refused('server.web_command_invalid') from None
        if not words:
            raise Refused('server.web_command_missing')
        program = words[0]
        found = (root / program) if '/' in program else shutil.which(program)
        if not found or not Path(found).is_file() or not os.access(found, os.X_OK):
            raise Refused('server.program_missing', service='Website', program=program)
        return words
    account = visitor()
    index = root / 'index.html'
    if account and index.is_file() and not readable_as(account, index):
        raise Refused('server.web_not_readable', folder=root, user=account.pw_name)
    return None


def self_signed(folder):
    """A certificate for this machine's name, made once and kept: a browser
    told to trust it once is not asked again after every restart."""
    certificate, key = folder / 'certificate.pem', folder / 'key.pem'
    if not (certificate.is_file() and key.is_file()):
        folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        name = os.uname().nodename or 'localhost'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1',
                        '-nodes', '-days', '825', '-keyout', str(key), '-out', str(certificate), '-subj', '/CN=' + name,
                        '-addext', 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1' % name],
                       check=True, capture_output=True, timeout=20)
        key.chmod(0o600)
    return certificate, key


class Secure(http.server.ThreadingHTTPServer):
    """HTTPS: the handshake in each request's own thread, so one slow or
    plain-HTTP visitor never holds up the others."""
    context = None

    def finish_request(self, request, client_address):
        try:
            request = self.context.wrap_socket(request, server_side=True)
        except (ssl.SSLError, OSError):
            return
        super().finish_request(request, client_address)


def visitor():
    """Who serves the files once the port is open. Started as root, a web
    server that stays root hands any file it can be tricked into to anyone
    on the network; so it becomes the distribution's web account."""
    if os.geteuid() != 0:
        return None
    for name in ('www-data', 'nginx', 'http', 'nobody'):  # Debian, Fedora, Arch, any
        try:
            return pwd.getpwnam(name)
        except KeyError:
            pass
    return None


def become(account):
    os.setgroups([])
    os.setgid(account.pw_gid)
    os.setuid(account.pw_uid)


def readable_as(account, path):
    """Whether `account` can open `path`, asked the only reliable way: try."""
    child = os.fork()
    if child == 0:
        try:
            become(account)
            with open(path, 'rb'):
                pass
            os._exit(0)
        except BaseException:
            os._exit(1)
    return os.waitpid(child, 0)[1] == 0


class Files(http.server.SimpleHTTPRequestHandler):
    def list_directory(self, path):
        self.send_error(404, 'No index.html in this folder')
        return None

    def translate_path(self, path):
        target = Path(super().translate_path(path)).resolve()
        if not target.is_relative_to(Path(self.directory).resolve()):
            return str(Path(self.directory) / '.beatbox-unavailable')
        return str(target)


def application(words, root, port):
    # Keep the runner's identity stable for the Server's process checks.
    env = dict(os.environ, PORT=str(port), HOST='0.0.0.0')
    child = None
    stopping = False

    def stop(signum, frame):
        nonlocal stopping
        stopping = True
        if child is not None:
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    child = subprocess.Popen(words, cwd=root, env=env, start_new_session=True, preexec_fn=tied(os.getpid()))
    if stopping:
        stop(None, None)
    try:
        while True:
            try:
                code = child.wait(timeout=0.1)
                return code if code >= 0 else 128 - code
            except subprocess.TimeoutExpired:
                if stopping:
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        pass
                    return 0
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def main():
    operation, mode, folder, page, port, command, health_path, own, https_port, tls = sys.argv[1:]
    root, port, https_port, tls = Path(folder), int(port), int(https_port), Path(tls)
    try:
        words = prepare(mode, root, port, command, health_path, https_port, tls)
    except Refused as error:
        print(json.dumps(error.reason), flush=True)
        return 1
    if operation == 'check':
        return 0
    if mode == 'application':
        return application(words, root, port)
    if mode == 'page':
        # The service's own folder, never the one in `root`: a simple page
        # must not write over a site someone already has there.
        root = Path(own)
        root.mkdir(mode=0o755, parents=True, exist_ok=True)
        root.chmod(0o755)
        (root / 'index.html').write_text(page + '\n', encoding='utf-8')
        (root / 'index.html').chmod(0o644)
        # Its parents are BeatBox's own; the web account only has to pass
        # through them (o+x), never list them.
        for parent in (root.parent, root.parent.parent):
            parent.chmod(parent.stat().st_mode | 0o001)
    handler = functools.partial(Files, directory=str(root))
    server = http.server.ThreadingHTTPServer(('0.0.0.0', port), handler)
    if https_port:
        # Bound and keyed while still root; the key stays root's to read.
        secure = Secure(('0.0.0.0', https_port), handler)
        secure.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        secure.context.load_cert_chain(*self_signed(tls))
        threading.Thread(target=secure.serve_forever, daemon=True).start()
        print('Website listening for HTTPS on port %s' % https_port, flush=True)
    account = visitor()
    if account:
        become(account)
    print('Website listening on port %s' % port, flush=True)
    server.serve_forever()
    return 0


if __name__ == '__main__':
    sys.exit(main())
