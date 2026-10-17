// Runs the journeys against a running Calc service, the local Silicon Accounts stack and the mock Ting, as real
// Carbons and Silicons (scripts/e2e.sh starts what they need and runs this).
//
//   node e2e/journeys/run.ts                       every journey (e2e/journeys/NN-name.ts, in order)
//   node e2e/journeys/run.ts 02-silicon-mention    only these (names without .ts; a prefix like 02 works too)
//
// Environment: CALC_API_URL, MOCK_TING_URL, CALC_CLI, CALC_WEB_URL, STACK_JSON (see e2e/lib/world.ts);
// JOURNEY_RESULTS (where the results go; default .dev/e2e/results.json).
// Prints every step as it finishes and a summary at the end; exits 1 when anything failed.

import { existsSync, mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { type Journey, type JourneyResult, describe, runJourney } from '../lib/journey.ts';
import { ROOT } from '../lib/stack.ts';
import { world } from '../lib/world.ts';

const here = dirname(fileURLToPath(import.meta.url));
const wanted = process.argv.slice(2).filter((a) => !a.startsWith('-'));
const files = readdirSync(here)
  .filter((f) => /^\d\d-[a-z0-9-]+\.ts$/.test(f))
  .sort()
  .filter((f) => wanted.length === 0 || wanted.some((w) => f === `${w}.ts` || f === w || f.startsWith(`${w}-`) || f.startsWith(w)));
const resultsFile = process.env.JOURNEY_RESULTS ?? join(ROOT, '.dev', 'e2e', 'results.json');
const startedAt = new Date().toISOString();

function save(journeys: JourneyResult[], preflight: string[]): void {
  mkdirSync(dirname(resultsFile), { recursive: true });
  writeFileSync(resultsFile, `${JSON.stringify({ started_at: startedAt, finished_at: new Date().toISOString(), preflight, journeys }, null, 2)}\n`);
}

if (wanted.length && files.length === 0) {
  console.error(`error: no journey matches ${wanted.join(', ')}\nhint: the journeys are ${readdirSync(here).filter((f) => /^\d\d-/.test(f)).map((f) => f.replace(/\.ts$/, '')).join(', ')}`);
  process.exit(2);
}

// ---- preflight: the stack, the service and the mock Ting must answer
const w = world();
const preflight: string[] = [];
async function probe(name: string, url: string): Promise<boolean> {
  try {
    const res = await fetch(url, { signal: AbortSignal.timeout(10_000) });
    if (res.ok) return true;
    preflight.push(`${name} answered HTTP ${res.status} at ${url}`);
  } catch (error) {
    preflight.push(`${name} does not answer at ${url}: ${(error as Error).message}`);
  }
  return false;
}
const up = [
  await probe('Silicon Accounts', `${w.env.stack.accounts.api_url}/readyz`),
  await probe('the Calc service', `${w.apiUrl}/healthz`),
  await probe('the mock Ting', `${w.ting.url}/healthz`),
].every(Boolean);

console.log(`Silicon Calc journeys: ${files.length} against ${w.apiUrl} (Silicon Accounts ${w.env.stack.accounts.public_url}, mock Ting ${w.ting.url})`);
console.log(`calc command line: ${w.cliBin}${existsSync(w.cliBin) ? '' : ' (not built: the steps that use it fail)'}`);

const results: JourneyResult[] = [];
if (!up) {
  for (const p of preflight) console.log(`  FAIL  ${p}`);
  for (const f of files) {
    const name = f.replace(/\.ts$/, '');
    results.push({ name, title: name, passed: 0, failed: 1, skipped: 0, ms: 0, error: `not run: ${preflight.join('; ')}`, steps: [] });
  }
  save(results, preflight);
  process.exit(1);
}

for (const f of files) {
  const name = f.replace(/\.ts$/, '');
  let j: Journey;
  try {
    j = (await import(pathToFileURL(join(here, f)).href)).default as Journey;
    if (!j || typeof j.build !== 'function') throw new Error(`${f} does not export a journey (export default journey(…))`);
  } catch (error) {
    results.push({ name, title: name, passed: 0, failed: 1, skipped: 0, ms: 0, error: `the journey could not be loaded: ${describe(error)}`, steps: [] });
    console.log(`\n${name}\n  FAIL  the journey could not be loaded: ${describe(error)}`);
    continue;
  }
  results.push(await runJourney(name, j));
  save(results, preflight);
}
save(results, preflight);

// ---- summary
const totals = results.reduce((t, r) => ({ passed: t.passed + r.passed, failed: t.failed + r.failed, skipped: t.skipped + r.skipped }), { passed: 0, failed: 0, skipped: 0 });
console.log('\nSummary');
for (const r of results) {
  const state = r.failed ? 'FAIL' : r.passed ? 'pass' : 'skip';
  console.log(`  ${state.padEnd(5)} ${r.name.padEnd(28)} ${String(r.passed).padStart(3)} passed ${String(r.failed).padStart(3)} failed ${String(r.skipped).padStart(3)} skipped  (${(r.ms / 1000).toFixed(1)} s)`);
}
console.log(`\n${totals.passed} steps passed, ${totals.failed} failed, ${totals.skipped} skipped. Results: ${resultsFile}`);
process.exit(totals.failed ? 1 : 0);
