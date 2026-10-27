/**
 * Calc's service from the browser (docs/API.md), through the web kit's proxy (`/api/*`, which adds the bearer token).
 * One function per route the web app uses. A line is the main sheet or one of its copies: every route under
 * `{line}` exists as `/v1/sheets/{sheet}/…` and `/v1/sheets/{sheet}/copies/{copy}/…`.
 */
import { api, queryString, seg, type RequestOptions } from "../client/api";
import type {
  Account, CellDetail, CellsAnswer, Comment, Copy, CopyCompare, DownloadFormat, Edit, EditAnswer, Folder, FolderPage, MergeAnswer,
  MergeRequest, Meta, PublicAccess, Role, RunAnswer, RunRequest, SearchResult, Sharing, Sheet, SheetList, Tab, TabRef, Thread, Transfer,
  Version, WorkbookDiff,
} from "./types";

/** Which line: the main sheet (copy null) or one copy of it. */
export interface LineRef {
  sheet: string;
  copy: string | null;
}

/** The path of a line: `/v1/sheets/{sheet}` or `/v1/sheets/{sheet}/copies/{copy}`. */
export function linePath(line: LineRef): string {
  return line.copy ? `/v1/sheets/${seg(line.sheet)}/copies/${seg(line.copy)}` : `/v1/sheets/${seg(line.sheet)}`;
}

const tabSeg = (tab: TabRef) => seg(String(tab));

/* ---- meta and accounts ------------------------------------------------------------------------------------------- */

export const getMeta = (signal?: AbortSignal) => api.get<Meta>("/v1/meta", { signal });
export const getMe = (signal?: AbortSignal) => api.get<{ account: Account }>("/v1/me", { signal });
export const getNotifications = (signal?: AbortSignal) => api.get<{ enabled: boolean }>("/v1/me/notifications", { signal });
export const enableNotifications = () => api.post<{ enabled: boolean }>("/v1/me/notifications", {});
export const disableNotifications = () => api.delete<{ enabled: boolean }>("/v1/me/notifications");
export const lookupAccount = (id: string, signal?: AbortSignal) => api.get<Account>("/v1/accounts/lookup", { query: { id }, signal });

/* ---- sheets and folders ------------------------------------------------------------------------------------------ */

export function listSheets(params: { scope: "all" | "mine" | "shared" | "trash"; folder?: string | null; q?: string; cursor?: string | null; limit?: number }, signal?: AbortSignal) {
  return api.get<SheetList>("/v1/sheets", { query: { scope: params.scope, folder: params.folder ?? undefined, q: params.q || undefined, cursor: params.cursor ?? undefined, limit: params.limit }, signal });
}

export const createSheet = (body: { title?: string; folder_id?: string | null }) => api.post<Sheet>("/v1/sheets", body, { idempotencyKey: true });

/** Opens a file as a new sheet (the file streams as the raw body). */
export function importSheet(file: File, options: { title?: string; folder_id?: string | null; onUploadProgress?: RequestOptions["onUploadProgress"] } = {}) {
  return api.post<Sheet>(`/v1/sheets/import${queryString({ filename: file.name, title: options.title, folder_id: options.folder_id ?? undefined })}`, undefined, {
    raw: file,
    contentType: file.type || "application/octet-stream",
    idempotencyKey: true,
    onUploadProgress: options.onUploadProgress,
  });
}

export const getSheet = (sheet: string, signal?: AbortSignal) => api.get<Sheet>(`/v1/sheets/${seg(sheet)}`, { signal });
export const updateSheet = (sheet: string, body: { title?: string; folder_id?: string | null }) => api.patch<Sheet>(`/v1/sheets/${seg(sheet)}`, body);
export const trashSheet = (sheet: string) => api.delete(`/v1/sheets/${seg(sheet)}`);
export const restoreSheet = (sheet: string) => api.post<Sheet>(`/v1/sheets/${seg(sheet)}/restore`);
export const deleteSheetForever = (sheet: string, title: string) => api.delete(`/v1/sheets/${seg(sheet)}/forever`, { query: { confirm: title } });
export const transferSheet = (sheet: string, to: string) => api.post<Transfer>(`/v1/sheets/${seg(sheet)}/transfer`, { to });

export const createFolder = (body: { name: string; parent_id?: string | null }) => api.post<Folder>("/v1/folders", body, { idempotencyKey: true });
export const getFolder = (folder: string, signal?: AbortSignal) => api.get<FolderPage>(`/v1/folders/${seg(folder)}`, { signal });
export const updateFolder = (folder: string, body: { name?: string; parent_id?: string | null }) => api.patch<Folder>(`/v1/folders/${seg(folder)}`, body);
export const trashFolder = (folder: string) => api.delete(`/v1/folders/${seg(folder)}`);
export const restoreFolder = (folder: string) => api.post<Folder>(`/v1/folders/${seg(folder)}/restore`);
export const deleteFolderForever = (folder: string, name: string) => api.delete(`/v1/folders/${seg(folder)}/forever`, { query: { confirm: name } });
export const transferFolder = (folder: string, to: string) => api.post<Transfer>(`/v1/folders/${seg(folder)}/transfer`, { to });

