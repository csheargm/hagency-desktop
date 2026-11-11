#!/usr/bin/env python3
"""Exercise offline room rows and reply previews through theme reloads."""
import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import uuid

from native_multi_account import App

PRIMARY_MODIFIER = 'cmd' if sys.platform == 'darwin' else 'ctrl'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=Path('target/fast/examples/room_list_performance'))
    args = parser.parse_args()
    root = Path('target/room-list-performance-validation') / uuid.uuid4().hex
    report = {'passed': False, 'runs': []}
    try:
        for mobile in (False, True):
            run = root / ('mobile' if mobile else 'desktop')
            profile = run / 'profile'
            profile.mkdir(parents=True, mode=0o700)
            with socket.socket() as probe:
                probe.bind(('127.0.0.1', 0))
                port = probe.getsockname()[1]
            app = App(run, port=port, auto_login=False)
            app.output.mkdir(parents=True)
            app.log = (app.output / 'native.log').open('w')
            env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile.resolve()),
                       RINX_DATA_DIR=str(profile.resolve()),
                       MAKEPAD_REMOTE=str(port), MAKEPAD_HIDE_WINDOWS='1', MAKEPAD_NO_FOCUS='1')
            env.pop('MAKEPAD_FOCUS', None)
            app.process = subprocess.Popen([str(args.binary.resolve()), *(['--mobile'] if mobile else [])],
                                           env=env, stdin=subprocess.DEVNULL,
                                           stdout=app.log, stderr=subprocess.STDOUT)
            try:
                for _ in range(120):
                    if app.process.poll() is not None:
                        raise RuntimeError(f'Room-list fixture exited; see {app.output}')
                    try:
                        if app.request('/s')['pid'] == app.process.pid:
                            break
                    except OSError:
                        pass
                    time.sleep(.1)
                app.wait_text('Room 00 version 0')
                app.click_id('text_input')
                app.request('/t', t='Unsent draft 中文', wait=1)
                app.wait_text('Unsent draft 中文')
                app.request('/m', k='scroll', x=200, y=300, dy=700, wait=1)
                time.sleep(.5)
                visible = lambda: [w.get('t', '') for w in app.snap()
                                   if w.get('i') == 'room_name' and w['r'][1] >= 0 and w['r'][1] < 700]
                before = visible()
                assert before and all('version 0' in name for name in before), before
                assert not any('Room 00 ' in name for name in before), before
                app.request('/event', data='fixture:update', wait=1)
                app.wait_text('version 1')
                app.request('/event', data='fixture:dark', wait=1)
                time.sleep(.5)
                app.wait_text('Unsent draft 中文')
                after = visible()
                assert after and all('version 1' in name for name in after), after
                app.request('/m', k='scroll', x=200, y=300, dy=-10000, wait=1)
                app.wait_text('Room 00 version 1')
                assert any('Latest message 00 version 1' in row['text'] for row in app.ocr())
                app.wait_text('Show more')
                app.click_id('reply_expand_button')
                app.wait_text('Show less')
                app.click_id('reply_collapse_button')
                app.wait_text('Show more')
                app.click_id('text_input')
                app.request('/k', c='KeyA', wait=1, **{PRIMARY_MODIFIER: 1})
                app.request('/t', t='Replacement', wait=1)
                app.wait_text('Replacement')
                app.request('/k', c='KeyZ', wait=1, **{PRIMARY_MODIFIER: 1})
                app.wait_text('Unsent draft 中文')
                report['runs'].append({'layout': 'mobile' if mobile else 'desktop',
                                       'scrolled_rows': before, 'updated_rows': after,
                                       'reply_expands_and_collapses_after_theme_reload': True,
                                       'composer_draft_and_undo_survive_theme_reload': True})
            finally:
                app.stop()
        report['passed'] = True
    finally:
        root.mkdir(parents=True, exist_ok=True)
        (root / 'report.json').write_text(json.dumps(report, indent=2))
        print(json.dumps({'report': str(root / 'report.json'), 'passed': report['passed']}), flush=True)


if __name__ == '__main__':
    main()
