#!/usr/bin/env python3
"""Live navigation benchmark: open a page in a new Zen tab, then close it.

classic  how an agent drives it with separate tools: look, act, look again
act      one `act` call: keys, URL, Enter, wait, then a delta check

Usage: nav_bench.py --window WINDOW_ID --url URL [--bin PATH] [--runs N]
"""
import argparse
import json
import statistics
import time

from workflow_bench import Client, image_tokens

LOAD_WAIT_MS = 4000
GAP = 4.9  # Session median of model thinking per round.


def title(c, wid):
    windows = json.loads(c.call('list_windows', {})['content'][0]['text'])['windows']
    return next((w['title'] for w in windows if w['window_id'] == wid), '')


def classic(c, wid, url):
    tokens = 0
    shot = c.call('screenshot', {'window_id': wid, 'format': 'jpeg', 'quality': 75})
    tokens += image_tokens(shot.get('content', []))
    c.call('press_key', {'window_id': wid, 'key': 'ctrl+t'})
    c.call('type_text', {'window_id': wid, 'text': url + '\n'})
    time.sleep(LOAD_WAIT_MS / 1000)
    shot = c.call('screenshot', {'window_id': wid, 'format': 'jpeg', 'quality': 75})
    tokens += image_tokens(shot.get('content', []))
    return {'rounds': 3, 'calls': 5, 'tokens': tokens}


def act(c, wid, url):
    steps = [{'key': {'key': 'ctrl+t'}}, {'type': {'text': url + '\n'}},
             {'wait': {'ms': LOAD_WAIT_MS}}]
    result = c.call('act', {'window_id': wid, 'steps': steps, 'expect': 'change'})
    summary = json.loads(result['content'][-1]['text'])
    return {'rounds': 1, 'calls': 1, 'tokens': image_tokens(result.get('content', [])),
            'expect_passed': summary.get('expect_passed'),
            'changed_fraction': (summary.get('changed_region') or {}).get('fraction')}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', default='computer-use-linux')
    ap.add_argument('--window', type=int, required=True)
    ap.add_argument('--url', required=True)
    ap.add_argument('--runs', type=int, default=2)
    a = ap.parse_args()

    c = Client(a.bin)
    try:
        print(f"{'mode':8} {'rounds':>6} {'calls':>5} {'tool s':>7} {'img tok':>7} {'+gaps s':>8}  landed on")
        for name, run in (('classic', classic), ('act', act)):
            times, last, landed = [], None, ''
            for _ in range(a.runs):
                t0 = time.perf_counter()
                last = run(c, a.window, a.url)
                times.append(time.perf_counter() - t0)
                landed = title(c, a.window)
                c.call('press_key', {'window_id': a.window, 'key': 'ctrl+w'})  # Close the test tab.
                time.sleep(1.0)
            tool = statistics.median(times)
            extra = {k: v for k, v in last.items() if k not in ('rounds', 'calls', 'tokens')}
            print(f"{name:8} {last['rounds']:6} {last['calls']:5} {tool:7.2f} {last['tokens']:7} "
                  f"{tool + last['rounds'] * GAP:8.2f}  {landed[:60]} {extra or ''}")
    finally:
        c.close()


if __name__ == '__main__':
    main()
