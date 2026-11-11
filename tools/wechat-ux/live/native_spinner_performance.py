#!/usr/bin/env python3
"""Measure an Agent-sized loading spinner in a hidden native chat timeline.

Build chat_timeline_selection with --profile fast --features palpo-instrument.
Hidden-window counts are regression checks, not desktop frame-rate benchmarks.
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
    root = Path('target/spinner-performance-validation') / uuid.uuid4().hex
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

        def process_cpu_seconds():
            try:
                raw = subprocess.check_output(['ps', '-p', str(app.process.pid), '-o', 'time='], text=True).strip()
                # The fixture runs only briefly, so this is MM:SS.ss or HH:MM:SS.ss.
                elapsed = 0.0
                for part in raw.split(':'):
                    elapsed = elapsed * 60.0 + float(part)
                return elapsed
            except (FileNotFoundError, subprocess.CalledProcessError, ValueError):
                return None

        def sample(name, duration):
            output.unlink(missing_ok=True)
            cpu_start = process_cpu_seconds()
            app.request('/event', data='perf:start')
            time.sleep(duration)
            app.request('/event', data='perf:stop')
            cpu_end = process_cpu_seconds()
            cpu_seconds = None if cpu_start is None or cpu_end is None else round(cpu_end - cpu_start, 2)
            for _ in range(100):
                if output.exists():
                    break
                time.sleep(.05)
            report['samples'][name] = dict(json.loads(output.read_text()), seconds=duration,
                                           process_cpu_seconds=cpu_seconds)

        def captures(prefix):
            rect = next(w['r'] for w in app.snap() if w['i'] == 'activity_spinner')
            window = app.request('/s')['w'][0]['sz']
            x, y, width, height = rect
            assert x >= 0 and y >= 0 and x + width <= window[0] and y + height <= window[1], rect
            crops = []
            for index in range(5):
                path = app.capture(f'{prefix}-{index}')
                with Image.open(path) as image:
                    sx, sy = image.width / window[0], image.height / window[1]
                    box = (round(x * sx), round(y * sy), round((x + width) * sx), round((y + height) * sy))
                    crops.append(hashlib.sha256(image.crop(box).tobytes()).hexdigest())
                time.sleep(.12)
            return len(set(crops))

        time.sleep(1)
        sample('idle', 2)
        app.request('/event', data='perf:spinner-show')
        time.sleep(.7)
        sample('active', 4)
        report['distinct_spinner_images'] = captures('active')
        app.request('/event', data='perf:typing-show')
        app.wait_text('Alice is typing')
        time.sleep(.7)
        sample('spinner_and_typing', 4)
        app.request('/event', data='perf:typing-hide')
        time.sleep(1)
        app.request('/event', data='perf:spinner-reapply')
        time.sleep(.7)
        sample('reapplied', 2)
        report['distinct_reapplied_images'] = captures('reapplied')
        for suffix in ('', '-blocked'):
            app.request('/event', data='perf:spinner-obscure' + suffix)
            time.sleep(1)
            sample('obscured' + suffix, 2)
            app.request('/event', data='perf:spinner-reveal')
            time.sleep(.7)
            sample('revealed' + suffix, 2)
        app.request('/event', data='perf:spinner-hide')
        time.sleep(1)
        sample('stopped', 2)

        assert report['distinct_spinner_images'] > 1, report
        assert report['distinct_reapplied_images'] > 1, report
        if args.assert_pacing:
            assert 10 < report['samples']['active']['frames_painted'] < 180, report
            assert report['samples']['active']['draw_events'] < 10, report
            assert 10 < report['samples']['spinner_and_typing']['frames_painted'] < 260, report
            assert report['samples']['spinner_and_typing']['draw_events'] < 10, report
            for name in ('reapplied', 'revealed', 'revealed-blocked'):
                assert report['samples'][name]['frames_painted'] > 5, report
            for name in ('idle', 'obscured', 'obscured-blocked', 'stopped'):
                assert report['samples'][name]['frames_painted'] < 20, report
                assert report['samples'][name]['timer_events'] < 3, report
        report['passed'] = True
    finally:
        (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(root / 'report.json'), **report}), flush=True)
        app.stop()


if __name__ == '__main__':
    main()
