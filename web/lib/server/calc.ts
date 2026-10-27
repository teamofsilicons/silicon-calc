/**
 * What the sheet editor's pages read on the server before the first paint: the sheet (or copy), its tabs at the version
 * asked for, and the first block of the first tab, so the grid shows cells at once. Signed in, the calls carry the
 * session's token (apiFetch); signed out they go without one, which the service answers for public sheets only
 * (UNDERSTANDING.md: "Reading a public sheet never needs signing in").
 */
import "server-only";
import { cookies } from "next/headers";
import { viewGrantHeaders } from "./view-grants";
import { cache } from "react";
import type { Account, CellsAnswer, Copy, Sheet, Tab } from "../calc/types";
import { ApiError, errorFromResponse, networkError, type ApiErrorInit } from "../errors";
import { apiFetch, getSession } from "./rsc";
import { serverEnv } from "./env";

/** Reads the service without a token (a public sheet, signed out). */
async function anonymousFetch<T>(path: string, grants: Record<string, string>): Promise<T> {
  const env = serverEnv();
  let response: Response;
  try {
    response = await fetch(`${env.appApiUrl}${path}`, { headers: { Accept: "application/json", ...grants }, cache: "no-store", redirect: "manual", signal: AbortSignal.timeout(30_000) });
  } catch (error) {
    throw networkError(error, "GET", path, "the service");
  }
  const text = await response.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  if (!response.ok) throw errorFromResponse(response, body, "GET", path);
  return body as T;
}

export type LoadedLine =
  | {
      kind: "ok";
      sheet: Sheet;
      copy: Copy | null;
      copies: Copy[];
      tabs: Tab[];
      version: number;
      atVersion: number | null;
      me: Account | null;
      initialCells: CellsAnswer | null;
    }
  | { kind: "email_required" }
  | { kind: "sign_in" }
  | { kind: "not_found" }
  | { kind: "error"; error: ApiErrorInit };

const seg = (value: string | number) => encodeURIComponent(String(value));

/** The first block the grid reads (lib/sheet/store.ts: 128 rows by 32 columns). */
const FIRST_BLOCK = "A1:AF128";

export function loadLine(options: { sheet: string; copy: string | null; version: number | null; tab: number | null }): Promise<LoadedLine> {
  return loadLineOnce(options.sheet, options.copy, options.version, options.tab);
}

// Metadata and the editor share one authorized read within this request only.
// Primitive keys also deduplicate calls that construct separate options objects.
const loadLineOnce = cache(async (sheetId: string, copyId: string | null, atVersion: number | null, initialTab: number | null): Promise<LoadedLine> => {
  const options = { sheet: sheetId, copy: copyId, version: atVersion, tab: initialTab };
  const session = await getSession();
  const grants = viewGrantHeaders(await cookies());
  const read = <T,>(path: string) => (session ? apiFetch<T>(path, { headers: grants }) : anonymousFetch<T>(path, grants));
  try {
    const sheet = await read<Sheet>(`/v1/sheets/${seg(options.sheet)}`);
    let copy: Copy | null = null;
    let copies: Copy[] = [];
    try {
      copies = (await read<{ copies: Copy[] }>(`/v1/sheets/${seg(options.sheet)}/copies?status=all`)).copies ?? [];
    } catch (failure) {
      if (ApiError.from(failure).code === "email_required") throw failure;
      copies = [];
    }
    if (options.copy) {
      copy = copies.find(entry => entry.id === options.copy) ?? null;
      if (!copy) return { kind: "not_found" };
    }
    const base = options.copy ? `/v1/sheets/${seg(options.sheet)}/copies/${seg(options.copy)}` : `/v1/sheets/${seg(options.sheet)}`;
    let tabs: Tab[];
    let version: number;
    if (options.version !== null) {
      const answer = await read<{ version: number; tabs: Tab[] }>(`${base}/versions/${options.version}/tabs`);
      tabs = answer.tabs;
      version = answer.version;
    } else if (!options.copy && sheet.tabs) {
      tabs = sheet.tabs;
      version = sheet.version;
    } else {
      const answer = await read<{ version: number; tabs: Tab[] }>(`${base}/tabs`);
      tabs = answer.tabs;
      version = answer.version;
    }
    const ordered = [...tabs].sort((a, b) => a.index - b.index);
    const first = ordered.find(tab => tab.id === options.tab) ?? ordered[0] ?? null;
    let initialCells: CellsAnswer | null = null;
    if (first) {
      const cellsBase = options.version !== null ? `${base}/versions/${options.version}` : base;
      initialCells = await read<CellsAnswer>(`${cellsBase}/tabs/${seg(first.id)}/cells?range=${FIRST_BLOCK}`).catch(failure => { if (ApiError.from(failure).code === "email_required") throw failure; return null; });
    }
    const me: Account | null = session ? { uuid: session.account.uuid, id: session.account.id, kind: session.account.kind, display_name: session.account.display_name, pfp_url: session.account.pfp_url } : null;
    return { kind: "ok", sheet, copy, copies, tabs: ordered, version, atVersion: options.version, me, initialCells };
  } catch (failure) {
    const error = ApiError.from(failure);
    if (error.status === 0 && error.code === "client_error") throw failure;
    if (error.code === "email_required") return { kind: "email_required" };
    if (!session && (error.status === 401 || error.status === 403 || error.status === 404)) return { kind: "sign_in" };
    if (error.status === 404) return { kind: "not_found" };
    return { kind: "error", error: error.toJSON() };
  }
});
