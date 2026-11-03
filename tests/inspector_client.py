"""Minimal test-only RFC6455 client for Node Inspector through the authenticated proxy."""
import base64
import hashlib
import json
import os
import socket
import ssl
import struct
import urllib.parse


class Inspector:
    def __init__(self, url, headers):
        target = urllib.parse.urlsplit(url)
        port = target.port or (443 if target.scheme == 'https' else 80)
        connection = socket.create_connection((target.hostname, port), timeout=15)
        if target.scheme == 'https':
            connection = ssl.create_default_context().wrap_socket(connection, server_hostname=target.hostname)
        self.connection = connection
        self.reader = connection.makefile('rb')
        key = base64.b64encode(os.urandom(16)).decode()
        request_headers = dict(headers, Host=target.netloc, Upgrade='websocket', Connection='Upgrade',
                               **{'Sec-WebSocket-Key': key, 'Sec-WebSocket-Version': '13',
                                  'Origin': target.scheme + '://' + target.netloc})
        path = target.path + ('?' + target.query if target.query else '')
        connection.sendall(('GET ' + path + ' HTTP/1.1\r\n' + ''.join(
            name + ': ' + value + '\r\n' for name, value in request_headers.items()) + '\r\n').encode())
        status = self.reader.readline().decode()
        response = {}
        while True:
            line = self.reader.readline()
            if line == b'\r\n':
                break
            if not line:
                raise AssertionError('incomplete upgrade response')
            name, value = line.decode().split(':', 1)
            response[name.lower()] = value.strip()
        assert ' 101 ' in status, status
        expected = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
        assert response.get('sec-websocket-accept') == expected
        self.counter = 0
        self.events = []

    def send_frame(self, payload, opcode=1):
        mask = os.urandom(4)
        size = len(payload)
        header = bytes([0x80 | opcode])
        if size < 126:
            header += bytes([0x80 | size])
        elif size < 65536:
            header += bytes([0x80 | 126]) + struct.pack('!H', size)
        else:
            header += bytes([0x80 | 127]) + struct.pack('!Q', size)
        self.connection.sendall(header + mask + bytes(value ^ mask[index % 4] for index, value in enumerate(payload)))

    def exact(self, count):
        result = self.reader.read(count)
        if len(result) != count:
            raise EOFError('WebSocket closed')
        return result

    def receive(self):
        output = bytearray()
        while True:
            first, second = self.exact(2)
            length = second & 127
            if length == 126:
                length = struct.unpack('!H', self.exact(2))[0]
            elif length == 127:
                length = struct.unpack('!Q', self.exact(8))[0]
            assert length <= 1024 * 1024 and not second & 128
            payload = self.exact(length)
            opcode = first & 15
            if opcode == 9:
                self.send_frame(payload, 10)
                continue
            if opcode == 10:
                continue
            if opcode == 8:
                raise EOFError('WebSocket closed')
            output.extend(payload)
            if first & 128:
                return json.loads(output)

    def command(self, method, params=None):
        self.counter += 1
        self.send_frame(json.dumps({'id': self.counter, 'method': method, 'params': params or {}}).encode())
        while True:
            reply = self.receive()
            if reply.get('id') == self.counter:
                assert 'error' not in reply, reply
                return reply.get('result', {})
            self.events.append(reply)

    def event(self, method):
        while True:
            for index, event in enumerate(self.events):
                if event.get('method') == method:
                    return self.events.pop(index)
            self.events.append(self.receive())

    def close(self):
        self.reader.close()
        self.connection.close()
