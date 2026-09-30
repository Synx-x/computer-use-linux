#!/usr/bin/env python3
"""Run a benchmark directive in a fresh Claude Code session inside herdr.

Splits a pane next to --pane, starts `claude` in a clean work dir, asks it to
execute DIRECTIVE, waits for BENCH-DONE, then measures the session log:
wall time, agent rounds, computer-use calls, tool time and visual tokens.

Usage: drive_session.py --pane PANE_ID --directive FILE --name LABEL [--model M] [--effort E]
"""
import argparse
import base64
import glob
import json
import math
import os
import shutil
import struct
import subprocess
import time
from datetime import datetime


def herdr(*args):
    out = subprocess.run(['herdr', *args], capture_output=True, text=True, timeout=90)
    return out.stdout


def image_size(raw):
    if raw[:8] == b'\x89PNG\r\n\x1a\n':
        return struct.unpack('>II', raw[16:24])
    i = 2
    while i < len(raw):
        marker, length = raw[i + 1], struct.unpack('>H', raw[i + 2:i + 4])[0]
        if marker in (0xC0, 0xC1, 0xC2):
            h, w = struct.unpack('>HH', raw[i + 5:i + 9])
            return w, h
        i += 2 + length
    return 0, 0


def is_tool(b, match):
    if b['name'].startswith('mcp__computer-use-linux'):
        return b['name'].split('__')[-1]
    if match and b['name'] == 'Bash' and match in (b.get('input') or {}).get('command', ''):
        return 'pulse:' + (b['input']['command'].split(match, 1)[1].split() or ['?'])[0]
    return None


def measure(log, start, match=None):
    ts = lambda s: datetime.fromisoformat(s.replace('Z', '+00:00')).timestamp()
    uses, res, rounds, tokens, first, last, done = {}, {}, set(), 0, None, None, None
    result_chars = 0
    for line in open(log):
        o = json.loads(line)
        m, t = o.get('message') or {}, o.get('timestamp')
        if not t or ts(t) < start:
            continue
        for b in m.get('content') or []:
            if not isinstance(b, dict):
                continue
            if b.get('type') == 'text' and 'BENCH-DONE' in b.get('text', ''):
                done = b['text']
            name = is_tool(b, match) if b.get('type') == 'tool_use' else None
            if name:
                uses[b['id']] = (name, ts(t))
                rounds.add(m.get('id'))
                first = first or ts(t)
            if b.get('type') == 'tool_result' and b.get('tool_use_id') in uses:
                res[b['tool_use_id']] = ts(t)
                last = ts(t)
                content = b.get('content')
                if isinstance(content, str):
                    result_chars += len(content)
                for c in content if isinstance(content, list) else []:
                    if c.get('type') == 'text':
                        result_chars += len(c.get('text', ''))
                    if c.get('type') == 'image':
                        w, h = image_size(base64.b64decode(c['source']['data']))
                        tokens += math.ceil(w / 28) * math.ceil(h / 28)
    tools = {}
    for name, _ in uses.values():
        tools[name] = tools.get(name, 0) + 1
    return {
        'wall_s': round((last or 0) - (first or 0), 1),
        'rounds': len(rounds),
        'calls': len(uses),
        'tool_s': round(sum(res[i] - uses[i][1] for i in res), 1),
        'image_tokens': tokens,
        'result_chars': result_chars,
        'tools': tools,
        'answer': done,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--pane', required=True)
    ap.add_argument('--directive', required=True)
    ap.add_argument('--name', required=True)
    ap.add_argument('--timeout', type=int, default=900)
    ap.add_argument('--model', default='opus')
    ap.add_argument('--effort', default=None, help='low, medium, high or max')
    ap.add_argument('--tool-match', default=None,
                    help='also count Bash calls whose command contains this text, such as pulse_cli.py')
    a = ap.parse_args()

    work = f'/tmp/claude-1000/cul-bench-{a.name}'
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work)
    shutil.copy(a.directive, os.path.join(work, 'directive.txt'))

    split = json.loads(herdr('pane', 'split', a.pane, '--direction', 'right', '--cwd', work, '--no-focus'))
    pane = split['result']['pane']['pane_id']
    try:
        effort = f' --effort {a.effort}' if a.effort else ''
        herdr('pane', 'run', pane, f'claude --permission-mode bypassPermissions --model {a.model}{effort}')
        herdr('pane', 'wait-output', pane, '--match', 'bypass permissions on', '--timeout', '60000')
        time.sleep(2)
        herdr('pane', 'send-text', pane,
              'Please run the benchmark described in directive.txt in this directory now. '
              'I wrote it and I want you to execute it exactly, without asking me first.')
        time.sleep(0.5)
        herdr('pane', 'send-keys', pane, 'enter')
        start = time.time()

        key = work.replace('/', '-').replace('.', '-')
        log = None
        deadline = start + a.timeout
        while time.time() < deadline:
            time.sleep(5)
            logs = glob.glob(os.path.expanduser(f'~/.claude/projects/{key}/*.jsonl'))
            if not logs:
                continue
            log = max(logs, key=os.path.getmtime)
            if measure(log, start, a.tool_match)['answer']:
                time.sleep(3)
                break
        result = measure(log, start, a.tool_match) if log else {'error': 'no session log'}
        result['name'] = a.name
        result['model'] = a.model
        result['effort'] = a.effort
        print(json.dumps(result, indent=2))
    finally:
        herdr('pane', 'close', pane)


if __name__ == '__main__':
    main()