export const listTransfers = (signal?: AbortSignal) => api.get<{ incoming: Transfer[]; outgoing: Transfer[] }>("/v1/transfers", { signal });
export const answerTransfer = (id: string, answer: "accept" | "decline" | "cancel") => api.post<Transfer>(`/v1/transfers/${seg(id)}/${answer}`);

export const search = (q: string, limit = 20, signal?: AbortSignal) => api.get<{ results: SearchResult[] }>("/v1/search", { query: { q, limit }, signal });

/* ---- sharing ----------------------------------------------------------------------------------------------------- */

export type ItemRef = { kind: "sheet"; id: string } | { kind: "folder"; id: string };
const itemPath = (item: ItemRef) => (item.kind === "sheet" ? `/v1/sheets/${seg(item.id)}` : `/v1/folders/${seg(item.id)}`);

export const getSharing = (item: ItemRef, signal?: AbortSignal) => api.get<Sharing>(`${itemPath(item)}/sharing`, { signal });
export const invite = (item: ItemRef, body: { invite: string[]; role: Exclude<Role, "owner">; notify?: boolean }) => api.post<Sharing>(`${itemPath(item)}/sharing`, body);
export const changeRole = (item: ItemRef, account: string, role: Exclude<Role, "owner">) => api.patch<Sharing>(`${itemPath(item)}/sharing/${seg(account)}`, { role });
export const removeMember = (item: ItemRef, account: string) => api.delete<Sharing>(`${itemPath(item)}/sharing/${seg(account)}`);
export const removeInvite = (item: ItemRef, email: string) => api.delete<Sharing>(`${itemPath(item)}/invites/${seg(email)}`);
export const setPublic = (sheet: string, body: { enabled?: boolean; access?: PublicAccess; require_email?: boolean }) => api.put<{ enabled: boolean; access: PublicAccess; url?: string }>(`/v1/sheets/${seg(sheet)}/public`, body);

/* ---- cells and edits --------------------------------------------------------------------------------------------- */

export const getTabs = (line: LineRef, signal?: AbortSignal) => api.get<{ version: number; tabs: Tab[] }>(`${linePath(line)}/tabs`, { signal });

export function getCells(line: LineRef, tab: TabRef, range: string | null, options: { version?: number | null; signal?: AbortSignal } = {}) {
  const base = options.version !== undefined && options.version !== null ? `${linePath(line)}/versions/${options.version}` : linePath(line);
  return api.get<CellsAnswer>(`${base}/tabs/${tabSeg(tab)}/cells`, { query: { range: range ?? undefined }, signal: options.signal });
}

export function getValues(line: LineRef, tab: TabRef, range: string, signal?: AbortSignal, version?: number | null) {
  const base = version !== undefined && version !== null ? `${linePath(line)}/versions/${version}` : linePath(line);
  return api.get<{ version: number; range: string; values: Array<Array<number | string | boolean | null>> }>(`${base}/tabs/${tabSeg(tab)}/cells`, { query: { range, format: "values" }, signal });
}

export const getCell = (line: LineRef, tab: TabRef, a1: string, signal?: AbortSignal) => api.get<CellDetail>(`${linePath(line)}/tabs/${tabSeg(tab)}/cells/${seg(a1)}`, { signal });

export const postEdits = (line: LineRef, body: { base_version: number; edits: Edit[]; client_id?: string }) => api.post<EditAnswer>(`${linePath(line)}/edits`, body);

export const postPresence = (line: LineRef, body: { tab: number; range: string; client_id: string }) => api.post<null>(`${linePath(line)}/presence`, body);

/** The address of the live event stream (EventSource connects to it directly; the proxy adds the bearer). */
export const eventsUrl = (line: LineRef, since: number) => `/api${linePath(line)}/events?since=${since}`;

/* ---- code -------------------------------------------------------------------------------------------------------- */

export const runCode = (line: LineRef, body: RunRequest, signal?: AbortSignal) => api.post<RunAnswer>(`${linePath(line)}/run`, body, { signal });

/* ---- files ------------------------------------------------------------------------------------------------------- */

export function importTabs(line: LineRef, file: File, mode: "new_tab" | "replace" = "new_tab", tab?: TabRef, onUploadProgress?: RequestOptions["onUploadProgress"]) {
  return api.post<{ version: number; tabs: Tab[] }>(`${linePath(line)}/tabs/import${queryString({ filename: file.name, mode, tab: tab === undefined ? undefined : String(tab) })}`, undefined, {
    raw: file,
    contentType: file.type || "application/octet-stream",
    onUploadProgress,
  });
}

