/**
 * The shapes of Calc's service API (docs/API.md), as the web app reads and sends them. Everything here mirrors that
 * document; when the two disagree, docs/API.md wins and this file changes.
 */

export type AccountKind = "carbon" | "silicon";

/** An account, always shown by its current id (`c:ada`, `si:analyst`), keyed by its uuid. */
export interface Account {
  uuid: string;
  id: string;
  kind: AccountKind;
  display_name: string;
  pfp_url?: string | null;
}

export type Role = "owner" | "editor" | "commenter" | "viewer";
export type PublicAccess = "view" | "comment" | "edit";

/** A cell's value. Errors are their Excel text. */
export type Value =
  | { t: "empty" }
  | { t: "number"; v: number }
  | { t: "text"; v: string }
  | { t: "bool"; v: boolean }
  | { t: "error"; v: string };

export interface TabCode {
  language: CodeLanguage;
  code: string;
  inputs: number[];
  run_by: Account;
  run_at: string;
  keep_updated: boolean;
}

export interface Tab {
  id: number;
  name: string;
  index: number;
  /** The grid size to show: max(used extent + 1, 1000 rows / 26 columns). */
  rows: number;
  cols: number;
  used: string | null;
  frozen_rows: number;
  frozen_cols: number;
  color: string | null;
  code: TabCode | null;
}

export interface Sheet {
  id: string;
  title: string;
  owner: Account;
  folder_id: string | null;
  role: Role;
  public: { enabled: boolean; access: PublicAccess };
  version: number;
  created_at: string;
  created_by: Account;
  updated_at: string;
  updated_by: Account;
  trashed_at: string | null;
  tabs?: Tab[];
  copies: number;
}

export interface Folder {
  id: string;
  name: string;
  parent_id: string | null;
  owner: Account;
  role: Role;
  created_at: string;
  trashed_at: string | null;
}

export interface SheetList {
  sheets: Sheet[];
  folders: Folder[];
  next_cursor: string | null;
}

export interface FolderPage {
  folder: Folder;
  path: Folder[];
  sheets: Sheet[];
  folders: Folder[];
}

export type CopyStatus = "open" | "merged" | "deleted";

export interface Copy {
  id: string;
  sheet_id: string;
  name: string;
  parent_id: string | null;
  forked_at: number;
  version: number;
  created_by: Account;
  created_at: string;
  status: CopyStatus;
  merged_at: string | null;
  merged_by: Account | null;
}

export interface Transfer {
  id: string;
  kind: "sheet" | "folder";
  item_id: string;
  title: string;
  from: Account;
  to: Account;
  status: "pending" | "accepted" | "declined" | "cancelled";
  created_at: string;
}

export interface Member {
  account: Account;
  role: Exclude<Role, "owner">;
  via: "direct" | { folder: Folder };
}

export interface Invite {
  email: string;
  role: Exclude<Role, "owner">;
  invited_by: Account;
  created_at: string;
}

export interface Sharing {
  owner: Account;
  members: Member[];
  invites: Invite[];
  public: { enabled: boolean; access: PublicAccess; url?: string };
}

/* ---- styles (the engine's Style JSON) ---------------------------------------------------------------------------- */

export type HAlign = "general" | "left" | "center" | "right";
export type VAlign = "bottom" | "middle" | "top";
export type BorderStyle = "none" | "thin" | "medium" | "thick" | "dashed" | "dotted" | "double";

export interface Border {
  style: BorderStyle;
  color?: string;
}

export interface Style {
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  font_size?: number;
  font?: string;
  color?: string;
  fill?: string;
  h_align?: HAlign;
  v_align?: VAlign;
  wrap?: boolean;
  num_fmt?: string;
  border_top?: Border;
  border_bottom?: Border;
  border_left?: Border;
  border_right?: Border;
}

/** A partial change to a style: a field replaces that part; null clears it. */
export interface StylePatch {
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  font_size?: number | null;
  font?: string | null;
  color?: string | null;
  fill?: string | null;
  h_align?: HAlign;
  v_align?: VAlign;
  wrap?: boolean;
  num_fmt?: string | null;
  border_top?: Border | null;
  border_bottom?: Border | null;
  border_left?: Border | null;
  border_right?: Border | null;
  clear?: boolean;
}

