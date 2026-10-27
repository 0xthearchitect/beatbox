"""Read-only PostgreSQL login probe, using Python's standard library."""
import base64
import hashlib
import hmac
import json
import secrets
import socket
import ssl
import stringprep
import struct
import sys
import time
import unicodedata


def scram_password(password):
    """PostgreSQL SASLprep (Unicode 3.2), with its raw-password fallback."""
    mapped = ''.join(' ' if stringprep.in_table_c12(char) else char
                     for char in password if not stringprep.in_table_b1(char))
    prepared = unicodedata.ucd_3_2_0.normalize('NFKC', mapped)
    forbidden = (stringprep.in_table_a1, stringprep.in_table_c12,
                 stringprep.in_table_c21, stringprep.in_table_c22,
                 stringprep.in_table_c3, stringprep.in_table_c4,
                 stringprep.in_table_c5, stringprep.in_table_c6,
                 stringprep.in_table_c7, stringprep.in_table_c8,
                 stringprep.in_table_c9)
    if any(check(char) for char in prepared for check in forbidden):
        return password.encode()
    if any(stringprep.in_table_d1(char) for char in prepared):
        if (any(stringprep.in_table_d2(char) for char in prepared)
                or not stringprep.in_table_d1(prepared[0])
                or not stringprep.in_table_d1(prepared[-1])):
            return password.encode()
    return prepared.encode()


def probe(config):
    connection = socket.create_connection((config['address'], config['port']), timeout=5)
    try:
        def read(size):
            result = b''
            while len(result) < size:
                part = connection.recv(size - len(result))
                if not part:
                    raise ValueError('protocol_error')
                result += part
            return result

        def send(kind, body):
            connection.sendall(kind + struct.pack('!I', len(body) + 4) + body)

        connection.sendall(struct.pack('!II', 8, 80877103))
        answer = read(1)
        if answer == b'S':
            connection = ssl.create_default_context().wrap_socket(connection, server_hostname=config['address'])
        elif answer != b'N':
            raise ValueError('protocol_error')
        startup = struct.pack('!I', 196608) + b'user\0' + config['username'].encode() + b'\0database\0' + config['database'].encode() + b'\0\0'
        connection.sendall(struct.pack('!I', len(startup) + 4) + startup)
        password = scram_password(config['password'])
        first = expected = None
        queried = verified = row = False
        for _ in range(100):
            kind = read(1)
            size = struct.unpack('!I', read(4))[0]
            if not 4 <= size <= 1048576:
                raise ValueError('protocol_error')
            body = read(size - 4)
            if kind == b'E':
                fields = dict((part[:1], part[1:]) for part in body.split(b'\0') if part)
                code = fields.get(b'C', b'').decode()
                return 'authentication_failed' if code.startswith('28') else 'database_missing' if code == '3D000' else 'query_failed'
            if kind == b'R':
                method = struct.unpack('!I', body[:4])[0]
                if method == 0:
                    verified = True
                elif method == 3:
                    if not isinstance(connection, ssl.SSLSocket):
                        return 'tls_required'
                    send(b'p', config['password'].encode() + b'\0')
                elif method == 5:
                    digest = hashlib.md5((config['password'] + config['username']).encode()).hexdigest().encode()
                    send(b'p', b'md5' + hashlib.md5(digest + body[4:8]).hexdigest().encode() + b'\0')
                elif method == 10:
                    if b'SCRAM-SHA-256' not in body[4:].split(b'\0'):
                        return 'unsupported_authentication'
                    nonce = secrets.token_hex(18)
                    first = 'n=,r=' + nonce
                    initial = ('n,,' + first).encode()
                    send(b'p', b'SCRAM-SHA-256\0' + struct.pack('!I', len(initial)) + initial)
                elif method == 11 and first:
                    server = body[4:].decode()
                    parts = dict(field.split('=', 1) for field in server.split(','))
                    rounds = int(parts['i'])
                    if not parts['r'].startswith(nonce) or len(parts['r']) <= len(nonce) or not 1 <= rounds <= 1000000:
                        raise ValueError('protocol_error')
                    final = 'c=biws,r=' + parts['r']
                    salted = hashlib.pbkdf2_hmac('sha256', password, base64.b64decode(parts['s'], validate=True), rounds)
                    client = hmac.digest(salted, b'Client Key', 'sha256')
                    transcript = (first + ',' + server + ',' + final).encode()
                    signature = hmac.digest(hashlib.sha256(client).digest(), transcript, 'sha256')
                    proof = bytes(a ^ b for a, b in zip(client, signature))
                    expected = base64.b64encode(hmac.digest(hmac.digest(salted, b'Server Key', 'sha256'), transcript, 'sha256')).decode()
                    send(b'p', (final + ',p=' + base64.b64encode(proof).decode()).encode())
                elif method == 12:
                    if expected is None or not hmac.compare_digest(body[4:].decode(), 'v=' + expected):
                        return 'authentication_failed'
                else:
                    return 'unsupported_authentication'
            elif kind == b'Z':
                if not verified:
                    raise ValueError('protocol_error')
                if queried:
                    return 'query_ok' if row else 'query_failed'
                send(b'Q', b'SELECT 1\0')
                queried = True
            elif kind == b'D' and queried:
                row = body == b'\0\1\0\0\0\1' + b'1'
        return 'protocol_error'
    finally:
        connection.close()


if __name__ == '__main__':
    started = time.monotonic()
    try:
        status = probe(json.loads(bytes.fromhex(sys.argv[1])))
    except ssl.SSLError:
        status = 'tls_error'
    except OSError:
        status = 'connection_failed'
    except (ValueError, KeyError, UnicodeError, struct.error):
        status = 'protocol_error'
    print('%d %s' % (int((time.monotonic() - started) * 1000), status))