/** A download link (a plain link to the proxy; the answer streams with its Content-Disposition). */
export function downloadUrl(line: LineRef, format: DownloadFormat, options: { tab?: TabRef; version?: number | null } = {}): string {
  return `/api${linePath(line)}/download${queryString({ format, tab: options.tab === undefined ? undefined : String(options.tab), version: options.version ?? undefined })}`;
}

/* ---- versions ---------------------------------------------------------------------------------------------------- */

export const listVersions = (line: LineRef, cursor?: string | null, limit = 50, signal?: AbortSignal) => api.get<{ versions: Version[]; next_cursor: string | null }>(`${linePath(line)}/versions`, { query: { cursor: cursor ?? undefined, limit }, signal });
export const nameVersion = (line: LineRef, version: number, name: string) => api.post(`${linePath(line)}/versions/${version}/name`, { name });
export const unnameVersion = (line: LineRef, version: number) => api.delete(`${linePath(line)}/versions/${version}/name`);
export const versionTabs = (line: LineRef, version: number, signal?: AbortSignal) => api.get<{ version: number; tabs: Tab[] }>(`${linePath(line)}/versions/${version}/tabs`, { signal });

export function compare(line: LineRef, params: { from: number; to: number | "head"; limit?: number; tab?: TabRef; range?: string }, signal?: AbortSignal) {
  return api.get<WorkbookDiff>(`${linePath(line)}/compare`, { query: { from: params.from, to: params.to, limit: params.limit ?? 500, tab: params.tab === undefined ? undefined : String(params.tab), range: params.range }, signal });
}

export const restoreVersion = (line: LineRef, version: number, body: { tab?: TabRef; range?: string } = {}) => api.post<{ version: number }>(`${linePath(line)}/versions/${version}/restore`, body);
export const undoAccount = (line: LineRef, version: number, account: string) => api.post<{ version: number; cells_changed: number }>(`${linePath(line)}/versions/${version}/undo-account`, { account });
export const newSheetFromVersion = (line: LineRef, version: number, title: string) => api.post<Sheet>(`${linePath(line)}/versions/${version}/new-sheet`, { title });

/* ---- copies ------------------------------------------------------------------------------------------------------ */

export const listCopies = (sheet: string, status: "open" | "all" = "all", signal?: AbortSignal) => api.get<{ copies: Copy[] }>(`/v1/sheets/${seg(sheet)}/copies`, { query: { status }, signal });
export const makeCopy = (line: LineRef, name: string) => api.post<Copy>(`${linePath(line)}/copies`, { name });
export const renameCopy = (sheet: string, copy: string, name: string) => api.patch<Copy>(`/v1/sheets/${seg(sheet)}/copies/${seg(copy)}`, { name });
export const deleteCopy = (sheet: string, copy: string) => api.delete(`/v1/sheets/${seg(sheet)}/copies/${seg(copy)}`);
export const compareCopy = (sheet: string, copy: string, limit = 500, signal?: AbortSignal) => api.get<CopyCompare>(`/v1/sheets/${seg(sheet)}/copies/${seg(copy)}/compare`, { query: { limit }, signal });
export const mergeCopy = (sheet: string, copy: string, body: MergeRequest) => api.post<MergeAnswer>(`/v1/sheets/${seg(sheet)}/copies/${seg(copy)}/merge`, body);

/* ---- comments ---------------------------------------------------------------------------------------------------- */

export const listThreads = (line: LineRef, status: "open" | "resolved" | "all" = "all", signal?: AbortSignal) => api.get<{ threads: Thread[] }>(`${linePath(line)}/comments`, { query: { status }, signal });
export const createThread = (line: LineRef, body: { tab: number; range: string | null; body: string }) => api.post<Thread>(`${linePath(line)}/comments`, body);
export const reply = (thread: string, body: string) => api.post<Comment>(`/v1/comments/${seg(thread)}/replies`, { body });
export const editComment = (comment: string, body: string) => api.patch<Comment>(`/v1/comments/${seg(comment)}`, { body });
export const deleteComment = (comment: string) => api.delete(`/v1/comments/${seg(comment)}`);
export const resolveThread = (thread: string) => api.post<Thread>(`/v1/comments/${seg(thread)}/resolve`);
export const reopenThread = (thread: string) => api.post<Thread>(`/v1/comments/${seg(thread)}/reopen`);
export const react = (comment: string, emoji: string) => api.post(`/v1/comments/${seg(comment)}/reactions`, { emoji });
export const unreact = (comment: string, emoji: string) => api.delete(`/v1/comments/${seg(comment)}/reactions/${seg(emoji)}`);

export type FileViewer = { email: string; email_source: "account" | "entered"; first_viewed_at: string; last_viewed_at: string };
export const getViewers = (sheet: string, offset = 0, signal?: AbortSignal) => api.get<{ viewers: FileViewer[]; total: number; next_offset: number | null }>(`/v1/sheets/${seg(sheet)}/viewers?offset=${offset}`, { signal });
export const recordVisit = (sheet: string, email: string) => api.post<{ ok: boolean }>(`/v1/sheets/${seg(sheet)}/visits`, { email });
