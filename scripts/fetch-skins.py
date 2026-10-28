#!/usr/bin/env python3
"""Build the third-party skin presets from the Awesome-BongoCat collection.

The catalog reads one directory per model under ``desktop/resources/models`` and
takes the directory name as the model id, so every skin needs a portable ASCII
key. The collection publishes each skin twice, once with a Chinese name and once
with an English one, and the two folders hold byte-identical archives — so the
English name supplies the key and the byte size pairs them without ambiguity.

Those directories are 640 MB and are not in the repository. This script is how a
clone gets them; ``model_listing.rs`` carries the matching id → title table, and
the summary this prints is what to check it against.

    python scripts/fetch-skins.py             # fetch and unpack
    python scripts/fetch-skins.py --list      # print the id/title table only
    python scripts/fetch-skins.py --keep-zips # leave the archives behind

Honours ``HTTPS_PROXY`` / ``http_proxy`` when set.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import pathlib
import re
import shutil
import sys
import urllib.parse
import urllib.request
import zipfile

REPOSITORY = 'ayangweb/Awesome-BongoCat'
BRANCH = 'master'
API = f'https://api.github.com/repos/{REPOSITORY}'
RAW = f'https://raw.githubusercontent.com/{REPOSITORY}/{BRANCH}/'

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent
MODELS_ROOT = REPO_ROOT / 'desktop' / 'resources' / 'models'
CACHE_ROOT = REPO_ROOT / '.tmp' / 'skins'

MODE_KEY = {'标准模式': 'standard', '键盘模式': 'keyboard', '手柄模式': 'gamepad'}
MODE_TITLE = {'标准模式': '', '键盘模式': ' · 键盘', '手柄模式': ' · 手柄'}
MODE_ENUM = {'标准模式': 'Standard', '键盘模式': 'Keyboard', '手柄模式': 'Gamepad'}

JUNK = {'.DS_Store', 'Thumbs.db', 'desktop.ini', '__MACOSX'}
# A converter's leftover: the same manifest without motions or expressions.
JUNK_PREFIX = ('tmp_',)
# Cubism Editor project files; the runtime never opens them.
JUNK_SUFFIX = ('.can3', '.cmo3', '.psd')


def opener() -> urllib.request.OpenerDirector:
    proxy = os.environ.get('https_proxy') or os.environ.get('http_proxy')
    handlers = [urllib.request.ProxyHandler({'http': proxy, 'https': proxy})] if proxy else []
    return urllib.request.build_opener(*handlers)


def api_listing(directory: str, browser: urllib.request.OpenerDirector):
    """Names and sizes of one directory, following the API's pagination."""
    page = 1
    out = []
    while True:
        url = f'{API}/contents/{urllib.parse.quote(directory)}?per_page=100&page={page}'
        with browser.open(url, timeout=60) as response:
            batch = json.load(response)
        if not batch:
            return out
        out.extend((item['name'], item['size']) for item in batch if item['type'] == 'file')
        if len(batch) < 100:
            return out
        page += 1


def kebab(name: str) -> str:
    slug = re.sub(r'[^a-z0-9]+', '-', name.lower())
    return re.sub(r'-{2,}', '-', slug).strip('-')


def fetch_one(task, browser: urllib.request.OpenerDirector):
    path, size, target = task
    if target.exists() and target.stat().st_size == size:
        return (target.name, 'cached')
    url = RAW + urllib.parse.quote(path)
    try:
        with browser.open(url, timeout=300) as response:
            data = response.read()
    except Exception as error:  # noqa: BLE001 - reported, not raised
        return (target.name, f'FAIL {type(error).__name__}: {error}')
    if len(data) != size:
        return (target.name, f'SHORT {len(data)} of {size}')
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    return (target.name, 'ok')


