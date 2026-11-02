#!/usr/bin/env python3
"""Linux release idle measurement. Browser not included; no third-party packages."""
import argparse
import json
import os
from pathlib import Path
import platform
import socket
import shutil
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser()
parser.add_argument('--seconds', type=int, default=600)
parser.add_argument('--bin-dir', type=Path, default=Path('target/release'))
parser.add_argument('--tray', action='store_true')
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
assert args.seconds > 0
binaries = args.bin_dir.resolve()
processes = {}

def measure(p):
    # stat: strip comm first (it may contain spaces or parentheses).
    fields = Path(f'/proc/{p.pid}/stat').read_text().rsplit(')', 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / os.sysconf('SC_CLK_TCK')
    mem = {}
    for line in Path(f'/proc/{p.pid}/smaps_rollup').read_text().splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] in ('Rss:', 'Pss:', 'Private_Clean:', 'Private_Dirty:'):
            mem[parts[0][:-1]] = int(parts[1])
    return {'cpu_seconds': cpu, 'rss_kib': mem['Rss'], 'pss_kib': mem['Pss'], 'private_kib': mem['Private_Clean'] + mem['Private_Dirty'], 'threads': int(fields[17])}

with tempfile.TemporaryDirectory(prefix='cf-perf-') as temp:
    data = Path(temp) / 'node'
    env = dict(os.environ, COFLEET_TOKEN='cofleet-local-measurement-token')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    subprocess.run([binaries/'cofleetctl', '--data-dir', data, 'init', '--name', 'idle-measurement', '--manager', f'http://127.0.0.1:{port}'], env=env, check=True, capture_output=True)
    def spawn(name, *arguments):
        processes[name] = subprocess.Popen([binaries/name, *arguments], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        spawn('cofleet-manager', '--listen', f'127.0.0.1:{port}')
        spawn('cofleet-agent', '--data-dir', data)
        if args.tray:
            spawn('cofleet-tray', '--data-dir', data)
        time.sleep(5)
        result = subprocess.run([binaries/'cofleetctl', '--data-dir', data, 'status'], env=env, check=True, capture_output=True, text=True)
        assert json.loads(result.stdout)['connected']
        initial = {name: measure(p) for name, p in processes.items()}
        samples = {name: [] for name in processes}
        start = time.monotonic()
        while time.monotonic() - start < args.seconds:
            time.sleep(max(0, min(1, args.seconds - (time.monotonic()-start))))
            for name, p in processes.items():
                assert p.poll() is None, f'{name} exited'
                samples[name].append(measure(p))
        elapsed = time.monotonic() - start
        summary = {}
        for name, values in samples.items():
            summary[name] = {
                'mean_rss_mib': round(statistics.mean(v['rss_kib'] for v in values)/1024, 2),
                'peak_rss_mib': round(max(v['rss_kib'] for v in values)/1024, 2),
                'mean_pss_mib': round(statistics.mean(v['pss_kib'] for v in values)/1024, 2),
                'mean_private_mib': round(statistics.mean(v['private_kib'] for v in values)/1024, 2),
                'rss_growth_kib': values[-1]['rss_kib'] - initial[name]['rss_kib'],
                'cpu_percent_one_core': round((values[-1]['cpu_seconds']-initial[name]['cpu_seconds'])/elapsed*100, 3),
                'threads': values[-1]['threads'],
                'binary_bytes': (binaries/name).stat().st_size,
            }
        report = {'measured_at': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'system': platform.platform(), 'cpu': platform.machine(), 'rustc': subprocess.check_output([shutil.which('rustc') or str(Path.home()/'.cargo/bin/rustc'), '--version'], text=True).strip(), 'transport': 'websocket-v1', 'build': 'release', 'duration_seconds': round(elapsed,2), 'warmup_seconds': 5, 'nodes': 1, 'heartbeat_seconds': 5, 'browser_included': False, 'processes': summary}
        args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2)+'\n')
        print(json.dumps(report, ensure_ascii=False, indent=2))
    finally:
        for p in reversed(list(processes.values())):
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill(); p.wait()
