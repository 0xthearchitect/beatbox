#!/usr/bin/env python3
"""Keep portable built-in definitions in sync with their runner sources."""
import json
from pathlib import Path

folder = Path(__file__).resolve().parents[1] / 'crates/server/definitions'
for name, runner in [('postgresql', 'postgresql-run.sh'), ('nginx', 'nginx-run.py')]:
    path = folder / (name + '.json')
    definition = json.loads(path.read_text(encoding='utf-8'))
    if name == 'postgresql':
        definition['version'] = '5'
        # The runner does the postgres-side work itself since version 4; a
        # query runs in the service's own database since version 5.
        definition['capabilities'][0]['run'] = ['bash', '/etc/beatbox/{_id}/postgresql-run.sh', 'query', '{bin_directory}', '/var/lib/postgresql/beatbox/{_id}', '{port}', '{sql}', '{database}']
        definition['files'] = definition['files'][:1]
        definition['runtime']['command'] = ['bash', '/etc/beatbox/{_id}/postgresql-run.sh', 'serve', '{bin_directory}', '/var/lib/postgresql/beatbox/{_id}', '{port}', '{database}', '{user}', '{initial_password}']
        definition['configuration'] = {
            'port': {'kind': 'number', 'default': 5432},
            'bin_directory': {'kind': 'text', 'default': ''},
            'initial_password': {'kind': 'secret'},
            'database': {'kind': 'text', 'default': ''},
            'user': {'kind': 'text', 'default': ''},
        }
        definition['description'] = 'A PostgreSQL database in UTF-8, initialized on first start and kept across restarts. Needs PostgreSQL installed in the guest. Finds its programs automatically and prepares the postgres account and data folder. The initial password only applies to a new database.'
        definition['prepare'] = ['bash', '/etc/beatbox/{_id}/postgresql-run.sh', 'check', '{bin_directory}', '/var/lib/postgresql/beatbox/{_id}', '{port}']
    else:
        definition['version'] = '5'
        arguments = ['{mode}', '{root}', '{upstream}', '{port}', '{https_port}', '/etc/beatbox/{_id}/nginx.conf', '{_tls}']
        definition['runtime']['command'] = ['python3', '/etc/beatbox/{_id}/nginx-run.py', 'serve'] + arguments
        definition['files'][0]['path'] = '/etc/beatbox/{_id}/nginx-run.py'
        definition['prepare'] = ['python3', '/etc/beatbox/{_id}/nginx-run.py', 'check'] + arguments
        definition['configuration'] = {
            'mode': {'kind': 'enum', 'values': ['folder', 'proxy'], 'default': 'folder'},
            'root': {'kind': 'text', 'default': '/var/www/html'},
            'upstream': {'kind': 'text', 'default': '127.0.0.1:3000'},
            'port': {'kind': 'number', 'default': 80},
            'https_port': {'kind': 'number', 'default': 0},
            'health_path': {'kind': 'text', 'default': '/'},
        }
        # Asked of a proxy too, whose application may answer only elsewhere.
        definition['health'] = {'type': 'http', 'port': '{port}', 'path': '{health_path}'}
        definition['interfaces'] = [
            {'transport': 'tcp', 'port': '{port}', 'protocol': 'http'},
            {'transport': 'tcp', 'port': '{https_port}', 'protocol': 'https'},
        ]
        definition['description'] = 'Serves a folder, or passes visitors on to a web application, using nginx. Can also answer HTTPS with a self-signed certificate. Requires nginx and Python 3 (and openssl for HTTPS). Validates configuration before starting and keeps PID and logs separate for each instance.'
    definition['files'][0]['content'] = (folder / runner).read_text(encoding='utf-8')
    path.write_text(json.dumps(definition, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')

# Email: hand-written but for the inbox check it embeds.
path = folder / 'smtp.json'
definition = json.loads(path.read_text(encoding='utf-8'))
definition['version'] = '3'
definition['files'] = [
    {'path': '/var/lib/beatbox/{_id}/README.txt', 'content': "Mailpit stores this service's lab messages in mailpit.db.\n"},
    {'path': '/etc/beatbox/{_id}/inbox-check.sh', 'content': (folder / 'inbox-check.sh').read_text(encoding='utf-8')},
]
definition['capabilities'] = [{
    'capability': 'email.received',
    'inputs': [{'name': 'search', 'kind': 'text', 'required': False}],
    'outputs': [{'name': 'exit_code', 'kind': 'number'}, {'name': 'stdout', 'kind': 'text'}, {'name': 'stderr', 'kind': 'text'}],
    'run': ['bash', '/etc/beatbox/{_id}/inbox-check.sh', '{web_port}', '{search}'],
}]
path.write_text(json.dumps(definition, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
