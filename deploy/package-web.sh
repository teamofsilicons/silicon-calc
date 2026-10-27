#!/bin/sh
# Build and stage the native Next standalone service. Run on the target Linux architecture.
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ] || { [ "$#" -eq 2 ] && [ "$2" != "--prebuilt" ]; }; then
  echo "Usage: deploy/package-web.sh DESTINATION [--prebuilt]" >&2
  exit 2
fi
destination=$1
repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
web_dir=$repo_dir/web

# Next can include .env files in standalone output. Production configuration belongs in web.env at runtime.
for env_file in "$web_dir"/.env*; do
  if [ -e "$env_file" ] && [ "$(basename -- "$env_file")" != ".env.example" ]; then
    echo "Refusing a build from a tree containing a local environment file: $(basename -- "$env_file"). Use the secret-free release source." >&2
    exit 1
  fi
done

if [ "${2:-}" != "--prebuilt" ]; then
  (
    cd "$web_dir"
    export NEXT_TELEMETRY_DISABLED=1
    export NEXT_DIST_DIR=.next
    pnpm install --frozen-lockfile --prod=false
    pnpm typecheck
    pnpm lint
    pnpm test
    NEXT_OUTPUT=standalone pnpm build
  )
fi

node --input-type=module - "$web_dir" "$destination" <<'NODE'
import { chmodSync, cpSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, readlinkSync, realpathSync, renameSync, rmSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';

const web = resolve(process.argv[2]);
const destination = resolve(process.argv[3]);
const standalone = join(web, '.next/standalone');
if (destination === web || destination.startsWith(web + sep)) throw new Error('Stage outside the web source directory.');
if (lstatSync(destination, { throwIfNoEntry: false })) throw new Error('Destination already exists; choose a fresh release directory.');
for (const path of [join(standalone, 'server.js'), join(standalone, '.next/BUILD_ID'), join(web, '.next/BUILD_ID'), join(web, '.next/static'), join(web, 'public')]) {
  if (!existsSync(path)) throw new Error('Missing standalone build asset: ' + relative(web, path));
}
const buildId = readFileSync(join(web, '.next/BUILD_ID'), 'utf8').trim();
if (!buildId) throw new Error('Build id must not be empty.');
if (readFileSync(join(standalone, '.next/BUILD_ID'), 'utf8').trim() !== buildId) throw new Error('Standalone and static assets are from different builds.');

function verifyTree(root, normalize = false, omitBuildCache = false) {
  const canonical = realpathSync(root);
  const visit = path => {
    if (omitBuildCache && relative(root, path) === join('.next', 'cache')) return;
    if (basename(path).startsWith('.env')) throw new Error('An environment file is present in the release tree: ' + relative(root, path));
    const stat = lstatSync(path);
    if (stat.isSymbolicLink()) {
      if (isAbsolute(readlinkSync(path))) throw new Error('Absolute symlink in release: ' + relative(root, path));
      const target = realpathSync(path);
      if (target !== canonical && !target.startsWith(canonical + sep)) throw new Error('Escaping symlink in release: ' + relative(root, path));
    } else if (stat.isDirectory()) {
      if (normalize) chmodSync(path, 0o755);
      for (const item of readdirSync(path)) visit(join(path, item));
    } else if (!stat.isFile()) throw new Error('Unsupported file in release: ' + relative(root, path));
    else if (normalize) chmodSync(path, stat.mode & 0o111 ? 0o755 : 0o644);
  };
  if (normalize) chmodSync(root, 0o755);
  for (const item of readdirSync(root)) visit(join(root, item));
}

verifyTree(standalone, false, true);
mkdirSync(dirname(destination), { recursive: true });
const stage = mkdtempSync(join(dirname(destination), '.calc-web-'));
try {
  const copy = { recursive: true, dereference: false, verbatimSymlinks: true };
  cpSync(standalone, stage, { ...copy, filter: path => relative(standalone, path) !== join('.next', 'cache') });
  cpSync(join(web, '.next/static'), join(stage, '.next/static'), copy);
  cpSync(join(web, 'public'), join(stage, 'public'), copy);
  verifyTree(stage, true);
  renameSync(stage, destination);
  console.log('Staged Calc web build ' + buildId + ' at ' + destination);
} finally {
  rmSync(stage, { recursive: true, force: true });
}
NODE
