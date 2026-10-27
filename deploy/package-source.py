#!/usr/bin/env python3
"""Archive the complete Calc source used for a Linux build; exclude local state and credentials."""
import hashlib
import io
import json
import pathlib
import subprocess
import tarfile

root = pathlib.Path(__file__).resolve().parent.parent
skip = {'node_modules', '.next', '.git', '__pycache__', 'test-results', 'playwright-report', 'screens'}
files = []
for name in ['Cargo.toml', 'Cargo.lock', 'rustfmt.toml', 'LICENSE', 'crates', 'migrations', 'runner', 'web', 'deploy', 'docs/CLI.md']:
    path = root / name
    if not path.exists():
        continue
    candidates = path.rglob('*') if path.is_dir() else [path]
    for candidate in candidates:
        relative = candidate.relative_to(root)
        if any(part in skip or part.startswith('.next') for part in relative.parts):
            continue
        if candidate.name.startswith('.env') or candidate.suffix in {'.log', '.pyc'} or candidate.name.endswith('.tsbuildinfo'):
            continue
        if candidate.is_symlink():
            raise SystemExit(f'Refusing source symlink: {relative}')
        if candidate.is_file():
            files.append((str(relative), candidate.read_bytes(), candidate.stat().st_mode & 0o777))
files.sort()
manifest = {name: hashlib.sha256(data).hexdigest() for name, data, _ in files}
source_hash = hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest()
revision = 'source-' + source_hash[:20]
metadata = {'revision': revision, 'source_sha256': source_hash, 'base_git_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(), 'files': manifest}
destination = root / '.dev/production' / (revision + '.tar.gz')
destination.parent.mkdir(parents=True, exist_ok=True)
with tarfile.open(destination, 'w:gz') as tar:
    for name, data, mode in files + [('source-manifest.json', (json.dumps(metadata, indent=2) + '\n').encode(), 0o644)]:
        info = tarfile.TarInfo(name)
        info.size = len(data)
        info.mode = mode
        tar.addfile(info, io.BytesIO(data))
receipt = {'revision': revision, 'path': str(destination), 'sha256': hashlib.sha256(destination.read_bytes()).hexdigest(), 'bytes': destination.stat().st_size, 'file_count': len(files), 'base_git_revision': metadata['base_git_revision']}
destination.with_suffix('.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps(receipt, indent=2))
