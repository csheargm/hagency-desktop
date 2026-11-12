#!/usr/bin/env python3
"""Install/relocate a packaged Desktop and render sign-in with fresh app data."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
from release_image_check import image_readiness


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--package', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--source-root', type=Path, default=Path(__file__).resolve().parents[1],
                    help='Source checkout that produced the installer (for its kernel pin)')
parser.add_argument('--remote-tool', type=Path,
                    help="The engine's makepad-remote tool; built from the source checkout when not given")
args = parser.parse_args()
package, output = args.package.resolve(), args.output.resolve()
output.mkdir(parents=True, exist_ok=True)


def remote_tool():
    """The engine's command-line client for a running app's remote endpoint.

    The app no longer serves HTTP on a port: it opens a same-user local endpoint,
    and this tool, built from the same pinned engine as the app, is how a check
    reads its semantic tree and its frames.
    """
    if args.remote_tool:
        return args.remote_tool.resolve()
    subprocess.run(['cargo', 'build', '--locked', '--release', '-p', 'makepad-remote',
                    '--bin', 'makepad-remote'], cwd=args.source_root, check=True, timeout=1800)
    target = Path(os.environ.get('CARGO_TARGET_DIR') or args.source_root/'target')
    return target/'release'/('makepad-remote.exe' if sys.platform == 'win32' else 'makepad-remote')


class RemoteNotReady(Exception):
    pass


# Before the app starts, so that the build takes nothing from its readiness deadline.
tool = remote_tool()
assert tool.is_file(), f'No makepad-remote tool at {tool}'

with tempfile.TemporaryDirectory(prefix='hagency-release-smoke-') as temporary:
    temporary = Path(temporary)
    if sys.platform == 'darwin':
        if package.suffix.lower() == '.dmg':
            mount = temporary/'image'
            mount.mkdir()
            # cargo-packager removes its intermediate .app after creating a
            # DMG. Exercise the distributable itself, including its contents.
            # The EULA is our own bundled application license.
            subprocess.run(['hdiutil', 'attach', '-readonly', '-nobrowse',
                            '-mountpoint', str(mount), str(package)],
                           input='Y\n', text=True, check=True, timeout=120)
            try:
                candidates = list(mount.glob('*.app'))
                assert len(candidates) == 1, 'Disk image must contain one Desktop app'
                app = temporary/candidates[0].name
                shutil.copytree(candidates[0], app)
            finally:
                subprocess.run(['hdiutil', 'detach', str(mount)], check=True, timeout=30)
        else:
            app = temporary/package.name
            shutil.copytree(package, app)
        binary = app/'Contents/MacOS/rinx'
        kernel = app/'Contents/MacOS/octos'
        subprocess.run(['codesign','--verify','--deep','--strict',str(app)], check=True)
    elif sys.platform == 'win32':
        install = temporary/'installed'
        subprocess.run([str(package), '/S', '/D='+str(install)], timeout=120, check=True)
        candidates = list(install.rglob('rinx.exe'))
        assert len(candidates) == 1, 'Installer did not install one Desktop executable'
        binary = candidates[0]
        kernel = binary.with_name('octos.exe')
        version_info = json.loads(subprocess.check_output(
            ['powershell', '-NoProfile', '-NonInteractive', '-Command',
             '$v=(Get-Item -LiteralPath $env:HAGENCY_SMOKE_BINARY).VersionInfo; '
             '@{product=$v.ProductName;company=$v.CompanyName}|ConvertTo-Json -Compress'],
            env=dict(os.environ, HAGENCY_SMOKE_BINARY=str(binary)), text=True, timeout=20))
        assert version_info == {'product': 'Hagency Desktop', 'company': 'Hagency'}, version_info
    else:
        raise SystemExit('This smoke check covers macOS and Windows installers')
    kernel_version = subprocess.check_output([str(kernel),'--version'], text=True, timeout=20).strip()
    pin = json.loads((args.source_root/'packaging/octos.lock.json').read_text())['revision']
    revision = re.search(r'\(([0-9a-f]{7,40}) ', kernel_version)
    assert revision and pin.startswith(revision[1]), kernel_version
    # Launched for a check: the app prints its endpoint line once it listens.
    env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(temporary/'profile'),
               MAKEPAD_REMOTE='1')
    # Exercise the normal visible Windows launch path on the CI desktop. A
    # permanently hidden HWND exercises compositor occlusion instead of the
    # installer behavior a person sees. macOS supports offscreen Metal draws.
    env.pop('MAKEPAD_HIDE_WINDOWS', None)
    if sys.platform == 'darwin':
        env['MAKEPAD_HIDE_WINDOWS'] = '1'
    log = output/'startup.log'
    with log.open('w') as stream:
        process = subprocess.Popen([str(binary)], cwd=temporary, env=env, stdout=stream, stderr=stream)
    try:
        deadline = time.monotonic()+90
        def startup_log():
            return log.read_text(errors='replace')[-3000:]
        pid = None
        while time.monotonic() < deadline:
            assert process.poll() is None, startup_log()
            match = re.search(r'\[makepad-remote\] pid=(\d+) ', log.read_text(errors='replace'))
            if match: pid = match[1]; break
            time.sleep(.2)
        assert pid, 'Packaged app did not open its remote endpoint: ' + startup_log()
        def remote(*command):
            try:
                result = subprocess.run([str(tool), 'app', pid, *command], capture_output=True,
                                        encoding='utf-8', errors='replace', timeout=30)
            except subprocess.TimeoutExpired as error:
                raise RemoteNotReady(f'{command[0]} did not answer in 30 s') from error
            if result.returncode != 0:
                raise RemoteNotReady(f'{command[0]}: {(result.stdout + result.stderr)[-1000:]}')
            try:
                return json.loads(result.stdout)
            except ValueError as error:
                raise RemoteNotReady(f'{command[0]} answered {result.stdout[-1000:]!r}') from error
        frames, ready_frames, not_ready = 0, 0, ''
        while time.monotonic() < deadline and ready_frames < 3:
            assert process.poll() is None, startup_log()
            try:
                snapshot = remote('ui', 'json=1')
                # Written where the last capture is kept even on failure, so an
                # empty GPU frame cannot be hidden by a passing semantic snapshot.
                image = remote('grab', 'out='+str(output/'sign-in.png'))
                (output/'snapshot.json').write_text(json.dumps(snapshot, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
                (output/'frame.json').write_text(json.dumps(image, indent=2)+'\n')
                frames += 1
                texts = [node.get('name') or node.get('text') or ''
                         for tree in snapshot.get('trees', []) for node in tree.get('nodes', [])
                         if len(node.get('bounds', [])) == 4 and node['bounds'][2]>0 and node['bounds'][3]>0]
                sign_in = any('Sign in to Hagency' in text or '登录 Hagency' in text for text in texts)
                pixels = image_readiness(image['png'])
                ready_frames = ready_frames + 1 if sign_in and pixels['nonBlank'] else 0
            except RemoteNotReady as error:
                # The endpoint line is printed just before the first frame is
                # drawn. Retry within the readiness deadline; the next loop
                # still fails immediately if the application has exited.
                not_ready = str(error)
            time.sleep(.4)
        if sys.platform == 'win32':
            # Save independent OS evidence, including whether the ordinary
            # launch actually made its HWND visible. Backend grabs alone can
            # otherwise confuse readback failures with a blank application.
            import ctypes
            from ctypes import wintypes
            from PIL import ImageGrab
            user32 = ctypes.windll.user32
            windows = []
            callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
            user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
            user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
            user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
            user32.IsWindowVisible.argtypes = [wintypes.HWND]
            def inspect_window(hwnd, _):
                pid = wintypes.DWORD()
                user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
                if pid.value == process.pid:
                    title, rect = ctypes.create_unicode_buffer(1024), wintypes.RECT()
                    user32.GetWindowTextW(hwnd, title, len(title))
                    user32.GetWindowRect(hwnd, ctypes.byref(rect))
                    windows.append(dict(title=title.value, visible=bool(user32.IsWindowVisible(hwnd)),
                                        rect=[rect.left, rect.top, rect.right, rect.bottom]))
                return True
            user32.EnumWindows(callback_type(inspect_window), 0)
            (output/'native-windows.json').write_text(json.dumps(windows, indent=2)+'\n')
            ImageGrab.grab(all_screens=True).save(output/'desktop.png')
        assert ready_frames == 3, ('Packaged app did not render sign-in pixels before the deadline'
                                   + (f' (last remote failure: {not_ready})' if not_ready else ''))
        errors = [line for line in log.read_text(errors='replace').splitlines()
                  if re.search(r'panicked at|Bundle[d]? file open failed|failed to load.*(font|resource)',line,re.I)]
        assert not errors, errors
        (output/'report.json').write_text(json.dumps(dict(passed=True, platform=sys.platform,
            package=str(package), isolatedProfile=True, kernelVersion=kernel_version,
            signInRendered=True, nonBlankScreenshot=pixels, framesChecked=frames,
            rendererOverride=False,
            windowHidden=sys.platform == 'darwin',
            resourceErrors=errors), indent=2)+'\n')
        print('Packaged Desktop started and rendered sign-in with bundled resources')
    finally:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(timeout=5)
