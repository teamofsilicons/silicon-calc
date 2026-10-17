// The real `calc` command line, as one account, for the journeys. Every account gets its own CALC_HOME (its
// session file), so nothing here touches ~/.calc, and every run points at the e2e service and Accounts stack.
//
//   const cli = new Cli({ bin, home: '.dev/e2e/cli/si-analyst', apiUrl, accountsUrl, who: 'si:analyst' });
//   await cli.login(silicon);                                   // calc login --silicon si:… --stk-stdin
//   const rows = await cli.json(['read', sheetId, 'A1:C9']);     // calc --json read …
//   const text = await cli.text(['read', sheetId, 'A1:C9', '--format', 'csv']);

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';

export interface CliRun {
  args: string[];
  code: number | null;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  ms: number;
}

export class CliFailure extends Error {
  readonly run: CliRun;
  readonly detail: string;

  constructor(who: string, run: CliRun, expected: string) {
    const error = /^error: (.*)$/m.exec(run.stderr)?.[1];
    const hint = /^hint: (.*)$/m.exec(run.stderr)?.[1];
    const first = error ?? run.stderr.trim().split('\n')[0] ?? '';
    super(`calc ${run.args.map(shellWord).join(' ')} (as ${who}) ${run.signal ? `was killed (${run.signal})` : `exited ${run.code}`}, expected ${expected}${first ? `: ${first}` : ''}${hint ? ` (hint: ${hint})` : ''}`);
    this.name = 'CliFailure';
    this.run = run;
    const lines = [];
    if (run.stderr.trim() && !error) lines.push(`stderr: ${run.stderr.trim().slice(0, 800)}`);
    if (run.stdout.trim()) lines.push(`stdout: ${run.stdout.trim().slice(0, 400)}`);
    this.detail = lines.join('\n');
  }
}

