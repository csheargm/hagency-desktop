#!/usr/bin/env python3
"""Exercise overlapping timed/persistent notifications in a hidden native window."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import uuid

from PIL import Image, ImageChops
from native_multi_account import App


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=Path('target/fast/examples/theme_mvp'))
    args = parser.parse_args()
    root = Path('target/popup-lifecycle-validation') / uuid.uuid4().hex
    profile = root / 'profile'
    profile.mkdir(parents=True, mode=0o700)
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        port = probe.getsockname()[1]
    app = App(root, port=port, auto_login=False)
    app.output.mkdir(parents=True)
    app.log = (app.output / 'native.log').open('w')
    output = root / 'sample.json'
    env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile.resolve()),
               RINX_DATA_DIR=str(profile.resolve()), MAKEPAD_REMOTE=str(port),
               MAKEPAD_HIDE_WINDOWS='1', MAKEPAD_NO_FOCUS='1',
               HAGENCY_PERF_OUTPUT=str(output.resolve()))
    env.pop('MAKEPAD_FOCUS', None)
    app.process = subprocess.Popen([str(args.binary.resolve())], env=env, stdin=subprocess.DEVNULL,
                                   stdout=app.log, stderr=subprocess.STDOUT)
    report = {'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'headless': True, 'passed': False}
    try:
        for _ in range(120):
            if app.process.poll() is not None:
                raise RuntimeError(f'Theme fixture exited; see {app.output}')
            try:
                if app.request('/s')['pid'] == app.process.pid:
                    break
            except OSError:
                pass
            time.sleep(.1)
        app.wait_text('One theme, three native surfaces')
        started = time.monotonic()
        for event in ('theme:popup-first', 'theme:popup-second', 'theme:popup-static'):
            app.request('/event', data=event)
        app.wait_text('Second timed notification', pixels=True)
        rows = app.ocr()
        first = next(row for row in rows if 'First timed notification' in row['text'])
        second = next(row for row in rows if 'Second timed notification' in row['text'])
        assert any('Persistent theme notification' in row['text'] for row in rows), rows
        window = app.request('/s')['w'][0]['sz']
        before = app.capture('progress-before')
        time.sleep(1.3)
        after = app.capture('progress-after')

        # Inspect the pixels immediately below each label, where each progress
        # bar is drawn. Both bars must advance independently while overlapping.
        with Image.open(before).convert('RGB') as a, Image.open(after).convert('RGB') as b:
            changes = []
            for row in (first, second):
                _, y, _, height = row['box']
                top = (y + height) * a.height
                box = (int(.71 * a.width), int(top), int(.995 * a.width),
                       int(top + 35 * a.height / window[1]))
                changes.append(ImageChops.difference(a.crop(box), b.crop(box)).getbbox() is not None)
        assert all(changes), changes
        report['both_progress_bars_advanced'] = True

        # Reapplying the theme must retain the popups and the original deadlines.
        app.request('/event', data='theme:dark')
        time.sleep(.7)
        rows = app.ocr()
        assert all(any(label in row['text'] for row in rows) for label in
                   ('First timed notification', 'Second timed notification', 'Persistent theme notification')), rows
        themed = app.capture('after-theme-change')
        persistent = next(row for row in rows if 'Persistent theme notification' in row['text'])
        with Image.open(themed).convert('RGB') as image:
            _, y, _, height = persistent['box']
            text_bottom = (y + height) * image.height
            x = int(.82 * image.width)
            background = image.getpixel((x, round(text_bottom + 10 * image.height / window[1])))
            bar_position = image.getpixel((x, round(text_bottom + 22 * image.height / window[1])))
            assert sum(abs(a - b) for a, b in zip(background, bar_position)) < 15, (background, bar_position)
        report['theme_change_kept_all_notifications'] = True
        report['persistent_progress_stays_hidden_after_theme_change'] = True
        time.sleep(.6)
        themed_later = app.capture('after-theme-progress')
        with Image.open(themed).convert('RGB') as a, Image.open(themed_later).convert('RGB') as b:
            for label in ('First timed notification', 'Second timed notification'):
                row = next(row for row in rows if label in row['text'])
                _, y, _, height = row['box']
                top = (y + height) * a.height
                box = (int(.71 * a.width), int(top), int(.995 * a.width),
                       int(top + 35 * a.height / window[1]))
                assert ImageChops.difference(a.crop(box), b.crop(box)).getbbox() is not None, label
        report['progress_continues_after_theme_change'] = True

        deadline = started + 13
        while time.monotonic() < deadline:
            rows = app.ocr()
            if not any('First timed notification' in row['text'] for row in rows):
                break
            time.sleep(.25)
        else:
            raise AssertionError('First notification did not dismiss on its original timer')
        report['first_dismissed_after_seconds'] = round(time.monotonic() - started, 2)
        assert report['first_dismissed_after_seconds'] >= 6, report
        second = next(row for row in rows if 'Second timed notification' in row['text'])
        assert any('Persistent theme notification' in row['text'] for row in rows), rows

        # The close control is at the upper right of the second popup, above its
        # OCR label. Coordinates are in the remote window's logical units.
        _, y, _, _ = second['box']
        app.click(window[0] * .97, y * window[1] - 27)
        rows = app.ocr()
        assert not any('Second timed notification' in row['text'] for row in rows), rows
        assert any('Persistent theme notification' in row['text'] for row in rows), rows
        report['manual_close_left_persistent_notification'] = True

        time.sleep(.5)
        app.request('/event', data='perf:start')
        time.sleep(2)
        app.request('/event', data='perf:stop')
        for _ in range(100):
            if output.exists():
                break
            time.sleep(.05)
        report['idle_frames_after_close'] = json.loads(output.read_text())['frames_painted']
        assert report['idle_frames_after_close'] < 20, report

        app.request('/event', data='theme:popup-second')
        app.wait_text('Second timed notification', pixels=True)
        app.request('/event', data='theme:clear-popups')
        time.sleep(.7)
        rows = app.ocr()
        assert not any('notification' in row['text'] for row in rows), rows
        output.unlink(missing_ok=True)
        app.request('/event', data='perf:start')
        time.sleep(2)
        app.request('/event', data='perf:stop')
        for _ in range(100):
            if output.exists():
                break
            time.sleep(.05)
        report['idle_frames_after_logout'] = json.loads(output.read_text())['frames_painted']
        assert report['idle_frames_after_logout'] < 20, report
        report['passed'] = True
    finally:
        (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(root / 'report.json'), **report}), flush=True)
        app.stop()


if __name__ == '__main__':
    main()
