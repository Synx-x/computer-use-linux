#!/usr/bin/env python3
"""Replay a real agent workflow and measure the whole loop, not single calls.

The workflow is the userscript install sequence from a live session: 7 key
presses, one 46-character URL, 1 click and 3 checks, in 3 agent rounds. It runs
against a throwaway kitty window running `cat`, targeted by window_id only.

Modes:
  classic   individual tools plus a screenshot per round (how the session ran)
  act       one `act` call per round: the steps plus a delta check
  landmark  one `act` call for the whole flow, clicks via validated landmarks

Model thinking time cannot be replayed, so each round adds the session's
measured median gap between calls (--gap, default 4.9 s). Image cost uses
Claude's visual-token rule: ceil(w/28) * ceil(h/28).

Usage: workflow_bench.py [--bin PATH] [--modes classic,act,landmark] [--runs N]
"""
import argparse
import base64
import json
import math
import os
import statistics
import struct
import subprocess
import time

SCRUB_PREFIXES = ('CLAUDE_CODE', 'HERDR')
URL = 'http://127.0.0.1:8765/ytm-queue-filter.user.js'  # 46 characters


def clean_env():
    return {k: v for k, v in os.environ.items() if not k.startswith(SCRUB_PREFIXES)}


def image_tokens(content):
    """Visual tokens for every image block in a tool result."""
    total = 0
    for block in content:
        if block.get('type') != 'image':
            continue
        raw = base64.b64decode(block['data'])
        w, h = image_size(raw)
        total += math.ceil(w / 28) * math.ceil(h / 28)
    return total


def image_size(raw):
    if raw[:8] == b'\x89PNG\r\n\x1a\n':
        return struct.unpack('>II', raw[16:24])
    i = 2  # JPEG: walk segments to the first SOF marker.
    while i < len(raw):
        marker, length = raw[i + 1], struct.unpack('>H', raw[i + 2:i + 4])[0]
        if marker in (0xC0, 0xC1, 0xC2):
            h, w = struct.unpack('>HH', raw[i + 5:i + 9])
            return w, h
        i += 2 + length
    return 0, 0