/* ---- cells ------------------------------------------------------------------------------------------------------- */

export interface Cell {
  a: string;
  r: number;
  c: number;
  v: Value;
  t: string;
  f?: string;
  s?: number;
  spill?: boolean;
  by?: string;
  at?: string;
}

export type ChartKind = "line" | "bar" | "column" | "area" | "pie" | "scatter";

/** A chart as the cells answer gives it. The anchor is a cell address (`E2`). */
export interface Chart {
  id: string;
  tab: number;
  kind: ChartKind;
  title: string;
  range: string;
  anchor: string;
  width: number;
  height: number;
}

/** A protected tab or range. No range: the whole tab. */
export interface Protection {
  id: string;
  tab: number;
  range: string | null;
  editors: Account[];
  description: string;
}

export interface CellsAnswer {
  version: number;
  tab: Tab;
  range: string | null;
  cells: Cell[];
  styles: Record<string, Style>;
  accounts: Record<string, Account>;
  col_widths: Record<string, number>;
  row_heights: Record<string, number>;
  hidden_rows: number[];
  hidden_cols: string[];
  merges: string[];
  frozen_rows: number;
  frozen_cols: number;
  charts: Chart[];
  protections: Protection[];
}

export interface CellHistoryEntry {
  version: number;
  by: Account | string;
  at: string;
  before: Value | string | null;
  after: Value | string | null;
}

export interface CellDetail extends Partial<CellsAnswer> {
  cell?: Cell | null;
  history: CellHistoryEntry[];
}

/* ---- edits ------------------------------------------------------------------------------------------------------- */

export type TabRef = number | string;

export type Edit =
  | { type: "set"; tab: TabRef; cells: Array<{ a: string; input: string }> }
  | { type: "set"; tab: TabRef; at: string; rows: string[][] }
  | { type: "values"; tab: TabRef; at: string; rows: unknown[][] }
  | { type: "style"; tab: TabRef; range: string; patch: StylePatch }
  | { type: "clear"; tab: TabRef; range: string; what: "contents" | "formats" | "all" }
  | { type: "insert_rows" | "insert_cols" | "delete_rows" | "delete_cols"; tab: TabRef; at: number; count: number }
  | { type: "move_rows" | "move_cols"; tab: TabRef; from: number; count: number; to: number }
  | { type: "sort"; tab: TabRef; range: string; by: Array<{ col: string; asc: boolean }>; header: boolean }
  | { type: "fill"; tab: TabRef; from: string; to: string }
  | { type: "add_tab"; name: string; index?: number }
  | { type: "rename_tab"; tab: TabRef; name: string }
  | { type: "move_tab"; tab: TabRef; index: number }
  | { type: "delete_tab"; tab: TabRef }
  | { type: "tab_props"; tab: TabRef; frozen_rows?: number; frozen_cols?: number; color?: string | null }
  | { type: "col_width"; tab: TabRef; cols: string; width: number | null }
  | { type: "row_height"; tab: TabRef; rows: string; height: number | null }
  | { type: "hide_rows" | "show_rows" | "hide_cols" | "show_cols"; tab: TabRef; range: string }
  | { type: "merge" | "unmerge"; tab: TabRef; range: string }
  | { type: "names"; names: Array<{ name: string; ref: string }> }
  | { type: "chart"; chart: { id?: string; tab: TabRef; kind: ChartKind; title: string; range: string; anchor: string; width: number; height: number } }
  | { type: "delete_chart"; id: string }
  | { type: "protect"; protection: { id?: string; tab: TabRef; range?: string; editors: string[]; description: string } }
  | { type: "unprotect"; id: string }
  | { type: "undo" }
  | { type: "redo" };

export type Change = { tab: number; range: string } | { tab: number; reload: true } | { tabs: true } | { meta: true } | { tab: number; id?: number; added?: boolean };

export interface Overwrite {
  tab: number;
  a: string;
  by: Account;
  at: string;
  before: Value;
  version: number;
}

export interface EditAnswer {
  version: number;
  changes: Change[];
  overwrote: Overwrite[];
  skipped: number;
}

/* ---- live -------------------------------------------------------------------------------------------------------- */

