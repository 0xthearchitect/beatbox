"""Generate and validate an instance-local nginx configuration."""
import grp
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import signal
import subprocess
import sys


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


UPSTREAM = re.compile(r'(https?://)?[A-Za-z0-9.\-]+(:[0-9]{1,5})?(/[A-Za-z0-9._~/%-]*)?')


def main():
    operation, mode, folder, upstream, port, https_port, config, *tls = sys.argv[1:]
    config, port, https_port = Path(config), int(port), int(https_port)
    # The certificate's folder: the instance's own, or one its Server shares.
    tls = Path(tls[0]) if tls else config.parent / 'tls'
    nginx = shutil.which('nginx')
    if not nginx:
        return refuse('server.program_missing', service='Website', program='nginx')
    account = worker()
    if mode == 'proxy':
        # Written into the configuration whole, so only what an address can
        # hold: no spaces, quotes, braces, semicolons or variables.
        if not UPSTREAM.fullmatch(upstream):
            return refuse('server.nginx_bad_upstream', upstream=upstream)
        target = upstream if '://' in upstream else 'http://' + upstream
        site = (' location / {\n  proxy_pass %s;\n  proxy_http_version 1.1;\n'
                '  proxy_set_header Host $host;\n  proxy_set_header X-Real-IP $remote_addr;\n'
                '  proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n'
                '  proxy_set_header X-Forwarded-Proto $scheme;\n'
                '  proxy_set_header Upgrade $http_upgrade;\n'
                '  proxy_set_header Connection $connection_upgrade;\n }\n' % target)
    else:
        root = Path(folder)
        if not root.is_absolute():
            return refuse('server.web_folder_absolute', folder=folder)
        if not root.is_dir():
            return refuse('server.web_folder_missing', folder=folder)
        if any(char in folder for char in '\r\n\0$'):
            return refuse('server.nginx_bad_folder')
        index = root / 'index.html'
        if not index.is_file():
            return refuse('server.web_index_missing', folder=folder)
        if account and not readable_as(account, index):
            return refuse('server.web_not_readable', folder=folder, user=account.pw_name)
        site = ' root "%s";\n' % folder.replace('\\', '\\\\').replace('"', '\\"')
    listen = ' listen %d;\n' % port
    if https_port:
        try:
            certificate, key = self_signed(tls)
        except FileNotFoundError:
            return refuse('server.program_missing', service='Website', program='openssl')
        except subprocess.SubprocessError:
            return refuse('server.tls_failed')
        listen += ' listen %d ssl;\n ssl_certificate "%s";\n ssl_certificate_key "%s";\n' % (https_port, certificate, key)
    prefix = str(config.parent)
    content = ''
    if account:
        content += 'user %s %s;\n' % (account.pw_name, grp.getgrgid(account.pw_gid).gr_name)
    content += 'pid "%s/nginx.pid";\nerror_log "%s/error.log";\nevents {}\nhttp {\n' % (prefix, prefix)
    mime = Path('/etc/nginx/mime.types')
    if mime.is_file():
        content += ' include /etc/nginx/mime.types;\n'
    content += ' map $http_upgrade $connection_upgrade { default upgrade; "" close; }\n'
    content += ' access_log "%s/access.log";\n server {\n%s%s }\n}\n' % (prefix, listen, site)
    config.parent.mkdir(parents=True, exist_ok=True)
    candidate = config.with_suffix('.candidate.conf')
    candidate.write_text(content, encoding='utf-8')
    try:
        try:
            checked = subprocess.run([nginx, '-t', '-c', str(candidate)], capture_output=True, text=True, timeout=2)
        except subprocess.TimeoutExpired:
            return refuse('server.nginx_invalid_config', said='nginx -t did not finish in 2 seconds')
        if checked.returncode:
            lines = checked.stderr.splitlines() or ['nginx -t failed']
            said = next((line for line in lines if '[emerg]' in line), lines[0])
            return refuse('server.nginx_invalid_config', said=said.strip())
        candidate.replace(config)
    finally:
        candidate.unlink(missing_ok=True)
    if operation == 'check':
        return 0
    child = subprocess.Popen([nginx, '-c', str(config), '-g', 'daemon off;'], start_new_session=True,
                             preexec_fn=tied(os.getpid()))
    stopping = False
    def stop(_signum, _frame):
        nonlocal stopping
        stopping = True
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        while True:
            try:
                return child.wait(timeout=0.1)
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


def worker():
    """Who nginx's workers run as. Started by root with no `user`, nginx
    falls back to nobody, and a folder only its owner can read answers 403;
    so name the distribution's web account, and check it can read."""
    if os.geteuid() != 0:
        return None
    for name in ('www-data', 'nginx', 'http', 'nobody'):  # Debian, Fedora, Arch, any
        try:
            return pwd.getpwnam(name)
        except KeyError:
            pass
    return None


def readable_as(account, path):
    """Whether `account` can open `path`, asked the only reliable way: try."""
    child = os.fork()
    if child == 0:
        try:
            os.initgroups(account.pw_name, account.pw_gid)
            os.setgid(account.pw_gid)
            os.setuid(account.pw_uid)
            with open(path, 'rb'):
                pass
            os._exit(0)
        except BaseException:
            os._exit(1)
    return os.waitpid(child, 0)[1] == 0


def self_signed(folder):
    """A certificate for this machine's name, made once and kept: a browser
    told to trust it once is not asked again after every restart."""
    certificate, key = folder / 'certificate.pem', folder / 'key.pem'
    if not (certificate.is_file() and key.is_file()):
        folder.mkdir(mode=0o700, parents=True, exist_ok=True)
        name = os.uname().nodename or 'localhost'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1', '-nodes', '-days', '825',
                        '-keyout', str(key), '-out', str(certificate), '-subj', '/CN=' + name,
                        '-addext', 'subjectAltName=DNS:%s,DNS:localhost,IP:127.0.0.1' % name],
                       check=True, capture_output=True, timeout=20)
        key.chmod(0o600)
    return certificate, key


def refuse(code, **values):
    print(json.dumps(dict(code=code, values=values)), flush=True)
    return 1


if __name__ == '__main__':
    sys.exit(main())
