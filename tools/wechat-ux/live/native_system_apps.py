#!/usr/bin/env python3
"""Offline native catalog/authorization check in isolated, hidden windows."""
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import uuid
from native_probe import NativeApp


def main():
    root = Path('target/deployment-validation/system-apps-native') / uuid.uuid4().hex
    root.mkdir(parents=True, mode=0o700)
    report = {'passed': False, 'checks': []}
    for hosted in (False, True):
        run = root / ('hosted' if hosted else 'standalone')
        profile = run / 'profile'
        profile.mkdir(parents=True, mode=0o700)
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        app = NativeApp(run, port=port, auto_login=False)
        app.output.mkdir(parents=True, mode=0o700)
        app.log = (app.output / 'native.log').open('w')
        env = dict(os.environ, HAGENCY_DESKTOP_DATA_DIR=str(profile.resolve()), MAKEPAD_HIDE_WINDOWS='1', MAKEPAD_NO_FOCUS='1', MAKEPAD_REMOTE=str(port))
        env.pop('MAKEPAD_FOCUS', None)
        args = [str(Path('target/fast/examples/system_app_catalog').resolve())]
        if hosted:
            args.append('--hosted')
        app.process = subprocess.Popen(args, env=env, stdout=app.log, stderr=subprocess.STDOUT)
        try:
            for _ in range(120):
                if app.process.poll() is not None:
                    raise RuntimeError('Catalog harness exited; see native.log')
                try:
                    status = app.request('/s')
                    assert status['pid'] == app.process.pid
                    if status['w']:
                        break
                except OSError:
                    pass
                time.sleep(.25)
            app.wait_text('Built-in apps')
            app.wait_text('Article editor')
            # A hidden Splash must not reserve half the catalog's layout.
            button = next(w for w in app.snap() if w['i'] == 'import_app')
            height = app.request('/s')['w'][0]['sz'][1]
            assert button['r'][1] > height * .75, button
            app.capture('catalog')
            app.click_id('import_app')
            app.wait_text('Review bundle')
            fields = {w['i'] for w in app.snap()}
            assert ('local_key' in fields) == (not hosted), fields
            app.click_id('close')
            app.wait_text('Built-in apps')
            app.click_id('launch')
            app.wait_text('Article studio')
            app.click_id('article_continue')
            app.wait_text('Sign in to Rinx to authorize this app.')
            assert 'article_allow' not in {w['i'] for w in app.snap()}
            app.capture('native-editor-login-required')
            app.click_id('article_close')
            app.wait_text('Built-in apps')
            report['checks'].append({'hosted': hosted, 'catalog_to_native_editor': True, 'requires_account_and_consent': True, 'provider_form_matches_deployment': True, 'evidence': str(app.output)})
        finally:
            app.stop()
    report['passed'] = True
    (root / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(dict(report, evidence=str(root))), flush=True)


if __name__ == '__main__':
    main()
