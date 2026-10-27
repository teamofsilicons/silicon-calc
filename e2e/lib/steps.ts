// Shared step patterns for the journeys.

import assert from 'node:assert/strict';
import type { Builder } from './journey.ts';

/**
 * Something done one way, with another way to fall back on, in two steps:
 *
 * 1. `<what> (<how>)`: a check that runs `first()`. Its failure is recorded, and the journey goes on.
 * 2. `<what>`: when the check failed, `otherwise()` gets the same outcome another way (it should first look whether
 *    the failed attempt left the outcome there already), with `note` added to the step; then `then()` checks the
 *    outcome. This step is required: what follows in the journey needs the outcome, however it came about.
 *
 * So a broken part (a CLI command, an import route) shows up as a failed check while the rest of the story still
 * runs against what does work.
 */
export function attempt<T>(
  j: Builder,
  what: string,
  how: string,
  o: {
    first: () => Promise<T>;
    otherwise: () => Promise<T>;
    note: string;
    then?: (result: T, way: 'first' | 'otherwise') => Promise<void>;
    timeoutMs?: number;
  },
): void {
  let got: { value: T } | null = null;
  j.check(
    `${what} (${how})`,
    async () => {
      got = { value: await o.first() };
    },
    { timeoutMs: o.timeoutMs },
  );
  j.step(
    what,
    async () => {
      let way: 'first' | 'otherwise' = 'first';
      if (!got) {
        j.note(o.note);
        got = { value: await o.otherwise() };
        way = 'otherwise';
      }
      if (o.then) await o.then(got.value, way);
    },
    { timeoutMs: o.timeoutMs },
  );
}

/**
 * Something a Silicon does with the calc command line: `cli()` as a check, and when the command line could not do
 * it, the same Silicon does it over HTTP (`http()`), so the rest of the journey still runs (see `attempt`).
 */
export function viaCli<T>(
  j: Builder,
  what: string,
  command: string,
  o: { cli: () => Promise<T>; http: () => Promise<T>; then?: (result: T, how: 'cli' | 'http') => Promise<void>; timeoutMs?: number },
): void {
  attempt(j, what, command, {
    first: o.cli,
    otherwise: o.http,
    note: 'the calc command line could not do it, so the same account did it over HTTP',
    then: o.then ? (r, way) => o.then!(r, way === 'first' ? 'cli' : 'http') : undefined,
    timeoutMs: o.timeoutMs,
  });
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

/** Fails with `why` unless `ready` is true (for checks that need something an earlier check was to make). */
export function needs(ready: unknown, why: string): void {
  if (!ready) throw new Error(`not checked: ${why}`);
}
