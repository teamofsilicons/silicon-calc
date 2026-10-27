// Journeys: what a Carbon or a Silicon does with Silicon Calc, end to end, as steps.
//
//   export default journey('A Carbon imports a CSV', (j) => {
//     let sheet: any;
//     j.step('the Carbon imports the CSV', async () => { sheet = await ada.api.post(...); });
//     j.check('the sheet is found by search', async () => { ... });   // a failure here doesn't stop the journey
//   });
//
// A `step` is something later steps need: when it fails, the rest of the journey is skipped (and says why). A
// `check` stands on its own: when it fails, it is recorded and the journey goes on. `j.part('…')` starts a part of
// the journey that doesn't need the parts before it (beyond the first, unnamed one): a step that fails skips only the
// rest of its own part. `j.note()` adds a line to the step that is running (what was measured, which fallback was
// used). `j.cleanup()` runs at the end, always.
//
// e2e/journeys/run.ts runs journeys and prints and saves what passed and what failed.

export type StepStatus = 'pass' | 'fail' | 'skip';

export interface StepResult {
  name: string;
  /** The part of the journey it belongs to ('' for the first, unnamed one). */
  part: string;
  /** false: a check (its failure doesn't stop the journey). */
  required: boolean;
  status: StepStatus;
  ms: number;
  error?: string;
  notes: string[];
}

export interface JourneyResult {
  name: string;
  title: string;
  passed: number;
  failed: number;
  skipped: number;
  ms: number;
  /** The first failure, for summaries. */
  error?: string;
  steps: StepResult[];
}

interface StepDef {
  name: string;
  part: number;
  required: boolean;
  timeoutMs: number;
  run: () => Promise<unknown>;
}

export interface StepOptions {
  /** The longest the step may take (default 120 s). */
  timeoutMs?: number;
}

export class Builder {
  readonly steps: StepDef[] = [];
  readonly cleanups: Array<() => Promise<unknown>> = [];
  /** Part titles by number; part 0 is the journey's first, unnamed part, which every other part needs. */
  readonly parts: string[] = [''];
  current: StepResult | null = null;

  /** Something later steps need: if it fails, the rest of its part (or of the journey, in the first part) is skipped. */
  step(name: string, run: () => Promise<unknown>, options: StepOptions = {}): void {
    this.steps.push({ name, part: this.parts.length - 1, required: true, timeoutMs: options.timeoutMs ?? 120_000, run });
  }

  /** A check on its own: if it fails, the journey goes on. */
  check(name: string, run: () => Promise<unknown>, options: StepOptions = {}): void {
    this.steps.push({ name, part: this.parts.length - 1, required: false, timeoutMs: options.timeoutMs ?? 120_000, run });
  }

  /** Starts a part that needs only the first part of the journey, not the parts between. */
  part(title: string): void {
    this.parts.push(title);
  }

  /** Runs at the end of the journey, whatever happened. */
  cleanup(run: () => Promise<unknown>): void {
    this.cleanups.push(run);
  }

  /** A line about the step that is running, printed under it and saved with it. */
  note(text: string): void {
    if (this.current) this.current.notes.push(text);
    else console.log(`    note: ${text}`);
  }
}

export interface Journey {
  title: string;
  build: (j: Builder) => void;
}

export function journey(title: string, build: (j: Builder) => void): Journey {
  return { title, build };
}

/** Why something failed, on one line plus whatever the error adds (HTTP answers, CLI output). */
export function describe(error: unknown): string {
  if (error instanceof Error) {
    const extra = (error as { detail?: string }).detail;
    return `${error.message}${extra ? `\n${extra}` : ''}`;
  }
  return String(error);
}

async function withTimeout<T>(ms: number, run: () => Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const limit = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(`the step took longer than ${Math.round(ms / 1000)} s`)), ms);
  });
  try {
    return await Promise.race([run(), limit]);
  } finally {
    clearTimeout(timer);
  }
}

function indent(text: string, by: string): string {
  return text
    .split('\n')
    .map((l) => `${by}${l}`)
    .join('\n');
}

/** Runs one journey's steps in order and prints each as it finishes. */
export async function runJourney(name: string, j: Journey): Promise<JourneyResult> {
  const b = new Builder();
  const started = performance.now();
  const result: JourneyResult = { name, title: j.title, passed: 0, failed: 0, skipped: 0, ms: 0, steps: [] };
  console.log(`\n${name}: ${j.title}`);
  try {
    j.build(b);
  } catch (error) {
    result.failed = 1;
    result.error = `the journey could not be set up: ${describe(error)}`;
    console.log(`  FAIL  ${result.error}`);
    return result;
  }
  // Which required step stopped each part (part 0 stops them all).
  const stoppedBy = new Map<number, string>();
  let shownPart = 0;
  for (const def of b.steps) {
    const step: StepResult = { name: def.name, part: b.parts[def.part] ?? '', required: def.required, status: 'skip', ms: 0, notes: [] };
    result.steps.push(step);
    if (def.part !== shownPart) {
      shownPart = def.part;
      console.log(`  -- ${b.parts[def.part]}`);
    }
    const stopper = stoppedBy.get(0) ?? stoppedBy.get(def.part);
    if (stopper) {
      step.error = `not run: "${stopper}" failed`;
      result.skipped++;
      console.log(`  skip  ${def.name}`);
      continue;
    }
    b.current = step;
    const t0 = performance.now();
    try {
      await withTimeout(def.timeoutMs, def.run);
      step.status = 'pass';
      result.passed++;
    } catch (error) {
      step.status = 'fail';
      step.error = describe(error);
      result.failed++;
      result.error ??= `${def.name}: ${step.error}`;
      if (def.required) stoppedBy.set(def.part, def.name);
    }
    step.ms = Math.round(performance.now() - t0);
    b.current = null;
    const tag = step.status === 'pass' ? '  ok  ' : def.required ? '  FAIL' : '  fail';
    console.log(`${tag}  ${def.name} (${(step.ms / 1000).toFixed(1)} s)${def.required ? '' : ' [check]'}`);
    for (const n of step.notes) console.log(indent(n, '          · '));
    if (step.error) console.log(indent(step.error, '          '));
  }
  for (const c of b.cleanups.reverse()) {
    try {
      await withTimeout(60_000, c);
    } catch (error) {
      console.log(`  note  cleaning up failed: ${describe(error)}`);
    }
  }
  result.ms = Math.round(performance.now() - started);
  return result;
}

// ------------------------------------------------------------------------------------------------ small helpers

export function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Calls `probe` until it returns something other than undefined/false, or throws the last reason after `timeoutMs`. */
export async function eventually<T>(what: string, probe: () => Promise<T | undefined | false>, timeoutMs = 20_000, everyMs = 250): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown = null;
  for (;;) {
    try {
      const v = await probe();
      if (v !== undefined && v !== false) return v as T;
      last = null;
    } catch (error) {
      last = error;
    }
    if (Date.now() > deadline) {
      throw new Error(`${what} did not happen within ${Math.round(timeoutMs / 1000)} s${last ? `: ${describe(last)}` : ''}`);
    }
    await sleep(everyMs);
  }
}

export function seconds(ms: number): string {
  return `${(ms / 1000).toFixed(2)} s`;
}