export interface ChangeEvent {
  version: number;
  by: Account;
  at: string;
  changes: Change[];
  cells?: Cell[];
  client_id?: string;
}

export interface Viewer {
  account: Account;
  tab: number;
  range: string;
  client_id: string;
  seen_at: string;
}

/* ---- code -------------------------------------------------------------------------------------------------------- */

export type CodeLanguage = "sql" | "python" | "javascript";

export interface RunRequest {
  language: CodeLanguage;
  code: string;
  into: null | { new_tab: string } | { tab: TabRef; at: string };
  keep_updated?: boolean;
}

export interface RunAnswer {
  columns: string[];
  rows: unknown[][];
  row_count: number;
  truncated: boolean;
  stdout: string;
  stderr: string;
  duration_ms: number;
  version: number | null;
  tab: Tab | null;
}

/* ---- versions, compare, copies ----------------------------------------------------------------------------------- */

export type VersionKind = "edit" | "import" | "code" | "merge" | "restore" | "undo" | "redo" | "create";

export interface Version {
  version: number;
  from_version: number;
  by: Account;
  started_at: string;
  ended_at: string;
  summary: string;
  name: string | null;
  kind: VersionKind;
}

export interface CellSummary {
  value?: Value;
  formula?: string;
  style?: Style;
}

export interface CellDiff {
  tab: number;
  row: number;
  col: number;
  address: string;
  before: CellSummary;
  after: CellSummary;
  by?: string;
}

export interface TabDiff {
  tab: number;
  name: string;
  renamed_from?: string;
  added?: boolean;
  removed?: boolean;
  rows_added: number;
  rows_removed: number;
  cols_added: number;
  cols_removed: number;
  reordered?: boolean;
  cells_changed: number;
}

export interface WorkbookDiff {
  tabs: TabDiff[];
  cells: CellDiff[];
  cells_changed: number;
  truncated: boolean;
  names_changed?: boolean;
  charts_changed?: boolean;
  accounts?: Record<string, Account>;
}

export interface Conflict {
  tab: number;
  tab_name: string;
  a: string;
  base: CellSummary;
  sheet: CellSummary;
  copy: CellSummary;
  taken: "copy" | "sheet";
}

export interface CopyCompare {
  diff: WorkbookDiff;
  conflicts: Conflict[];
  parent_changed: boolean;
}

export interface MergeRequest {
  tabs?: number[];
  range?: { tab: TabRef; range: string };
  resolutions?: Array<{ tab: TabRef; a: string; take: "copy" | "sheet" }>;
  default?: "copy" | "sheet";
  dry_run?: boolean;
  close?: boolean;
}

export interface MergeAnswer {
  version: number;
  cells_taken: number;
  conflicts: Conflict[];
  tabs_added: string[] | number;
  tabs_removed: string[] | number;
  rows_added: number;
  rows_removed: number;
}

/* ---- comments ---------------------------------------------------------------------------------------------------- */

export interface Reaction {
  emoji: string;
  accounts: Account[];
}

export interface Comment {
  id: string;
  author: Account;
  body: string;
  mentions: Account[];
  reactions: Reaction[];
  created_at: string;
  edited_at: string | null;
}

export interface Thread {
  id: string;
  sheet_id: string;
  copy_id: string | null;
  tab: number;
  tab_name: string;
  range: string | null;
  original_range: string | null;
  quote: string;
  detached: boolean;
  status: "open" | "resolved";
  resolved_by: Account | null;
  resolved_at: string | null;
  comments: Comment[];
  created_at: string;
}

/* ---- search, meta ------------------------------------------------------------------------------------------------ */

export interface SearchResult {
  sheet: Sheet;
  matches: Array<{ tab: number; tab_name: string; a: string; text: string }>;
}

export interface Meta {
  app_id: string;
  version: string;
  accounts_url: string;
  limits: { max_rows: number; max_cols: number; max_upload_bytes: number; max_cells_per_edit: number; code_timeout_seconds: number };
}

/** Download formats (docs/API.md Files). */
export type DownloadFormat = "xlsx" | "xls" | "ods" | "numbers" | "csv" | "tsv" | "json" | "parquet" | "pdf";
