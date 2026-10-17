#!/usr/bin/env node
/**
 * Writes lib/sheet/functions.names.ts from the engine's function table (crates/calc-engine/src/formula/functions.rs),
 * so the formula editor offers exactly the functions Calc knows. Run it after the engine gains functions:
 *
 *   pnpm sync-functions
 *
 * The generated file is kept in the repository, so the web app builds where the engine's source is not present.
 */
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const source = join(here, "..", "..", "crates", "calc-engine", "src", "formula", "functions.rs");
const target = join(here, "..", "lib", "sheet", "functions.names.ts");

const rust = readFileSync(source, "utf8");
const start = rust.indexOf("static TABLE");
const end = rust.indexOf("\n];", start);
if (start < 0 || end < 0) throw new Error(`no function TABLE found in ${source}`);
const names = [...new Set([...rust.slice(start, end).matchAll(/\("([A-Z0-9.]+)"\s*,/g)].map(match => match[1]))].sort();
if (names.length < 20) throw new Error(`only ${names.length} functions found in ${source}; the table's shape may have changed`);

const lines = [
  "/**",
  " * Every function Calc's engine knows, generated from crates/calc-engine/src/formula/functions.rs by",
  " * scripts/sync-functions.mjs (`pnpm sync-functions`). Do not edit by hand.",
  " */",
  "export const ENGINE_FUNCTIONS: readonly string[] = [",
  ...chunk(names.map(name => JSON.stringify(name)), 10).map(row => `  ${row.join(", ")},`),
  "];",
  "",
];
writeFileSync(target, lines.join("\n"));
console.log(`${names.length} functions written to ${target}`);

function chunk(items, size) {
  const out = [];
  for (let i = 0; i < items.length; i += size) out.push(items.slice(i, i + size));
  return out;
}
