#!/usr/bin/env python3
"""Prevent partial or mixed-source RC publication."""
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('release_assets', Path(__file__).with_name('release-assets.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseAssets(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        release.ROOT = self.root
        self.previous_sha = os.environ.get('GITHUB_SHA')
        os.environ['GITHUB_SHA'] = 'a'*40
        self.addCleanup(self.restore_sha)
        (self.root/'Cargo.toml').write_text('[package]\nversion="0.5.0-rc.1"\n[dependencies]\nhagency={path="crates/hagency-client/native/hagency"}\nmakepad-app-module={rev="makepad"}\n')
        self.out = self.root/'release-assets'
        self.out.mkdir()
        for platform, arch in [('macos','arm64'), ('windows','amd64')]:
            names = [f'hagency-desktop-{platform}-{arch}'+('.dmg' if platform == 'macos' else '.exe')]
            assets = {}
            for name in names:
                path = self.out/name
                path.write_bytes(name.encode())
                assets[name] = release.sha(path)
            report = dict(release.metadata(), platform=platform, arch=arch, assets=assets)
            (self.out/f'build-{platform}-{arch}.json').write_text(json.dumps(report))

    def restore_sha(self):
        if self.previous_sha is None: os.environ.pop('GITHUB_SHA', None)
        else: os.environ['GITHUB_SHA'] = self.previous_sha

    def test_rc_is_exactly_apple_silicon_desktop_and_windows(self):
        release.verify('macos-arm64-windows')
        sums = (self.out/'SHA256SUMS.txt').read_text().splitlines()
        self.assertEqual(len(sums), 2)
        self.assertFalse(any('macos-amd64' in row for row in sums))

    def test_changed_installer_is_rejected(self):
        (self.out/'hagency-desktop-windows-amd64.exe').write_bytes(b'wrong build')
        with self.assertRaisesRegex(AssertionError, 'Checksum mismatch'):
            release.verify('macos-arm64-windows')

    def test_library_source_is_the_desktop_commit(self):
        metadata = release.metadata()
        self.assertEqual(metadata['clientCommit'], metadata['sourceCommit'])
        self.assertEqual(metadata['clientPath'], 'crates/hagency-client')

    def test_engine_commit_is_the_pinned_checkout_that_is_built(self):
        # Without a pinned checkout, the manifest's git revision is what is built.
        self.assertEqual(release.metadata()['makepadCommit'], 'makepad')
        # With one, the manifest's revision is not built and must not be reported.
        (self.root/'.jit').mkdir()
        (self.root/'.jit'/'sources.lock').write_text(
            '# <directory under .sources>  <repository>  <commit>\n'
            'makepad   https://example.invalid/makepad.git   '+'e'*40+'\n')
        self.assertEqual(release.metadata()['makepadCommit'], 'e'*40)
        (self.root/'.jit'/'sources.lock').write_text('# nothing pinned\n')
        with self.assertRaisesRegex(SystemExit, 'does not pin makepad'):
            release.metadata()

    def test_external_client_revision_is_rejected(self):
        path = self.out/'build-windows-amd64.json'
        report = json.loads(path.read_text())
        report['clientCommit'] = 'c'*40
        path.write_text(json.dumps(report))
        with self.assertRaisesRegex(AssertionError, 'Mixed source versions'):
            release.verify('macos-arm64-windows')

    def test_different_source_commit_is_rejected(self):
        path = self.out/'build-windows-amd64.json'
        report = json.loads(path.read_text())
        report['sourceCommit'] = 'b'*40
        path.write_text(json.dumps(report))
        with self.assertRaisesRegex(AssertionError, 'Mixed source versions'):
            release.verify('macos-arm64-windows')

    def test_unrecorded_installer_is_rejected(self):
        path = self.out/'build-macos-arm64.json'
        report = json.loads(path.read_text())
        del report['assets']['hagency-desktop-macos-arm64.dmg']
        path.write_text(json.dumps(report))
        with self.assertRaises(AssertionError):
            release.verify('macos-arm64-windows')


if __name__ == '__main__':
    unittest.main()
