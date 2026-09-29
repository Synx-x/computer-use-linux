#!/usr/bin/env python3
"""Live in-app navigation benchmark: click through AI Studio's left nav.

Starts from a fresh load of the Usage screen and visits Spend, API Keys,
Projects and Rate Limit. AI Studio shows a billing banner on these screens that
moves the nav down by 50 px, so the coordinates below assume the banner.

classic   click, then a screenshot to confirm, for every screen
act       one `act` call per screen: click plus a delta check
landmark  one `act` call for the whole tour, clicking saved landmarks

Each mode ends back on Rate Limit. Window-relative coordinates come from a
screenshot of the Zen window at full size.

Usage: ui_bench.py --window WINDOW_ID [--bin PATH] [--runs N]
"""
import argparse
import json
import statistics
import time

from workflow_bench import Client, image_tokens

GAP = 4.9
SCREEN_WAIT_MS = 1500
NAV = {  # Window-relative centres of the nav links, with the banner shown.
    'Spend': (66, 382),
    'API Keys': (74, 200),
    'Projects': (72, 236),
    'Rate Limit': (76, 346),
}
START_URL = 'aistudio.google.com/usage'
TOUR = ['Spend', 'API Keys', 'Projects', 'Rate Limit']


def title(c, wid):
    windows = json.loads(c.call('list_windows', {})['content'][0]['text'])['windows']
    return next((w['title'] for w in windows if w['window_id'] == wid), '')


def go_home(c, wid):
    # A fresh page load gives every run the same starting layout.
    c.call('press_key', {'window_id': wid, 'key': 'ctrl+l'})
    c.call('type_text', {'window_id': wid, 'text': START_URL + '\n'})
    time.sleep(5.0)


def classic(c, wid):
    tokens, seen = 0, []
    for name in TOUR:
        x, y = NAV[name]
        c.call('click', {'window_id': wid, 'relative': True, 'x': x, 'y': y})
        time.sleep(SCREEN_WAIT_MS / 1000)
        shot = c.call('screenshot', {'window_id': wid, 'format': 'jpeg', 'quality': 75})
        tokens += image_tokens(shot.get('content', []))
        seen.append(title(c, wid).split(' | ')[0])
    return {'rounds': len(TOUR), 'calls': 2 * len(TOUR), 'tokens': tokens, 'seen': seen}


def act(c, wid):
    tokens, seen, ok = 0, [], True
    for name in TOUR:
        x, y = NAV[name]
        steps = [{'click': {'x': x, 'y': y}}, {'wait': {'ms': SCREEN_WAIT_MS}}]
        result = c.call('act', {'window_id': wid, 'steps': steps, 'expect': 'change'})
        tokens += image_tokens(result.get('content', []))
        ok = ok and json.loads(result['content'][-1]['text']).get('ok', False)
        seen.append(title(c, wid).split(' | ')[0])
    return {'rounds': len(TOUR), 'calls': len(TOUR), 'tokens': tokens, 'seen': seen, 'ok': ok}


def save_landmarks(c, wid):
    steps = [{'save_landmark': {'name': f'aistudio-{n}', 'x': x, 'y': y}} for n, (x, y) in NAV.items()]
    c.call('act', {'window_id': wid, 'steps': steps, 'observe': 'none'})


def landmark(c, wid):
    steps, seen = [], []
    for name in TOUR:
        steps += [{'click': {'landmark': f'aistudio-{name}'}}, {'wait': {'ms': SCREEN_WAIT_MS}}]
    result = c.call('act', {'window_id': wid, 'steps': steps, 'expect': 'change'})
    summary = json.loads(result['content'][-1]['text'])
    checks = [f"{ch['name'].split('-', 1)[1]}:{'pass' if ch['passed'] else 'FAIL ' + ch['reason']}"
              for ch in summary.get('landmarks', [])]
    return {'rounds': 1, 'calls': 1, 'tokens': image_tokens(result.get('content', [])),
            'seen': [title(c, wid).split(' | ')[0]], 'ok': summary.get('ok'), 'checks': checks}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--bin', default='computer-use-linux')
    ap.add_argument('--window', type=int, required=True)
    ap.add_argument('--runs', type=int, default=2)
    a = ap.parse_args()

    c = Client(a.bin)
    try:
        go_home(c, a.window)
        save_landmarks(c, a.window)  # One-time setup, outside the timed runs.
        print(f"{'mode':9} {'rounds':>6} {'calls':>5} {'tool s':>7} {'img tok':>7} {'+gaps s':>8}")
        for name, run in (('classic', classic), ('act', act), ('landmark', landmark)):
            times, last = [], None
            for _ in range(a.runs):
                t0 = time.perf_counter()
                last = run(c, a.window)
                times.append(time.perf_counter() - t0)
                go_home(c, a.window)
            tool = statistics.median(times)
            print(f"{name:9} {last['rounds']:6} {last['calls']:5} {tool:7.2f} {last['tokens']:7} "
                  f"{tool + last['rounds'] * GAP:8.2f}")
            print(f"{'':9} screens: {last['seen']} ok={last.get('ok', 'n/a')} {last.get('checks', '')}")
    finally:
        c.close()


if __name__ == '__main__':
    main()
