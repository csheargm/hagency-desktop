#!/usr/bin/env python3
"""Check animation pause/resume in real retained Dock tabs, without a visible window."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import uuid

from PIL import Image
from native_multi_account import App


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', type=Path, default=Path('target/fast/examples/spinner_dock_performance'))
    args = parser.parse_args()
    root = Path('target/spinner-dock-performance-validation') / uuid.uuid4().hex
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
               HAGENCY_PERF_OUTPUT=str(output.resolve()), RAYON_NUM_THREADS='1')
    env.pop('MAKEPAD_FOCUS', None)
    app.process = subprocess.Popen([str(args.binary.resolve())], env=env, stdin=subprocess.DEVNULL,
                                   stdout=app.log, stderr=subprocess.STDOUT)
    report = {'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              'headless': True, 'samples': {}}
    try:
        for _ in range(120):
            if app.process.poll() is not None:
                raise RuntimeError(f'Dock fixture exited; see {app.output}')
            try:
                if app.request('/s')['pid'] == app.process.pid:
                    break
            except OSError:
                pass
            time.sleep(.1)

        def sample(name):
            output.unlink(missing_ok=True)
            app.request('/event', data='perf:start')
            time.sleep(2)
            app.request('/event', data='perf:stop')
            for _ in range(100):
                if output.exists():
                    break
                time.sleep(.05)
            report['samples'][name] = json.loads(output.read_text())

        app.wait_text('Activity tab')
        time.sleep(1)
        sample('active')
        for index in range(3):
            if index == 2:
                app.request('/event', data='dock:block-timers')
            if index == 0:
                app.request('/event', data='dock:other')
            else:
                app.click_id('other_tab')
            app.wait_text('Other tab')
            time.sleep(1)
            sample(f'hidden_{index}')
            if index == 2:
                app.request('/event', data='dock:unblock-timers')
            if index == 0:
                app.request('/event', data='dock:activity')
            else:
                app.click_id('activity_tab')
            app.wait_text('Activity tab')
            time.sleep(1)
            sample(f'restored_{index}')

        def distinct_images(widget, name):
            rect = next(w['r'] for w in app.snap() if w['i'] == widget)
            window = app.request('/s')['w'][0]['sz']
            images = []
            for index in range(5):
                path = app.capture(f'{name}-{index}')
                with Image.open(path) as image:
                    x, y, width, height = rect
                    sx, sy = image.width / window[0], image.height / window[1]
                    box = (round(x * sx), round(y * sy), round((x + width) * sx), round((y + height) * sy))
                    images.append(hashlib.sha256(image.crop(box).tobytes()).hexdigest())
                time.sleep(.12)
            return len(set(images))

        report['distinct_restored_images'] = distinct_images('activity_spinner', 'spinner-restored')

        assert 5 < report['samples']['active']['frames_painted'] < 90, report
        for index in range(3):
            hidden = report['samples'][f'hidden_{index}']
            restored = report['samples'][f'restored_{index}']
            assert hidden['frames_painted'] < 10 and hidden['timer_events'] < 3, report
            assert 5 < restored['frames_painted'] < 90 and restored['timer_events'] > 5, report
            assert restored['draw_events'] < 10, report
        assert report['distinct_restored_images'] > 1, report

        app.request('/event', data='dock:use-dots')
        time.sleep(1)
        sample('dots_active')
        app.click_id('other_tab')
        app.wait_text('Other tab')
        time.sleep(1)
        sample('dots_hidden')
        app.click_id('activity_tab')
        app.wait_text('Activity tab')
        time.sleep(1)
        sample('dots_restored')
        report['distinct_restored_dots_images'] = distinct_images('bouncing_dots', 'dots-restored')
        active = report['samples']['dots_active']
        hidden = report['samples']['dots_hidden']
        restored = report['samples']['dots_restored']
        assert 5 < active['frames_painted'] < 90, report
        assert hidden['frames_painted'] < 10 and hidden['timer_events'] < 3, report
        assert 5 < restored['frames_painted'] < 90 and restored['timer_events'] > 5, report
        assert restored['draw_events'] < 10 and report['distinct_restored_dots_images'] > 1, report

        app.request('/event', data='dock:block-timers')
        app.click_id('other_tab')
        app.wait_text('Other tab')
        time.sleep(1)
        sample('dots_hidden_blocked')
        app.request('/event', data='dock:unblock-timers')
        app.click_id('activity_tab')
        app.wait_text('Activity tab')
        time.sleep(1)
        sample('dots_restored_blocked')
        report['distinct_restored_blocked_dots_images'] = distinct_images('bouncing_dots', 'dots-restored-blocked')
        hidden = report['samples']['dots_hidden_blocked']
        restored = report['samples']['dots_restored_blocked']
        assert hidden['frames_painted'] < 10 and hidden['timer_events'] < 3, report
        assert 5 < restored['frames_painted'] < 90 and restored['timer_events'] > 5, report
        assert restored['draw_events'] < 10 and report['distinct_restored_blocked_dots_images'] > 1, report
        report['passed'] = True
    finally:
        (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(root / 'report.json'), **report}), flush=True)
        app.stop()


if __name__ == '__main__':
    main()
