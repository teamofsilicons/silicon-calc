// The data the journeys bring in, and what they expect back.
//
// - e2e/fixtures/sales-2026.csv: revenue by month and region (36 rows). EU revenue drops in March: what the Carbon
//   asks the Silicon about.
// - e2e/fixtures/targets.csv: a small table to add as a new tab.
// - million(): a 1,000,000-row CSV made on first use in .dev/e2e/fixtures (deterministic), with its count and sums.

import { createWriteStream, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { once } from 'node:events';
import { join } from 'node:path';
import { E2E_DIR } from './stack.ts';

export const FIXTURES = join(E2E_DIR, 'fixtures');

export interface SalesRow {
  month: string;
  m: number;
  region: string;
  units: number;
  revenue: number;
}

export const SALES_FILE = join(FIXTURES, 'sales-2026.csv');
export const TARGETS_FILE = join(FIXTURES, 'targets.csv');

export function salesCsv(): string {
  return readFileSync(SALES_FILE, 'utf8');
}

export function sales(): SalesRow[] {
  const [, ...lines] = salesCsv().trim().split('\n');
  return lines.map((l) => {
    const [month, m, region, units, revenue] = l.split(',');
    return { month: month!, m: Number(m), region: region!, units: Number(units), revenue: Number(revenue) };
  });
}

/** The sales table as a grid of plain values, header first (what reading A1:E37 gives back). */
export function salesGrid(): Array<Array<string | number>> {
  return [['month', 'm', 'region', 'units', 'revenue'], ...sales().map((r) => [r.month, r.m, r.region, r.units, r.revenue])];
}

/** Revenue per month, in month order. */
export function revenueByMonth(): Array<{ month: string; m: number; revenue: number }> {
  const out = new Map<number, { month: string; m: number; revenue: number }>();
  for (const r of sales()) {
    const e = out.get(r.m) ?? { month: r.month, m: r.m, revenue: 0 };
    e.revenue += r.revenue;
    out.set(r.m, e);
  }
  return [...out.values()].sort((a, b) => a.m - b.m);
}

export function revenueOf(month: string, region: string): number {
  return sales().find((r) => r.month === month && r.region === region)!.revenue;
}

export function totalRevenue(): number {
  return sales().reduce((s, r) => s + r.revenue, 0);
}

// ------------------------------------------------------------------------------------------------ a million rows

export interface Million {
  file: string;
  rows: number;
  bytes: number;
  /** sum(amount) */
  total: number;
  /** sum(amount) per region */
  byRegion: Record<string, number>;
  /** count per region */
  countByRegion: Record<string, number>;
  maxAmount: number;
  /** The last rows, as written (id, day, region, amount). */
  tail: Array<[number, number, string, number]>;
}

const REGIONS = ['EU', 'US', 'APAC', 'LATAM'];

function millionRow(id: number): [number, number, string, number] {
  return [id, (id % 365) + 1, REGIONS[id % 4]!, ((id * 7919) % 1000) + 1];
}

/** `id,day,region,amount` for ids 1 to 1,000,000 (made once; about 20 MB). */
export async function million(dir: string, rows = 1_000_000): Promise<Million> {
  mkdirSync(dir, { recursive: true });
  const file = join(dir, `million-${rows}.csv`);
  const facts = `${file}.json`;
  if (existsSync(file) && existsSync(facts)) return JSON.parse(readFileSync(facts, 'utf8')) as Million;
  const byRegion: Record<string, number> = {};
  const countByRegion: Record<string, number> = {};
  let total = 0;
  let maxAmount = 0;
  let bytes = 0;
  const tmp = `${file}.tmp`;
  const out = createWriteStream(tmp);
  const write = async (s: string) => {
    bytes += Buffer.byteLength(s);
    if (!out.write(s)) await once(out, 'drain');
  };
  await write('id,day,region,amount\n');
  let chunk: string[] = [];
  for (let id = 1; id <= rows; id++) {
    const r = millionRow(id);
    total += r[3];
    byRegion[r[2]] = (byRegion[r[2]] ?? 0) + r[3];
    countByRegion[r[2]] = (countByRegion[r[2]] ?? 0) + 1;
    if (r[3] > maxAmount) maxAmount = r[3];
    chunk.push(r.join(','));
    if (chunk.length === 10_000) {
      await write(`${chunk.join('\n')}\n`);
      chunk = [];
    }
  }
  if (chunk.length) await write(`${chunk.join('\n')}\n`);
  await new Promise<void>((resolve, reject) => out.end((e?: Error | null) => (e ? reject(e) : resolve())));
  renameSync(tmp, file);
  const m: Million = { file, rows, bytes, total, byRegion, countByRegion, maxAmount, tail: [] };
  for (let id = rows - 6; id <= rows; id++) m.tail.push(millionRow(id));
  writeFileSync(facts, JSON.stringify(m, null, 2));
  return m;
}

// ------------------------------------------------------------------------------------------------ file formats

export const FORMATS = ['xlsx', 'xls', 'ods', 'numbers', 'csv', 'tsv', 'json', 'parquet', 'pdf'] as const;
export type Format = (typeof FORMATS)[number];
/** Every format a file can be opened from (PDF is download only). */
export const OPENABLE: Format[] = ['xlsx', 'xls', 'ods', 'numbers', 'csv', 'tsv', 'json', 'parquet'];

/** Checks that `bytes` look like a `format` file; throws what's wrong. */
export function checkFormat(format: Format, bytes: Buffer): string {
  const head = (n: number) => bytes.subarray(0, n);
  const zip = () => head(4).equals(Buffer.from([0x50, 0x4b, 0x03, 0x04]));
  if (bytes.length === 0) throw new Error(`the ${format} file is empty`);
  switch (format) {
    case 'xlsx':
    case 'ods':
    case 'numbers':
      if (!zip()) throw new Error(`a ${format} file is a zip archive (PK\\x03\\x04), but it starts with ${head(8).toString('hex')}`);
      if (format === 'ods' && !bytes.includes(Buffer.from('application/vnd.oasis.opendocument.spreadsheet'))) {
        throw new Error('the ods file has no OpenDocument spreadsheet mimetype');
      }
      if (format === 'xlsx' && !bytes.includes(Buffer.from('xl/'))) throw new Error('the xlsx file has no xl/ parts');
      return `${bytes.length} bytes, a zip archive`;
    case 'xls':
      if (!head(8).equals(Buffer.from([0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]))) {
        throw new Error(`an xls file is an OLE2 compound file (D0 CF 11 E0 A1 B1 1A E1), but it starts with ${head(8).toString('hex')}`);
      }
      return `${bytes.length} bytes, an OLE2 compound file`;
    case 'parquet':
      if (head(4).toString('latin1') !== 'PAR1' || bytes.subarray(bytes.length - 4).toString('latin1') !== 'PAR1') {
        throw new Error(`a Parquet file starts and ends with PAR1, but it starts with ${head(4).toString('hex')}`);
      }
      return `${bytes.length} bytes, Parquet`;
    case 'pdf':
      if (head(5).toString('latin1') !== '%PDF-') throw new Error(`a PDF starts with %PDF-, but it starts with ${head(8).toString('hex')}`);
      return `${bytes.length} bytes, PDF ${head(8).toString('latin1').slice(5).trim()}`;
    case 'json': {
      const v = JSON.parse(bytes.toString('utf8'));
      if (!Array.isArray(v?.tabs)) throw new Error('the JSON download has no "tabs" list');
      return `${bytes.length} bytes, ${v.tabs.length} tab(s)`;
    }
    case 'csv':
    case 'tsv': {
      const text = bytes.toString('utf8');
      const sep = format === 'csv' ? ',' : '\t';
      if (!text.split('\n')[0]!.includes(sep)) throw new Error(`the ${format} file's first line has no ${format === 'csv' ? 'comma' : 'tab'}: ${text.slice(0, 80)}`);
      return `${bytes.length} bytes, ${text.trim().split('\n').length} lines`;
    }
  }
}

/** Parses a small CSV/TSV (quotes allowed) into rows of strings. */
export function parseDelimited(text: string, sep = ','): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (quoted) {
      if (ch === '"' && text[i + 1] === '"') {
        cell += '"';
        i++;
      } else if (ch === '"') quoted = false;
      else cell += ch;
    } else if (ch === '"') quoted = true;
    else if (ch === sep) {
      row.push(cell);
      cell = '';
    } else if (ch === '\n' || ch === '\r') {
      if (ch === '\r' && text[i + 1] === '\n') i++;
      row.push(cell);
      rows.push(row);
      row = [];
      cell = '';
    } else cell += ch;
  }
  if (cell !== '' || row.length) {
    row.push(cell);
    rows.push(row);
  }
  return rows;
}