def unpack(archive: pathlib.Path, target: pathlib.Path) -> int:
    """Write one skin into its model directory and return files dropped."""
    if target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True)

    with zipfile.ZipFile(archive) as z:
        members = z.infolist()
        roots = {m.filename.split('/')[0] for m in members if m.filename.strip('/')}
        if len(roots) != 1:
            raise SystemExit(f'{archive.name}: expected one top folder, found {sorted(roots)}')
        root = roots.pop() + '/'
        for member in members:
            if not member.filename.startswith(root):
                continue
            relative = member.filename[len(root):]
            if not relative:
                continue
            if any(part in JUNK for part in relative.split('/')):
                continue
            if pathlib.PurePosixPath(relative).name.startswith(JUNK_PREFIX):
                continue
            if relative.lower().endswith(JUNK_SUFFIX):
                continue
            destination = target / relative
            if member.filename.endswith('/'):
                destination.mkdir(parents=True, exist_ok=True)
            else:
                destination.parent.mkdir(parents=True, exist_ok=True)
                with z.open(member) as source, open(destination, 'wb') as sink:
                    shutil.copyfileobj(source, sink)

    # The catalog refuses a package whose root names more than one manifest. Two
    # skins ship the exporter's own copy beside the real one — bangboo keeps
    # demomodel.model3.json next to cat.model3.json, and the kokomi archive
    # carries a stray leading space in one file name. A skin that ships a single
    # manifest keeps it whatever it is called: suxin and undertaker name theirs
    # demomodel.model3.json and load fine.
    manifests = sorted(
        (path for path in target.iterdir()
         if path.is_file() and path.name.strip().endswith('.model3.json')),
        key=lambda path: path.name,
    )
    if not manifests:
        raise SystemExit(f'{target.name}: no manifest survived extraction')
    keep = next((path for path in manifests if path.name == 'cat.model3.json'), manifests[0])
    dropped = 0
    for path in manifests:
        if path is not keep:
            path.unlink()
            dropped += 1
    if keep.name != keep.name.strip():
        keep.rename(target / keep.name.strip())
    return dropped


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--list', action='store_true',
                        help='print the id/title/mode table and change nothing')
    parser.add_argument('--keep-zips', action='store_true',
                        help='leave the downloaded archives in .tmp/skins')
    options = parser.parse_args()

    browser = opener()
    chinese = api_listing('models/Chinese', browser)
    english = api_listing('models/English', browser)
    by_size = {size: name for name, size in english}
    if len(by_size) != len(english):
        print('the English folder has two archives of the same size; cannot pair by size',
              file=sys.stderr)
        return 1
    print(f'{len(chinese)} Chinese skins, {len(english)} English twins')

    plans = []
    for name, size in sorted(chinese):
        twin = by_size.get(size)
        if twin is None:
            print(f'no English twin for {name}', file=sys.stderr)
            return 1
        character_zh, mode_zh = (part.strip() for part in name[:-4].split(' · '))
        character_en = twin[:-4].split(' · ')[0].strip()
        if mode_zh not in MODE_KEY:
            print(f'unknown mode {mode_zh} in {name}', file=sys.stderr)
            return 1
        plans.append((name, size, character_zh, character_en, mode_zh))

    modes_per_character: dict[str, int] = {}
    for _, _, character_zh, _, _ in plans:
        modes_per_character[character_zh] = modes_per_character.get(character_zh, 0) + 1

    rows = []
    for name, size, character_zh, character_en, mode_zh in plans:
        base = kebab(character_en)
        slug = (f'{base}-{MODE_KEY[mode_zh]}'
                if modes_per_character[character_zh] > 1 else base)
        rows.append((slug, character_zh + MODE_TITLE[mode_zh], MODE_ENUM[mode_zh], name, size))

    if options.list:
        for slug, title, mode, _, _ in sorted(rows):
            print(f'    PresetModel {{ id: "{slug}", title: "{title}", mode: ModelInputMode::{mode} }},')
        return 0

    CACHE_ROOT.mkdir(parents=True, exist_ok=True)
    tasks = [
        (f'models/Chinese/{name}', size, CACHE_ROOT / name)
        for name, size, _, _, _ in plans
    ]
    failed = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
        for index, (name, status) in enumerate(pool.map(lambda t: fetch_one(t, browser), tasks), 1):
            print(f'[{index}/{len(tasks)}] {status}  {name}', flush=True)
            if status not in ('ok', 'cached'):
                failed.append((name, status))
    if failed:
        print(f'\n{len(failed)} downloads failed; re-run to retry', file=sys.stderr)
        return 1

    MODELS_ROOT.mkdir(parents=True, exist_ok=True)
    for slug, _, _, name, _ in sorted(rows):
        dropped = unpack(CACHE_ROOT / name, MODELS_ROOT / slug)
        note = f'  (dropped {dropped} extra manifest)' if dropped else ''
        print(f'  {slug}{note}')

    total = sum(p.stat().st_size for p in MODELS_ROOT.rglob('*') if p.is_file())
    print(f'\n{len(rows)} skins under {MODELS_ROOT}, models tree now {total/1048576:.0f} MB')
    if not options.keep_zips:
        shutil.rmtree(CACHE_ROOT, ignore_errors=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
