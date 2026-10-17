// Shared step patterns for the journeys.

import assert from 'node:assert/strict';
import type { Builder } from './journey.ts';

/**
 * Something a Silicon does with the calc command line, in two steps:
 *
 * 1. `<what> (calc …)`: a check that runs `cli()`. Its failure is recorded, and the journey goes on.
 * 2. `<what>`: when the CLI failed, the same Silicon does it over HTTP (`http()`, which should first look whether the
 *    CLI's attempt left the result there already), then `then()` checks the outcome. This step is required: what
 *    follows in the journey needs the outcome, however it came about.
 *
 * So a broken CLI shows up as a failed check while the rest of the story still runs against the service.
 */
export function viaCli<T>(
  j: Builder,
  what: string,
  command: string,
  o: { cli: () => Promise<T>; http: () => Promise<T>; then?: (result: T, how: 'cli' | 'http') => Promise<void>; timeoutMs?: number },
): void {
  let got: { value: T } | null = null;
  j.check(
    `${what} (${command})`,
    async () => {
      got = { value: await o.cli() };
    },
    { timeoutMs: o.timeoutMs },
  );
  j.step(
    what,
    async () => {
      let how: 'cli' | 'http' = 'cli';
      if (!got) {
        j.note('the calc command line could not do it, so the same Silicon did it over HTTP');
        got = { value: await o.http() };
        how = 'http';
      }
      if (o.then) await o.then(got.value, how);
    },
    { timeoutMs: o.timeoutMs },
  );
}

/** Asserts that `rows` (plain values) equal `expected`, comparing numbers as numbers and everything else as text. */
export function sameGrid(rows: unknown[][], expected: unknown[][], what = 'the cells'): void {
  const norm = (v: unknown) => (typeof v === 'number' ? v : v === null || v === undefined ? '' : String(v));
  const got = rows.map((r) => r.map(norm));
  const want = expected.map((r) => r.map(norm));
  // Trailing empty cells don't count.
  const trim = (g: unknown[][]) => g.map((r) => {
    const c = [...r];
    while (c.length && c[c.length - 1] === '') c.pop();
    return c;
  });
  assert.deepEqual(trim(got), trim(want), `${what} differ`);
}

/** The number in a cell value or text ("12,400" → 12400). */
export function num(v: unknown): number {
  if (typeof v === 'number') return v;
  if (typeof v === 'string') {
    const n = Number(v.replace(/[, ]/g, ''));
    if (!Number.isNaN(n)) return n;
  }
  if (v && typeof v === 'object' && 'v' in (v as Record<string, unknown>)) return num((v as { v: unknown }).v);
  throw new Error(`${JSON.stringify(v)} is not a number`);
}