function shellWord(s: string): string {
  return /^[A-Za-z0-9_./:@%+=,-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

export interface CliOptions {
  bin: string;
  home: string;
  apiUrl: string;
  accountsUrl: string;
  who: string;
}

export class Cli {
  readonly o: CliOptions;

  constructor(o: CliOptions) {
    this.o = o;
    mkdirSync(o.home, { recursive: true, mode: 0o700 });
  }

  /** Whether the binary was built. */
  get available(): boolean {
    return existsSync(this.o.bin);
  }

  env(extra: Record<string, string> = {}): Record<string, string> {
    const env: Record<string, string> = {};
    for (const [k, v] of Object.entries(process.env)) {
      // The machine's own Silicon settings must not leak in: this run is the e2e account only.
      if (v === undefined || k.startsWith('ACCOUNTS_') || k.startsWith('CALC_') || k === 'SILICON_HOME') continue;
      env[k] = v;
    }
    return {
      ...env,
      CALC_HOME: this.o.home,
      CALC_API_URL: this.o.apiUrl,
      CALC_ACCOUNTS_URL: this.o.accountsUrl,
      NO_COLOR: '1',
      ...extra,
    };
  }

  /** Runs `calc <args>`; answers whatever happened. */
  run(args: string[], o: { stdin?: string; timeoutMs?: number; env?: Record<string, string> } = {}): Promise<CliRun> {
    if (!this.available) {
      return Promise.reject(new Error(`the calc command line is not built at ${this.o.bin} (scripts/e2e.sh builds it; set CALC_CLI to use another)`));
    }
    return new Promise((resolve, reject) => {
      const t0 = performance.now();
      const child = spawn(this.o.bin, args, { env: this.env(o.env), stdio: ['pipe', 'pipe', 'pipe'] });
      const out: Buffer[] = [];
      const err: Buffer[] = [];
      child.stdout.on('data', (d: Buffer) => out.push(d));
      child.stderr.on('data', (d: Buffer) => err.push(d));
      const timer = setTimeout(() => child.kill('SIGKILL'), o.timeoutMs ?? 120_000);
      child.on('error', (e) => {
        clearTimeout(timer);
        reject(new Error(`calc could not be started (${this.o.bin}): ${e.message}`));
      });
      child.on('close', (code, signal) => {
        clearTimeout(timer);
        resolve({ args, code, signal, stdout: Buffer.concat(out).toString('utf8'), stderr: Buffer.concat(err).toString('utf8'), ms: performance.now() - t0 });
      });
      child.stdin.on('error', () => {});
      if (o.stdin !== undefined) child.stdin.end(o.stdin);
      else child.stdin.end();
    });
  }

  /** `calc <args>` must succeed; answers stdout. */
  async text(args: string[], o: { stdin?: string; timeoutMs?: number } = {}): Promise<string> {
    const r = await this.run(args, o);
    if (r.code !== 0) throw new CliFailure(this.o.who, r, 'exit 0');
    return r.stdout;
  }

  /** `calc --json <args>` must succeed; answers the JSON it printed. */
  async json<T = any>(args: string[], o: { stdin?: string; timeoutMs?: number } = {}): Promise<T> {
    const r = await this.run(['--json', ...args], o);
    if (r.code !== 0) throw new CliFailure(this.o.who, r, 'exit 0');
    const text = r.stdout.trim();
    try {
      return JSON.parse(text) as T;
    } catch {
      // A stream prints one JSON value per line; the last one is the answer.
      const last = text.split('\n').filter(Boolean).pop() ?? '';
      try {
        return JSON.parse(last) as T;
      } catch {
        throw new CliFailure(this.o.who, r, 'JSON on stdout (--json)');
      }
    }
  }

  /** `calc <args>` must fail (exit not 0); answers the run, for its message. */
  async fails(args: string[], o: { stdin?: string; timeoutMs?: number } = {}): Promise<CliRun> {
    const r = await this.run(args, o);
    if (r.code === 0) throw new CliFailure(this.o.who, r, 'a refusal (exit not 0)');
    return r;
  }

  /** A Silicon signs in with its si:id and STK (on stdin, never in the command line). */
  async loginSilicon(id: string, stk: string): Promise<any> {
    return this.json(['login', '--silicon', id, '--stk-stdin', '--force'], { stdin: stk, timeoutMs: 90_000 });
  }

  /** Signs in with a short-lived token (`calc login --slt-stdin`). */
  async loginSlt(slt: string): Promise<any> {
    return this.json(['login', '--slt-stdin', '--force'], { stdin: slt, timeoutMs: 90_000 });
  }

  /**
   * A Carbon signs in the way Carbons do: `calc --json login` starts Silicon Accounts' device flow and prints the
   * code (a `device_code` JSON line on stderr); `approve` approves it as the Carbon (its browser session), and the
   * command finishes signed in.
   */
  loginCarbonDevice(approve: (userCode: string) => Promise<void>, timeoutMs = 120_000): Promise<any> {
    if (!this.available) return Promise.reject(new Error(`the calc command line is not built at ${this.o.bin}`));
    return new Promise((resolve, reject) => {
      const args = ['--json', 'login', '--force'];
      const t0 = performance.now();
      const child = spawn(this.o.bin, args, { env: this.env(), stdio: ['pipe', 'pipe', 'pipe'] });
      let stdout = '';
      let stderr = '';
      let approving: Promise<void> | null = null;
      let approveError: unknown = null;
      const timer = setTimeout(() => child.kill('SIGKILL'), timeoutMs);
      child.stdout.on('data', (d: Buffer) => (stdout += d.toString('utf8')));
      child.stderr.on('data', (d: Buffer) => {
        stderr += d.toString('utf8');
        if (approving) return;
        for (const line of stderr.split('\n')) {
          try {
            const v = JSON.parse(line);
            if (v?.event === 'device_code' && typeof v.user_code === 'string') {
              approving = approve(v.user_code).catch((e) => {
                approveError = e;
                child.kill('SIGTERM');
              });
              break;
            }
          } catch {
            // not a JSON line
          }
        }
      });
      child.on('error', (e) => {
        clearTimeout(timer);
        reject(new Error(`calc could not be started (${this.o.bin}): ${e.message}`));
      });
      child.on('close', async (code, signal) => {
        clearTimeout(timer);
        if (approving) await approving;
        const run: CliRun = { args, code, signal, stdout, stderr, ms: performance.now() - t0 };
        if (approveError) return reject(approveError);
        if (!approving) return reject(new CliFailure(this.o.who, run, 'a device_code line on stderr (--json)'));
        if (code !== 0) return reject(new CliFailure(this.o.who, run, 'exit 0 after the code was approved'));
        try {
          resolve(JSON.parse(stdout.trim()));
        } catch {
          reject(new CliFailure(this.o.who, run, 'JSON on stdout (--json)'));
        }
      });
      child.stdin.end();
    });
  }
}
