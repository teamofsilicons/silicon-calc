import { api } from "../client/api";
import { linePath, type LineRef } from "../calc/api";
import type { Value } from "../calc/types";
import { cellName } from "./address";
import { runBrowserCommand, SandboxStartupError, type JavascriptDependencies } from "./browser-code";

export interface JavascriptFunction { name: string; parameters: string[]; code: string; account?: string | null }
export interface JavascriptAnswer { rows: Value[][]; error?: string; dependencies?: JavascriptDependencies }
export interface JavascriptCall { id: number; name: string; tab: number; row: number; col: number; args: unknown[]; result?: JavascriptAnswer | null }
export interface JavascriptState { version: number; epoch: number; functions: JavascriptFunction[]; names: string[]; calls: JavascriptCall[] }
export const getFunctions = (line: LineRef, signal?: AbortSignal) => api.get<JavascriptState>(`${linePath(line)}/functions`, { signal });
export const getLibrary = (signal?: AbortSignal) => api.get<{ functions: JavascriptFunction[] }>("/v1/me/functions", { signal });
export const saveLibrary = (f: JavascriptFunction) => api.put<JavascriptFunction>(`/v1/me/functions/${encodeURIComponent(f.name)}`, f);
export const removeLibrary = (name: string) => api.delete(`/v1/me/functions/${encodeURIComponent(name)}`);
export const saveFunction = (line: LineRef, f: JavascriptFunction, base_version: number, remove = false) => api.put<{ version: number }>(`${linePath(line)}/functions`, { function: f, base_version, remove });
export const commitResults = (line: LineRef, state: JavascriptState, results: Array<[number, JavascriptAnswer]>, signal?: AbortSignal) => api.post<{ version: number }>(`${linePath(line)}/javascript/results`, { base_version: state.version, epoch: state.epoch, results }, { signal });

/** Import referenced helpers too, without copying an account's unrelated private source. */
export function neededLibrary(state: JavascriptState, library: JavascriptFunction[]): Set<string> {
  const needed = new Set(state.names);
  const local = new Map(state.functions.filter(f => !f.account).map(f => [f.name, f]));
  const available = new Map(library.map(f => [f.name, f]));
  const scan = (code: string) => {
    for (const token of code.match(/[A-Za-z_][A-Za-z_0-9]*/g) ?? []) if (available.has(token) || local.has(token)) needed.add(token);
  };
  for (const call of state.calls) if (call.name === "JS" && typeof call.args[0] === "string") scan(call.args[0]);
  const visited = new Set<string>();
  for (const name of needed) {
    if (visited.has(name)) continue;
    visited.add(name);
    const f = local.get(name) ?? available.get(name) ?? state.functions.find(f => f.name === name);
    if (f) scan(f.code);
  }
  return needed;
}

/** Generate only a worker program. Source never reaches eval/Function in the editor's origin. */
export function formulaProgram(call: JavascriptCall, functions: JavascriptFunction[]): string {
  const unique = new Map<string, JavascriptFunction>();
  for (const f of [...functions].sort((a,b) => Number(!a.account) - Number(!b.account))) unique.set(f.name.toUpperCase(), f);
  const library = [...unique].map(([name, f]) => `async function ${name}(${f.parameters.join(",")}) {\n${f.code}\n}`).join("\n");
  const invocation = call.name === "JS" ? `(async () => {\n${typeof call.args[0] === "string" ? call.args[0] : 'throw new Error("The first JS argument must be JavaScript text.");'}\n})()` : `${call.name}(...args)`;
  return `${library}\nconst args = ${JSON.stringify(call.name === "JS" ? call.args.slice(1) : call.args)};\nconst result = await ${invocation};
const rows = Array.isArray(result) ? (result.every(Array.isArray) ? result : [result]) : [[result ?? null]];
if (!rows.length || !rows[0].length) return {columns:["Value"], rows:[[null]]};
return {columns:rows[0].map((_,i) => String(i+1)), rows};`;
}
export async function evaluateCall(line: LineRef, call: JavascriptCall, functions: JavascriptFunction[], signal: AbortSignal): Promise<JavascriptAnswer> {
  try {
    const result = await runBrowserCommand(line, formulaProgram(call, functions), { signal, timeoutMs: 5000, context: { tab: call.tab, cell: cellName(call.row, call.col) }, formula: true });
    return { rows: result.rows.map(row => row.map(value => value === null ? { t: "empty" } : typeof value === "number" ? { t: "number", v: value } : typeof value === "boolean" ? { t: "bool", v: value } : { t: "text", v: value })), dependencies: result.dependencies };
  } catch (error) {
    if (signal.aborted || error instanceof SandboxStartupError || (error instanceof Error && error.message === "CALC_INPUT_PENDING")) throw error;
    return { rows: [[{ t: "error", v: "#CALC!" }]], error: (error instanceof Error ? error.message : "The JavaScript could not run.").slice(0, 2048) };
  }
}