class Client:
    def __init__(self, binary):
        self.proc = subprocess.Popen([binary, 'mcp'], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.next_id = 0
        self.rpc('initialize', {'protocolVersion': '2025-06-18', 'capabilities': {},
                                'clientInfo': {'name': 'workflow-bench', 'version': '0'}})
        self.notify('notifications/initialized')

    def notify(self, method):
        self.proc.stdin.write(json.dumps({'jsonrpc': '2.0', 'method': method}) + '\n')
        self.proc.stdin.flush()

    def rpc(self, method, params):
        self.next_id += 1
        msg = {'jsonrpc': '2.0', 'id': self.next_id, 'method': method, 'params': params}
        self.proc.stdin.write(json.dumps(msg) + '\n')
        self.proc.stdin.flush()
        while True:
            reply = json.loads(self.proc.stdout.readline())
            if reply.get('id') == self.next_id:
                return reply

    def call(self, name, args):
        reply = self.rpc('tools/call', {'name': name, 'arguments': args})
        return reply.get('result', {})

    def close(self):
        self.proc.kill()


def run_classic(c, wid):
    rounds = [
        [('press_key', {'key': 'a'}), ('type_text', {'text': URL}), ('press_key', {'key': 'b'})],
        [('press_key', {'key': 'c'}), ('press_key', {'key': 'd'}),
         ('click', {'relative': True, 'x': 40, 'y': 40}), ('press_key', {'key': 'e'})],
        [('press_key', {'key': 'f'}), ('press_key', {'key': 'g'})],
    ]
    calls, tokens = 0, 0
    for steps in rounds:
        for name, args in steps:
            c.call(name, {'window_id': wid, **args})
            calls += 1
        shot = c.call('screenshot', {'window_id': wid, 'format': 'jpeg', 'quality': 75})
        calls += 1
        tokens += image_tokens(shot.get('content', []))
    return {'rounds': len(rounds), 'calls': calls, 'tokens': tokens, 'ok': True}


def act_steps():
    return [
        [{'key': {'key': 'a'}}, {'type': {'text': URL}}, {'key': {'key': 'b'}}],
        [{'key': {'key': 'c'}}, {'key': {'key': 'd'}}, {'click': {'x': 40, 'y': 40}},
         {'key': {'key': 'e'}}],
        [{'key': {'key': 'f'}}, {'key': {'key': 'g'}}],
    ]


def run_act(c, wid):
    calls, tokens, ok = 0, 0, True
    for steps in act_steps():
        result = c.call('act', {'window_id': wid, 'steps': steps, 'expect': 'change'})
        calls += 1
        tokens += image_tokens(result.get('content', []))
        summary = json.loads(result['content'][-1]['text'])
        ok = ok and summary.get('ok', False)
    return {'rounds': 3, 'calls': calls, 'tokens': tokens, 'ok': ok}


def run_landmark(c, wid):
    # One-time setup, outside the timed flow: remember the click target.
    c.call('act', {'window_id': wid, 'observe': 'none',
                   'steps': [{'save_landmark': {'name': 'bench-target', 'x': 400, 'y': 300}}]})
    steps = [s for batch in act_steps() for s in batch]
    steps = [{'click': {'landmark': 'bench-target'}} if 'click' in s else s for s in steps]
    t0 = time.perf_counter()
    result = c.call('act', {'window_id': wid, 'steps': steps, 'expect': 'change'})
    summary = json.loads(result['content'][-1]['text'])
    return {'rounds': 1, 'calls': 1, 'tokens': image_tokens(result.get('content', [])),
            'ok': summary.get('ok', False), 'landmarks': summary.get('landmarks'),
            '_t0': t0}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', default='computer-use-linux')
    ap.add_argument('--modes', default='classic,act,landmark')
    ap.add_argument('--runs', type=int, default=3)
    ap.add_argument('--gap', type=float, default=4.9)
    ap.add_argument('--json')
    a = ap.parse_args()

    title = f'cul-flow-{os.getpid()}'
    term = subprocess.Popen(['kitty', '--class', 'cul-bench', '--title', title, 'cat'],
                            env=clean_env(), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    c = Client(a.bin)
    out = {}
    try:
        wid = None
        for _ in range(50):
            time.sleep(0.2)
            listing = json.loads(c.call('list_windows', {})['content'][0]['text'])
            wid = next((w['window_id'] for w in listing['windows'] if w.get('title') == title), None)
            if wid:
                break
        if not wid:
            raise RuntimeError('bench window not found')

        runners = {'classic': run_classic, 'act': run_act, 'landmark': run_landmark}
        print(f"{'mode':9} {'rounds':>6} {'calls':>5} {'tool s':>7} {'img tok':>7} "
              f"{'+gaps s':>8} {'ok':>3}")
        for mode in a.modes.split(','):
            times, last = [], None
            for _ in range(a.runs):
                t0 = time.perf_counter()
                last = runners[mode](c, wid)
                t0 = last.pop('_t0', t0)
                times.append(time.perf_counter() - t0)
            tool = statistics.median(times)
            total = tool + last['rounds'] * a.gap
            out[mode] = {**last, 'tool_s': tool, 'total_with_gaps_s': total}
            print(f"{mode:9} {last['rounds']:6} {last['calls']:5} {tool:7.2f} {last['tokens']:7} "
                  f"{total:8.2f} {str(last['ok']):>3}")
            if last.get('landmarks'):
                print(f"{'':9} landmark checks: {last['landmarks']}")
        if a.json:
            with open(a.json, 'w') as f:
                json.dump(out, f, indent=2)
    finally:
        c.close()
        term.kill()


if __name__ == '__main__':
    main()
