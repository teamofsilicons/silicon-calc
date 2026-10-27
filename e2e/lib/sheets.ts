// Sheets the journeys start from: the sales CSV opened as a new sheet (POST /v1/sheets/import), or, when opening
// files doesn't work, the same table typed into a new sheet, so the journey's own story still runs.

import { readFileSync } from 'node:fs';
import { basename } from 'node:path';
import type { CalcApi, Sheet, Tab } from './calc.ts';
import { SALES_FILE, salesGrid } from './fixtures.ts';
import type { Builder } from './journey.ts';
import { attempt, sameGrid } from './steps.ts';

export interface Opened {
  sheet: Sheet;
  tab: { id: number; name: string };
  /** true: the file was opened; false: the table was typed in instead. */
  imported: boolean;
}

/** Opens a CSV file as a new sheet. */
export function importCsv(api: CalcApi, file: string, title: string, filename = basename(file)): Promise<Sheet> {
  return api.post<Sheet>('/v1/sheets/import', { query: { filename, title }, body: readFileSync(file), contentType: 'text/csv' }, 201);
}

/** A sheet's tabs (GET /v1/sheets/{id}). */
export async function tabsOf(api: CalcApi, sheet: string): Promise<Tab[]> {
  return (await api.get<Sheet>(`/v1/sheets/${sheet}`)).tabs ?? [];
}

/** A new sheet with `rows` typed into its first tab from A1, as one edit (what pasting the table does). */
export async function typeTable(api: CalcApi, title: string, rows: unknown[][]): Promise<Sheet> {
  const sheet = await api.post<Sheet>('/v1/sheets', { json: { title } }, 201);
  const tab = (await tabsOf(api, sheet.id))[0];
  if (!tab) throw new Error(`the new sheet ${sheet.id} has no tab`);
  await api.edit(sheet.id, [{ type: 'set', tab: tab.id, at: 'A1', rows: rows.map((r) => r.map((v) => String(v))) }]);
  return api.get<Sheet>(`/v1/sheets/${sheet.id}`);
}

/**
 * The sales table in a new sheet of the account `api()` names, in two steps (see `attempt`): a check that opens
 * e2e/fixtures/sales-2026.csv (POST /v1/sheets/import), and a required step that, when that failed, types the same
 * table into a new sheet instead; then the table is read back. `done` gets the sheet and its first tab.
 */
export function salesSheet(j: Builder, who: string, api: () => CalcApi, title: string, done: (o: Opened) => void): void {
  attempt<Opened>(j, `${who} has the sales table in a new sheet`, 'opens sales-2026.csv: POST /v1/sheets/import', {
    first: async () => {
      const sheet = await importCsv(api(), SALES_FILE, title);
      const tab = (await tabsOf(api(), sheet.id))[0]!;
      return { sheet, tab: { id: tab.id, name: tab.name }, imported: true };
    },
    otherwise: async () => {
      const sheet = await typeTable(api(), title, salesGrid());
      const tab = sheet.tabs![0]!;
      return { sheet, tab: { id: tab.id, name: tab.name }, imported: false };
    },
    note: 'opening the CSV failed, so the same table was typed into a new sheet (POST /v1/sheets, then one set edit)',
    then: async (o) => {
      const values = await api().values(o.sheet.id, o.tab.id, 'A1:E37');
      sameGrid(values, salesGrid(), 'the sales table in the sheet');
      done(o);
    },
  });
}
