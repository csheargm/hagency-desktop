#!/usr/bin/env python3
"""Measure and exercise the typing notice in a hidden native chat timeline.

Build chat_timeline_selection with --profile fast --features palpo-instrument.
Hidden-window frame counts are regression checks, not desktop frame-rate benchmarks.
"""
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
    parser.add_argument('--binary', type=Path, default=Path('target/fast/examples/chat_timeline_selection'))
    parser.add_argument('--assert-pacing', action='store_true')
    args = parser.parse_args()
    root = Path('target/typing-performance-validation') / uuid.uuid4().hex
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
                raise RuntimeError(f'Timeline fixture exited; see {app.output}')
            try:
                if app.request('/s')['pid'] == app.process.pid:
                    break
            except OSError:
                pass
            time.sleep(.1)
        app.wait_text('Ready')

        def sample(name, duration):
            output.unlink(missing_ok=True)
            app.request('/event', data='perf:start')
            time.sleep(duration)
            app.request('/event', data='perf:stop')
            for _ in range(100):
                if output.exists():
                    break
                time.sleep(.05)
            report['samples'][name] = dict(json.loads(output.read_text()), seconds=duration)

        time.sleep(1)
        sample('idle', 2)
        app.request('/event', data='perf:typing-show')
        app.wait_text('Alice is typing')
        time.sleep(.7)  # Exclude the notice's slide-in animation.
        sample('typing', 4)

        # Confirm that the dots actually move, not just that frames are painted.
        rect = next(w['r'] for w in app.snap() if w['i'] == 'bouncing_dots')
        window = app.request('/s')['w'][0]['sz']
        crops = []
        for index in range(5):
            path = app.capture(f'dots-{index}')
            with Image.open(path) as image:
                sx, sy = image.width / window[0], image.height / window[1]
                x, y, width, height = rect
                box = (round(x * sx), round(y * sy), round((x + width) * sx), round((y + height) * sy))
                crops.append(hashlib.sha256(image.crop(box).tobytes()).hexdigest())
            time.sleep(.12)
        report['distinct_dot_images'] = len(set(crops))

        app.request('/event', data='perf:typing-hide')
        time.sleep(1)
        sample('stopped', 2)
        app.request('/event', data='perf:typing-show')
        app.wait_text('Alice is typing')
        time.sleep(.7)
        sample('restarted', 2)
        app.request('/event', data='perf:typing-reapply')
        time.sleep(.7)
        app.wait_text('Alice is typing')
        sample('reapplied', 2)
        # A retained but hidden tab can still receive timer events. A covered
        # panel can instead stop forwarding them entirely. Both must resume.
        for suffix in ('', '-blocked'):
            app.request('/event', data='perf:typing-obscure' + suffix)
            time.sleep(1)
            sample('obscured' + suffix, 2)
            app.request('/event', data='perf:typing-reveal')
            app.wait_text('Alice is typing')
            time.sleep(.7)
            sample('revealed' + suffix, 2)
        app.request('/event', data='perf:typing-switch')
        time.sleep(1)
        sample('room_switch', 2)

        assert report['distinct_dot_images'] > 1, report
        if args.assert_pacing:
            assert report['samples']['typing']['frames_painted'] < 180, report
            assert report['samples']['typing']['frames_painted'] > 10, report
            assert report['samples']['typing']['draw_events'] < 10, report
            assert report['samples']['restarted']['frames_painted'] > 5, report
            assert report['samples']['reapplied']['frames_painted'] > 5, report
            assert report['samples']['revealed']['frames_painted'] > 5, report
            assert report['samples']['revealed-blocked']['frames_painted'] > 5, report
            for name in ('idle', 'stopped', 'obscured', 'obscured-blocked', 'room_switch'):
                assert report['samples'][name]['frames_painted'] < 20, report
                assert report['samples'][name]['timer_events'] < 3, report
        report['passed'] = True
    finally:
        (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(root / 'report.json'), **report}), flush=True)
        app.stop()


if __name__ == '__main__':
    main()
