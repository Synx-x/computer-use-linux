#!/usr/bin/env python3
"""Latency benchmark for computer-use-linux over MCP stdio.

Spawns `<binary> mcp`, opens a throwaway kitty window running `cat`, and times
each tool against that window only. The window is targeted by window_id, never
by class, so no input can reach another window.

Usage: mcp_bench.py [--bin PATH] [--runs N] [--json OUT]
"""
import argparse
import json
import os
import statistics
import subprocess
import time

# Session markers must not leak into the long-lived kitty child.
SCRUB_PREFIXES = ('CLAUDE_CODE', 'HERDR')


def clean_env():
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB_PREFIXES)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', default='computer-use-linux')
    ap.add_argument('--runs', type=int, default=5)
    ap.add_argument('--json')
    a = ap.parse_args()

    title = f'cul-bench-{os.getpid()}'
    sock = f'unix:/tmp/cul-bench-{os.getpid()}'
    term = subprocess.Popen(['kitty', '-o', 'allow_remote_control=socket-only', '--listen-on', sock,
                             '--class', 'cul-bench', '--title', title, 'cat'],
                            env=clean_env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    srv = subprocess.Popen([a.bin, 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                           stderr=subprocess.DEVNULL, text=True, bufsize=1)
    next_id = [0]

    def rpc(method, params=None, notify=False):
        msg = {'jsonrpc': '2.0', 'method': method}
        if params is not None:
            msg['params'] = params
        if not notify:
            next_id[0] += 1
            msg['id'] = next_id[0]
        srv.stdin.write(json.dumps(msg) + '\n')
        srv.stdin.flush()
        if notify:
            return None
        while True:
            line = srv.stdout.readline()
            if not line:
                raise RuntimeError('server exited')
            reply = json.loads(line)
            if reply.get('id') == next_id[0]:
                return reply

    def call(name, args):
        t0 = time.perf_counter()
        reply = rpc('tools/call', {'name': name, 'arguments': args})
        dt = time.perf_counter() - t0
        result = reply.get('result', {})
        size = sum(len(c.get('data', '') or c.get('text', '')) for c in result.get('content', []))
        return dt, bool(result.get('isError')), size

    try:
        rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                           'clientInfo': {'name': 'bench', 'version': '0'}})
        rpc('notifications/initialized', notify=True)
        wid = None
        for _ in range(50):
            time.sleep(0.2)
            reply = rpc('tools/call', {'name': 'list_windows', 'arguments': {}})
            windows = json.loads(reply['result']['content'][0]['text']).get('windows', [])
            wid = next((w['window_id'] for w in windows if w.get('title') == title), None)
            if wid:
                break
        if not wid:
            raise RuntimeError('bench window not found')

        typed = 'the quick brown fox 0123456789'

        def screen_text():
            got = subprocess.run(['kitty', '@', '--to', sock, 'get-text'], env=clean_env(),
                                 capture_output=True, text=True, timeout=10)
            return got.stdout.replace('\n', '')

        cases = [
            ('screenshot png', 'screenshot', {'window_id': wid}),
            ('screenshot jpeg q70', 'screenshot', {'window_id': wid, 'format': 'jpeg', 'quality': 70}),
            ('press_key a', 'press_key', {'window_id': wid, 'key': 'a'}),
            ('type_text 30ch', 'type_text', {'window_id': wid, 'text': typed}),
            ('click (rel)', 'click', {'window_id': wid, 'relative': True, 'x': 40, 'y': 40}),
            ('get_app_state no shot', 'get_app_state',
             {'window_id': wid, 'include_screenshot': False, 'max_nodes': 50}),
            ('list_windows', 'list_windows', {}),
        ]
        out = {}
        print(f"{'case':24} {'median':>7} {'min':>7} {'max':>7} {'bytes':>9} err")
        for label, name, args in cases:
            call(name, args)  # Warm-up, not counted.
            times, errors, size = [], 0, 0
            for _ in range(a.runs):
                dt, err, size = call(name, args)
                times.append(dt)
                errors += err
            out[label] = {'median': statistics.median(times), 'min': min(times),
                          'max': max(times), 'bytes': size, 'errors': errors}
            print(f"{label:24} {statistics.median(times):7.3f} {min(times):7.3f} "
                  f"{max(times):7.3f} {size:9} {errors}")
            if name == 'type_text':
                # Every run must land intact and in order: warm-up plus timed runs.
                time.sleep(0.3)
                intact = typed * (a.runs + 1) in screen_text()
                out[label]['intact'] = intact
                print(f"{'':24} typed text intact: {intact}")
        if a.json:
            with open(a.json, 'w') as f:
                json.dump(out, f, indent=2)
    finally:
        srv.kill()
        term.kill()


if __name__ == '__main__':
    main()
