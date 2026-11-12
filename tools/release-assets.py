#!/usr/bin/env python3
"""Plan and verify complete releases from one source commit."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    'macos-arm64': dict(runner='macos-15', platform='macos', arch='arm64', formats='dmg'),
    'windows-amd64': dict(runner='windows-2022', platform='windows', arch='amd64', formats='nsis'),
    'macos-amd64': dict(runner='macos-15-intel', platform='macos', arch='amd64', formats='dmg'),
    'linux-amd64': dict(runner='ubuntu-22.04', platform='linux', arch='amd64', formats='deb,appimage'),
    'linux-arm64': dict(runner='ubuntu-22.04-arm', platform='linux', arch='arm64', formats='deb,appimage'),
}
EXTENSIONS = {'macos': ['.dmg'], 'windows': ['.exe'], 'linux': ['.deb', '.AppImage']}


def targets(scope):
    keys = ['macos-arm64', 'windows-amd64'] if scope == 'macos-arm64-windows' else list(TARGETS)
    return [TARGETS[key] for key in keys]


def expected_assets(scope):
    names = []
    for target in targets(scope):
        platform, arch = target['platform'], target['arch']
        names.extend(f'hagency-desktop-{platform}-{arch}{ext}' for ext in EXTENSIONS[platform])
    return names


def makepad_commit(manifest):
    """The engine commit the installers are built from.

    The engine is built from the checkout that `.jit/sources.lock` pins under
    `.sources/`, which Cargo.toml reaches by path. The git revision that the
    manifest still names for the optional app module is not what is built.
    """
    lock = ROOT/'.jit'/'sources.lock'
    if lock.is_file():
        for line in lock.read_text().splitlines():
            fields = line.split()
            if len(fields) == 3 and fields[0] == 'makepad':
                assert re.fullmatch('[0-9a-f]{40}', fields[2]), line
                return fields[2]
        raise SystemExit('.jit/sources.lock does not pin makepad')
    return re.search(r'^makepad-app-module\s*=\s*\{[^\n]*\brev\s*=\s*"([^"]+)"', manifest, re.M)[1]


def metadata():
    manifest = (ROOT/'Cargo.toml').read_text()
    return dict(version=re.search(r'^version\s*=\s*"([^"]+)"', manifest, re.M)[1],
                sourceCommit=os.environ['GITHUB_SHA'], clientCommit=os.environ['GITHUB_SHA'],
                clientPath='crates/hagency-client',
                makepadCommit=makepad_commit(manifest))


def sha(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024*1024), b''):
            digest.update(block)
    return digest.hexdigest()


def collect(platform, arch):
    out = ROOT/'release-assets'
    out.mkdir(exist_ok=True)
    assets = []
    for ext in EXTENSIONS[platform]:
        found = list((ROOT/'dist').glob('*'+ext))
        assert len(found) == 1, (ext, found)
        destination = out/f'hagency-desktop-{platform}-{arch}{ext}'
        shutil.copy2(found[0], destination)
        assets.append(destination)
    sums = {p.name: sha(p) for p in assets}
    report = dict(metadata(), platform=platform, arch=arch, assets=sums)
    (out/f'build-{platform}-{arch}.json').write_text(json.dumps(report, indent=2)+'\n')
    (out/f'SHA256SUMS-{platform}-{arch}.txt').write_text(''.join(f'{v}  {k}\n' for k,v in sums.items()))


def verify(scope):
    out = ROOT/'release-assets'
    expected = set(expected_assets(scope))
    recorded = {}
    expected_meta = metadata()
    reports = list(out.glob('build-*.json'))
    assert len(reports) == len(targets(scope)), 'Incomplete platform reports'
    for file in reports:
        report = json.loads(file.read_text())
        assert all(report.get(k) == v for k,v in expected_meta.items()), f'Mixed source versions: {file}'
        for name, digest in report['assets'].items():
            assert name not in recorded, f'Duplicate asset: {name}'
            assert Path(name).name == name and sha(out/name) == digest, f'Checksum mismatch: {name}'
            recorded[name] = digest
    assert set(recorded) == expected, (set(recorded), expected)
    actual = {p.name for p in out.iterdir() if p.suffix in ('.dmg','.exe','.deb','.AppImage')}
    assert actual == expected, 'Unexpected or missing installer'
    (out/'SHA256SUMS.txt').write_text(''.join(f'{recorded[name]}  {name}\n' for name in sorted(recorded)))
    return expected_meta


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['plan','collect','verify'])
    parser.add_argument('--scope', choices=['all','macos-arm64-windows'], default='all')
    parser.add_argument('--platform', choices=list(EXTENSIONS))
    parser.add_argument('--arch')
    args = parser.parse_args()
    if args.command == 'plan':
        data = dict(matrix=json.dumps({'include':targets(args.scope)}, separators=(',',':')),
                    scope=args.scope, **metadata())
        with open(os.environ['GITHUB_OUTPUT'], 'a') as out:
            for key, value in data.items(): out.write(f'{key}={value}\n')
        print(json.dumps(data, indent=2))
    elif args.command == 'collect':
        assert args.platform and args.arch
        collect(args.platform, args.arch)
    else:
        print(json.dumps(verify(args.scope), indent=2))


if __name__ == '__main__':
    main()
