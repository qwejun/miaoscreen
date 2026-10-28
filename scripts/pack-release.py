#!/usr/bin/env python3
"""Build the release archives: the application itself, and the optional skins.

Two downloads, because the skins are 640 MB and nearly all of it is model data
that only some people want: the main archive stays around 15 MB and carries the
three models the product ships, and the skin pack adds the rest.

    python scripts/pack-release.py                 # both archives
    python scripts/pack-release.py --main          # application only
    python scripts/pack-release.py --skins         # skins only

The main archive unpacks to ``MiaoScreen-<version>-windows-x64/`` and the skin
archive mirrors the install layout (``resources/models/<id>/``) so it can be
extracted over that directory. Inputs are the release build and the model tree;
run ``scripts/fetch-skins.py`` first if the skins are missing.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import shutil
import sys
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
DIST = ROOT / 'dist'
MODELS = ROOT / 'desktop' / 'resources' / 'models'
BUILD = ROOT / 'desktop' / 'target' / 'release'
# The models the product ships itself; everything else is a third-party skin.
BUILT_IN = ('standard', 'keyboard', 'gamepad')
# Icons and tray art are compiled into the executable by `include_bytes!`
# (`product_icon_contract.rs`, `system_menu_native.rs`), so only the Live2D
# models are read from disk at runtime.
PACKAGED_RESOURCES = ('models',)


def version() -> str:
    text = (ROOT / 'desktop' / 'Cargo.toml').read_text(encoding='utf-8')
    match = re.search(r'^version = "([^"]+)"', text, re.MULTILINE)
    if not match:
        raise SystemExit('cannot read the workspace version')
    return match.group(1)


def skin_ids() -> list[str]:
    """Every model directory that is not one of the product's own three."""
    return sorted(
        path.name for path in MODELS.iterdir()
        if path.is_dir() and path.name not in BUILT_IN
    )


def write_archive(target: pathlib.Path, entries) -> tuple[int, int]:
    """Write one zip from (disk path or None for a directory, archive name)."""
    files = 0
    with zipfile.ZipFile(target, 'w', zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
        for source, arcname in entries:
            if source is None:
                archive.writestr(arcname.rstrip('/') + '/', '')
            else:
                archive.write(source, arcname)
                files += 1
    return files, target.stat().st_size


def main_entries(stage: pathlib.Path):
    for path in sorted(stage.rglob('*')):
        arcname = f'{stage.name}/{path.relative_to(stage).as_posix()}'
        yield (None if path.is_dir() else path, arcname)


def pack_main(release: str) -> pathlib.Path:
    name = f'MiaoScreen-{release}-windows-x64'
    stage = DIST / name
    if stage.exists():
        shutil.rmtree(stage)
    stage.mkdir(parents=True)

    executable = BUILD / 'bongocat-app.exe'
    if not executable.is_file():
        raise SystemExit(f'missing release build at {executable}')
    shutil.copy2(executable, stage / f'{name}.exe')

    # The VC runtime is the one the 0.1.4 archive shipped; the executable links
    # against it dynamically.
    runtime_source = next(
        (p for p in (DIST / 'MiaoScreen-0.1.4-windows-x64').glob('vcruntime140.dll')), None
    )
    if runtime_source is None:
        raise SystemExit('missing vcruntime140.dll to copy into the archive')
    shutil.copy2(runtime_source, stage / 'vcruntime140.dll')

    for directory in PACKAGED_RESOURCES:
        for model in BUILT_IN:
            source = MODELS / model
            if not source.is_dir():
                raise SystemExit(f'missing built-in model {model} at {source}')
            shutil.copytree(source, stage / 'resources' / directory / model)

    target = DIST / f'{name}.zip'
    if target.exists():
        target.unlink()
    files, size = write_archive(target, main_entries(stage))
    print(f'{target.name}: {size/1048576:.1f} MB, {files} files')
    return target


def pack_skins(release: str) -> pathlib.Path:
    ids = skin_ids()
    if not ids:
        raise SystemExit('no skins on disk; run scripts/fetch-skins.py first')

    entries = []
    for model in ids:
        source = MODELS / model
        entries.append((None, f'resources/models/{model}/'))
        for path in sorted(source.rglob('*')):
            entries.append((path, f'resources/models/{model}/{path.relative_to(source).as_posix()}'))

    target = DIST / f'MiaoScreen-{release}-skins.zip'
    if target.exists():
        target.unlink()
    files, size = write_archive(target, entries)
    print(f'{target.name}: {size/1048576:.1f} MB, {len(ids)} skins, {files} files')
    return target


def write_checksums(paths: list[pathlib.Path], release: str) -> pathlib.Path:
    target = DIST / f'SHA256SUMS-{release}.txt'
    lines = []
    for path in paths:
        digest = hashlib.sha256()
        with path.open('rb') as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b''):
                digest.update(chunk)
        lines.append(f'{digest.hexdigest()} *{path.name}')
        print(f'  {digest.hexdigest()}  {path.name}')
    target.write_text('\n'.join(lines) + '\n', encoding='utf-8', newline='\n')
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--main', action='store_true', help='pack the application archive')
    parser.add_argument('--skins', action='store_true', help='pack the skin archive')
    options = parser.parse_args()
    both = not options.main and not options.skins

    release = version()
    produced = []
    if both or options.main:
        produced.append(pack_main(release))
    if both or options.skins:
        produced.append(pack_skins(release))

    apk = ROOT / 'android' / 'build' / 'CatScreen.apk'
    if apk.is_file():
        target = DIST / f'CatScreen-{release}-android.apk'
        shutil.copy2(apk, target)
        print(f'{target.name}: {target.stat().st_size/1048576:.1f} MB')
        produced.append(target)

    print('\nSHA256:')
    write_checksums(produced, release)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
