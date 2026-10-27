import assert from "node:assert/strict";
import { copyFileSync, existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readlinkSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const packageScript = fileURLToPath(new URL("./package-web.sh", import.meta.url));
function fixture(run) {
  const root = mkdtempSync(join(tmpdir(), "calc-package-web-"));
  const write = (relative, value) => { const path = join(root, relative); mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, value); };
  const invoke = () => spawnSync("sh", [join(root, "deploy/package-web.sh"), join(root, "release/web"), "--prebuilt"], { encoding: "utf8" });
  try {
    write("deploy/package-web.sh", ""); copyFileSync(packageScript, join(root, "deploy/package-web.sh"));
    write("web/.next/BUILD_ID", "fixture-build");
    write("web/.next/standalone/.next/BUILD_ID", "fixture-build");
    write("web/.next/standalone/server.js", "// fixture server");
    write("web/.next/standalone/node_modules/lib/index.js", "// dependency");
    write("web/.next/static/app.js", "// static asset");
    write("web/public/icon.svg", "<svg/>");
    write("web/.env.example", "APP_SECRET=\n");
    run({ root, write, invoke });
  } finally { rmSync(root, { recursive: true, force: true }); }
}

test("stages matched standalone, static and public assets while preserving relative dependency links", () => fixture(({ root, invoke }) => {
  symlinkSync("lib", join(root, "web/.next/standalone/node_modules/alias"));
  const answer = invoke();
  assert.equal(answer.status, 0, answer.stderr);
  assert.equal(readFileSync(join(root, "release/web/.next/static/app.js"), "utf8"), "// static asset");
  assert.equal(readFileSync(join(root, "release/web/public/icon.svg"), "utf8"), "<svg/>");
  assert.ok(lstatSync(join(root, "release/web/node_modules/alias")).isSymbolicLink());
  assert.equal(readlinkSync(join(root, "release/web/node_modules/alias")), "lib");
  assert.equal(lstatSync(join(root, "release/web")).mode & 0o777, 0o755, "the service account must traverse the staged root");
  assert.equal(lstatSync(join(root, "release/web/node_modules/lib")).mode & 0o777, 0o755);
  assert.equal(lstatSync(join(root, "release/web/server.js")).mode & 0o777, 0o644);
  assert.equal(invoke().status, 1, "an existing release must not be overwritten");
}));

test("refuses source environment files without printing their values", () => fixture(({ write, invoke }) => {
  write("web/.env.local", "SECRET=do-not-print-this");
  const answer = invoke();
  assert.equal(answer.status, 1);
  assert.match(answer.stderr, /local environment file/);
  assert.ok(!answer.stderr.includes("do-not-print-this"));
}));

test("refuses mismatched static and standalone builds before staging", () => fixture(({ root, write, invoke }) => {
  write("web/.next/standalone/.next/BUILD_ID", "older-build");
  const answer = invoke();
  assert.equal(answer.status, 1);
  assert.match(answer.stderr, /different builds/);
  assert.ok(!existsSync(join(root, "release/web")));
}));

test("refuses environment files hidden in standalone output", () => fixture(({ write, invoke }) => {
  write("web/.next/standalone/.env.production", "SECRET=do-not-print-this");
  const answer = invoke();
  assert.equal(answer.status, 1);
  assert.match(answer.stderr, /environment file is present/);
  assert.ok(!answer.stderr.includes("do-not-print-this"));
}));

test("refuses absolute and escaping dependency symlinks", () => {
  for (const target of ["/tmp", "../../../.."]) fixture(({ root, invoke }) => {
    symlinkSync(target, join(root, "web/.next/standalone/node_modules/outside"));
    const answer = invoke();
    assert.equal(answer.status, 1);
    assert.match(answer.stderr, /Absolute symlink|Escaping symlink/);
    assert.ok(!existsSync(join(root, "release/web")));
  });
});

test("omits build cache directories and cache symlinks from the staged release", () => {
  for (const linked of [false, true]) fixture(({ root, write, invoke }) => {
    if (linked) symlinkSync("/var/cache/silicon-calc-web", join(root, "web/.next/standalone/.next/cache"));
    else write("web/.next/standalone/.next/cache/nested/cache.bin", "build cache");
    const answer = invoke();
    assert.equal(answer.status, 0, answer.stderr);
    assert.equal(lstatSync(join(root, "release/web/.next/cache"), { throwIfNoEntry: false }), undefined);
    assert.equal(readFileSync(join(root, "release/web/.next/BUILD_ID"), "utf8"), "fixture-build");
  });
});

test("cache omission cannot hide an unsafe sibling symlink", () => fixture(({ root, invoke }) => {
  symlinkSync("/var/cache/silicon-calc-web", join(root, "web/.next/standalone/.next/cache-other"));
  const answer = invoke();
  assert.equal(answer.status, 1);
  assert.match(answer.stderr, /Absolute symlink/);
}));
