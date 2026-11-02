#!/usr/bin/env python3
"""Process-level checks against compiled binaries; no third-party Python dependencies."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import unittest
import urllib.request
import urllib.error

BIN = Path(os.environ.get('COFLEET_BIN_DIR', 'target/debug')).resolve()
TOKEN = 'cofleet-integration-token-0123456789'

class Smoke(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='cf-')
        self.data = Path(self.tmp.name) / 'node'
        self.env = dict(os.environ, COFLEET_TOKEN=TOKEN)
        self.processes = []
        with socket.socket() as s:
            s.bind(('127.0.0.1', 0))
            self.port = s.getsockname()[1]
        self.url = f'http://127.0.0.1:{self.port}'
        self.ctl('init', '--name', 'test-node', '--manager', self.url, '--heartbeat-secs', '1')

    def tearDown(self):
        for p in reversed(self.processes):
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()
                    p.wait()
        self.tmp.cleanup()

    def ctl(self, *args, check=True):
        return subprocess.run([str(BIN / 'cofleetctl'), '--data-dir', str(self.data), *args], env=self.env, text=True, capture_output=True, timeout=8, check=check)

    def spawn(self, name, *args):
        p = subprocess.Popen([str(BIN / name), *args], env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.processes.append(p)
        return p

    def wait_for(self, fn, timeout=15):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                value = fn()
                if value:
                    return value
            except (OSError, ValueError, subprocess.CalledProcessError) as e:
                last = e
            time.sleep(.15)
        self.fail(f'condition not met: {last}')

    def status(self):
        return json.loads(self.ctl('status').stdout)

    def devices(self, token=TOKEN):
        req = urllib.request.Request(self.url + '/api/v1/devices', headers={'Authorization': 'Bearer ' + token})
        with urllib.request.urlopen(req, timeout=3) as r:
            return json.load(r)

    def manager(self):
        p = self.spawn('cofleet-manager', '--listen', f'127.0.0.1:{self.port}')
        self.wait_for(lambda: urllib.request.urlopen(self.url + '/healthz', timeout=1).status == 200)
        return p

    def test_lifecycle(self):
        manager = self.manager()
        with self.assertRaises(urllib.error.HTTPError) as error:
            self.devices('wrong')
        self.assertEqual(error.exception.code, 401)
        agent = self.spawn('cofleet-agent', '--data-dir', str(self.data))
        self.wait_for(lambda: self.status()['connected'])
        self.wait_for(lambda: len(self.devices()) == 1)
        identity = self.devices()[0]['device_id']
        # A manager command is delivered down the existing Agent-initiated socket.
        req = urllib.request.Request(self.url + f'/api/v1/devices/{identity}/refresh', data=b'', headers={'Authorization': 'Bearer ' + TOKEN}, method='POST')
        with urllib.request.urlopen(req, timeout=3) as response:
            self.assertEqual(response.status, 202)
            request_id = json.load(response)['request_id']
        self.wait_for(lambda: self.devices()[0]['last_report_id'] == request_id)
        duplicate = self.spawn('cofleet-agent', '--data-dir', str(self.data))
        self.assertNotEqual(duplicate.wait(timeout=5), 0)
        self.assertFalse(json.loads(self.ctl('pause').stdout)['accepting_tasks'])
        self.wait_for(lambda: not self.devices()[0]['accepting_tasks'])
        self.assertTrue(self.status()['connected'])
        agent.terminate()
        agent.wait(timeout=5)
        agent = self.spawn('cofleet-agent', '--data-dir', str(self.data))
        self.wait_for(lambda: self.status()['connected'])
        self.assertFalse(self.status()['accepting_tasks'])
        self.assertEqual(self.status()['device_id'], identity)
        manager.terminate()
        manager.wait(timeout=5)
        self.wait_for(lambda: not self.status()['connected'])
        # Local controls continue to work without the management server.
        self.assertTrue(json.loads(self.ctl('resume').stdout)['accepting_tasks'])
        self.manager()
        self.wait_for(lambda: self.status()['connected'], timeout=20)
        self.wait_for(lambda: len(self.devices()) == 1)
        self.assertEqual(self.devices()[0]['device_id'], identity)
        self.assertTrue(self.devices()[0]['accepting_tasks'])
        if os.name != 'nt':
            self.assertEqual((self.data.stat().st_mode & 0o777), 0o700)
            self.assertEqual(((self.data / 'agent.sock').stat().st_mode & 0o777), 0o600)
            self.assertEqual(((self.data / 'agent.toml').stat().st_mode & 0o777), 0o600)
            # Verify an oversized client is disconnected without harming Agent.
            with socket.socket(socket.AF_UNIX) as s:
                s.settimeout(5)
                s.connect(str(self.data / 'agent.sock'))
                s.sendall(b'x' * 17000 + b'\n')
                try:
                    self.assertEqual(s.recv(1), b'')
                except ConnectionResetError:
                    pass
            self.assertTrue(self.status()['connected'])
            # SIGKILL leaves a stale socket; the next Agent must safely recover it.
            agent.kill()
            agent.wait(timeout=5)
            self.spawn('cofleet-agent', '--data-dir', str(self.data))
            self.wait_for(lambda: self.status()['connected'])
        # Credential must never appear in normal status output.
        self.assertNotIn(TOKEN, self.ctl('status').stdout)

if __name__ == '__main__':
    unittest.main()
