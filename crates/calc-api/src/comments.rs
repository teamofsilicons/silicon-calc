//! Comments: threads on a cell, a range, whole rows or columns, or a whole tab, on the main sheet or on a copy.
//! See docs/API.md, Comments.
//!
//! A thread is anchored by its tab's id and its rows' and columns' ids, never by position, so it stays on its cells
//! as rows and columns are inserted, deleted, moved or sorted around it. It keeps the text its cells showed when it
//! was made (the quote), so a thread whose cells were deleted still shows what it was on (it is then `detached`).
//! Cells that were never written have no ids yet; commenting on them first gives them ids (an `Extend` operation,
//! which changes nothing anyone can see).
//!
//! Mentions (`@c:ada`, `@si:analyst`) must name real accounts. Who hears about what, through Silicon Ting, each
//! ting written in the same transaction as the comment, and each account told once per comment (the most
//! specific one wins):
//! - `calc.mention.received`: the ones mentioned (who can open the sheet);
//! - `calc.comment.replied`: everyone in the thread (who started it, wrote in it or was mentioned in it) but the
//!   replier;
//! - `calc.comment.created`: the sheet's owner, unless they wrote it;
//! - `calc.thread.resolved`: who started the thread, when someone else resolves it.

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use calc_engine::{
    CellPos, Rect, Tab, Workbook,
    addr::{MAX_ROWS, col_index, col_name},
    model::Axis,
};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    accounts::Account,
    auth::{Caller, MaybeCaller},
    db::{rfc3339, rfc3339_opt},
    error::{ApiError, ApiResult, JsonBody, QueryParams},
    lines::{self, LineEvent, LineHandle, LineRow},
    notify,
    perms::{self, Role, SheetAccess},
    sheets,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sheets/{sheet}/comments", get(list_main).post(create_main))
        .route("/sheets/{sheet}/copies/{copy}/comments", get(list_copy).post(create_copy))
        .route("/comments/{id}", get(get_thread).patch(edit_comment).delete(delete_comment))
        .route("/comments/{id}/replies", post(reply))
        .route("/comments/{id}/resolve", post(resolve))
        .route("/comments/{id}/reopen", post(reopen))
        .route("/comments/{id}/reactions", post(react))
        .route("/comments/{id}/reactions/{emoji}", delete(unreact))
}

/// The longest comment.
pub const MAX_BODY: usize = 10_000;
/// A quote holds at most this many cells' text, and this many characters.
const QUOTE_CELLS: usize = 20;
const QUOTE_CHARS: usize = 500;
/// How many cells a quote looks at, at most.
const QUOTE_VISIT: usize = 200_000;
/// Ranges up to this many rows (or columns) remember every row (or column) id, not just the corners.
const ID_LIST_MAX: u32 = 1000;

// ------------------------------------------------------------------------------------------------
// Where a thread is
// ------------------------------------------------------------------------------------------------

/// A tab as a request names it.
#[derive(Clone, Debug)]
enum TabRef {
    /// No tab given: the first.
    First,
    Id(u32),
    /// A name, or a number written as text (a name first, then an id).
    Name(String),
}

impl TabRef {
    fn from_json(v: Option<&Value>) -> ApiResult<Self> {
        let bad = || ApiError::invalid_field("tab", "Name the tab by its id (1) or its name (\"Sales\").");
        match v {
            None | Some(Value::Null) => Ok(Self::First),
            Some(Value::Number(n)) => n.as_u64().and_then(|n| u32::try_from(n).ok()).map(Self::Id).ok_or_else(bad),
            Some(Value::String(s)) if !s.trim().is_empty() => Ok(Self::Name(s.trim().to_owned())),
            _ => Err(bad()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::First => "at all".into(),
            Self::Id(id) => format!("with id {id}"),
            Self::Name(n) => format!("named \"{n}\""),
        }
    }
}

fn find_tab<'a>(wb: &'a Workbook, r: &TabRef) -> ApiResult<&'a Tab> {
    let found = match r {
        TabRef::First => wb.tabs.first(),
        TabRef::Id(id) => wb.tab(*id),
        TabRef::Name(n) => wb.tab_by_name(n).or_else(|| n.parse::<u32>().ok().and_then(|id| wb.tab(id))),
    };
    found.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "no_such_tab",
            format!("This sheet has no tab {}.", r.describe()),
            "List the tabs with GET …/tabs and use one of their ids or names.",
        )
    })
}

/// What a comment is on, by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Tab,
    Rows(u32, u32),
    Cols(u32, u32),
    Cells(Rect),
}

impl Target {
    /// `B7`, `A1:C9`, `B:D`, `3:5`, or nothing (the whole tab). A tab name in front (`Sales!B7`) comes back too.
    fn parse(text: Option<&str>) -> ApiResult<(Option<String>, Self)> {
        let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) else { return Ok((None, Self::Tab)) };
        let bad = || {
            ApiError::invalid_field(
                "range",
                format!(
                    "\"{t}\" isn't something a comment can be on: use a cell (B7), a range (A1:C9), whole columns \
                     (B:D), whole rows (3:5), or no range for the whole tab."
                ),
            )
        };
        let (tab, rest) = calc_engine::addr::split_tab(t);
        let rest = rest.trim().replace('$', "");
        let (a, b) = match rest.split_once(':') {
            Some((a, b)) => (a.trim().to_owned(), b.trim().to_owned()),
            None => (rest.clone(), rest.clone()),
        };
        let letters = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic());
        let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
        let target = if letters(&a) && letters(&b) {
            let (c1, c2) = (col_index(&a).ok_or_else(bad)?, col_index(&b).ok_or_else(bad)?);
            Self::Cols(c1.min(c2), c1.max(c2))
        } else if digits(&a) && digits(&b) {
            let r1: u32 = a.parse().map_err(|_| bad())?;
            let r2: u32 = b.parse().map_err(|_| bad())?;
            if r1 == 0 || r2 == 0 || r1 > MAX_ROWS || r2 > MAX_ROWS {
                return Err(bad());
            }
            Self::Rows(r1.min(r2) - 1, r1.max(r2) - 1)
        } else {
            let p1 = CellPos::parse(&a).ok_or_else(bad)?;
            let p2 = CellPos::parse(&b).ok_or_else(bad)?;
            Self::Cells(Rect::new(p1.row, p1.col, p2.row, p2.col))
        };
        Ok((tab, target))
    }

    fn spans(self) -> (Option<(u32, u32)>, Option<(u32, u32)>) {
        match self {
            Self::Tab => (None, None),
            Self::Rows(a, b) => (Some((a, b)), None),
            Self::Cols(a, b) => (None, Some((a, b))),
            Self::Cells(r) => (Some((r.r1, r.r2)), Some((r.c1, r.c2))),
        }
    }
}

/// A range as text: `B7`, `A1:C9`, `B:D`, `3:5`; None for the whole tab.
fn range_text(rows: Option<(u32, u32)>, cols: Option<(u32, u32)>) -> Option<String> {
    match (rows, cols) {
        (None, None) => None,
        (Some((a, b)), None) => Some(format!("{}:{}", a + 1, b + 1)),
        (None, Some((a, b))) => Some(format!("{}:{}", col_name(a), col_name(b))),
        (Some((r1, r2)), Some((c1, c2))) => Some(Rect::new(r1, c1, r2, c2).to_string()),
    }
}

/// Where a thread is anchored: its tab and the ids of its rows and columns (None on an axis: all of it).
#[derive(Clone, Debug, Default)]
struct Anchor {
    tab: u32,
    r1: Option<u32>,
    r2: Option<u32>,
    c1: Option<u32>,
    c2: Option<u32>,
    row_ids: Option<Vec<u32>>,
    col_ids: Option<Vec<u32>>,
}

/// Where a thread is now.
#[derive(Clone, Debug)]
struct Located {
    tab_name: Option<String>,
    tab_index: Option<usize>,
    range: Option<String>,
    detached: bool,
    /// Top-left position, for ordering.
    top: (u32, u32),
}

enum Span {
    All,
    At(u32, u32),
    Gone,
}

fn span(axis: &Axis, all: Option<&[u32]>, first: Option<u32>, last: Option<u32>) -> Span {
    if let Some(ids) = all.filter(|ids| !ids.is_empty()) {
        let ps: Vec<u32> = ids.iter().filter_map(|&i| axis.pos_of(i)).collect();
        return match (ps.iter().min(), ps.iter().max()) {
            (Some(&a), Some(&b)) => Span::At(a, b),
            _ => Span::Gone,
        };
    }
    match (first, last) {
        (Some(f), Some(l)) => match (axis.pos_of(f), axis.pos_of(l)) {
            (Some(a), Some(b)) => Span::At(a.min(b), a.max(b)),
            _ => Span::Gone,
        },
        _ => Span::All,
    }
}

fn locate(wb: &Workbook, a: &Anchor) -> Located {
    let Some((index, tab)) = wb.tabs.iter().enumerate().find(|(_, t)| t.id == a.tab) else {
        return Located { tab_name: None, tab_index: None, range: None, detached: true, top: (0, 0) };
    };
    let rows = span(&tab.rows, a.row_ids.as_deref(), a.r1, a.r2);
    let cols = span(&tab.cols, a.col_ids.as_deref(), a.c1, a.c2);
    let (rows, cols) = match (rows, cols) {
        (Span::Gone, _) | (_, Span::Gone) => {
            return Located { tab_name: Some(tab.name.clone()), tab_index: Some(index), range: None, detached: true, top: (u32::MAX, u32::MAX) };
        }
        (r, c) => (
            match r {
                Span::At(x, y) => Some((x, y)),
                _ => None,
            },
            match c {
                Span::At(x, y) => Some((x, y)),
                _ => None,
            },
        ),
    };
    Located {
        tab_name: Some(tab.name.clone()),
        tab_index: Some(index),
        range: range_text(rows, cols),
        detached: false,
        top: (rows.map_or(0, |r| r.0), cols.map_or(0, |c| c.0)),
    }
}

/// The text the cells show, `1,200 | 900 | …`, row by row, skipping empty cells.
fn quote(wb: &Workbook, tab: &Tab, rows: Option<(u32, u32)>, cols: Option<(u32, u32)>) -> String {
    if rows.is_none() && cols.is_none() {
        return String::new();
    }
    let (Some(last_row), Some(last_col)) = (tab.rows.len().checked_sub(1), tab.cols.len().checked_sub(1)) else {
        return String::new();
    };
    let (r1, r2) = rows.unwrap_or((0, last_row));
    let (c1, c2) = cols.unwrap_or((0, last_col));
    let (r2, c2) = (r2.min(last_row), c2.min(last_col));
    let mut parts: Vec<String> = Vec::new();
    let (mut chars, mut visited, mut more) = (0usize, 0usize, false);
    'rows: for r in r1..=r2 {
        for c in c1..=c2 {
            visited += 1;
            if visited > QUOTE_VISIT {
                more = true;
                break 'rows;
            }
            let cell = tab.cell_at(CellPos::new(r, c));
            if cell.value.is_empty() {
                continue;
            }
            let shown = calc_engine::numfmt::format_value(&cell.value, wb.style_of(cell.style).num_fmt.as_deref()).0;
            let shown: String = shown.chars().map(|ch| if ch.is_control() { ' ' } else { ch }).collect();
            if parts.len() == QUOTE_CELLS || chars + shown.chars().count() > QUOTE_CHARS {
                more = true;
                break 'rows;
            }
            chars += shown.chars().count();
            parts.push(shown);
        }
    }
    let mut q = parts.join(" | ");
    if more {
        q.push_str(if q.is_empty() { "…" } else { " | …" });
    }
    q
}

/// Where a new thread goes.
#[derive(Clone, Debug)]
struct Placed {
    anchor: Anchor,
    tab_name: String,
    range: Option<String>,
    quote: String,
}

enum Placing {
    Ready(Placed),
    /// Some of the cells have no ids yet: the tab needs this many rows and columns of ids first.
    Extend { tab: u32, rows: u32, cols: u32 },
}

fn place(wb: &Workbook, tab: &TabRef, target: Target) -> ApiResult<Placing> {
    let t = find_tab(wb, tab)?;
    let (rows, cols) = target.spans();
    let need_rows = rows.map_or(0, |(_, b)| b + 1);
    let need_cols = cols.map_or(0, |(_, b)| b + 1);
    if need_rows > t.rows.len() || need_cols > t.cols.len() {
        return Ok(Placing::Extend { tab: t.id, rows: need_rows, cols: need_cols });
    }
    let side = |axis: &Axis, s: Option<(u32, u32)>| -> (Option<u32>, Option<u32>, Option<Vec<u32>>) {
        match s {
            None => (None, None, None),
            Some((a, b)) => {
                let all = (b - a < ID_LIST_MAX).then(|| (a..=b).filter_map(|p| axis.id_at(p)).collect());
                (axis.id_at(a), axis.id_at(b), all)
            }
        }
    };
    let (r1, r2, row_ids) = side(&t.rows, rows);
    let (c1, c2, col_ids) = side(&t.cols, cols);
    Ok(Placing::Ready(Placed {
        anchor: Anchor { tab: t.id, r1, r2, c1, c2, row_ids, col_ids },
        tab_name: t.name.clone(),
        range: range_text(rows, cols),
        quote: quote(wb, t, rows, cols),
    }))
}

/// Places a new thread on the line, giving never-written cells ids first if they need them.
async fn place_on_line(handle: &LineHandle, caller: &Caller, tab: TabRef, target: Target) -> ApiResult<Placed> {
    let (t, g) = (tab.clone(), target);
    match handle.read(move |wb, _| place(wb, &t, g)).await?? {
        Placing::Ready(p) => Ok(p),
        Placing::Extend { tab: tab_id, rows, cols } => {
            let place_text = range_text(target.spans().0, target.spans().1).unwrap_or_default();
            handle
                .apply(lines::ApplyRequest {
                    author: caller.uuid,
                    kind: lines::OpKind::Edit,
                    summary: Some(format!("{} commented on {place_text}", caller.id)),
                    base_version: None,
                    client_id: None,
                    build: Box::new(move |wb: &Workbook| {
                        let Some(t) = wb.tab(tab_id) else {
                            return Err(ApiError::new(
                                StatusCode::NOT_FOUND,
                                "no_such_tab",
                                format!("The tab with id {tab_id} was deleted just now."),
                                "List the tabs with GET …/tabs and comment on one of them.",
                            ));
                        };
                        let (r, c) = (rows.max(t.rows.len()), cols.max(t.cols.len()));
                        if r == t.rows.len() && c == t.cols.len() {
                            return Ok(None);
                        }
                        Ok(Some(calc_engine::Op::Extend { tab: tab_id, rows: r, cols: c }))
                    }),
                })
                .await?;
            match handle.read(move |wb, _| place(wb, &tab, target)).await?? {
                Placing::Ready(p) => Ok(p),
                Placing::Extend { .. } => Err(ApiError::internal("cells still had no ids after extending the tab")),
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Rows
// ------------------------------------------------------------------------------------------------

const THREAD_COLUMNS: &str = "id, sheet_id, line_id, tab_id, r1, c1, r2, c2, row_ids, col_ids, original_range, quote, \
                              tab_name, status, resolved_by, resolved_at, created_by, created_at";

#[derive(Clone, Debug, sqlx::FromRow)]
struct ThreadRow {
    id: Uuid,
    sheet_id: String,
    line_id: String,
    tab_id: i32,
    r1: Option<i32>,
    c1: Option<i32>,
    r2: Option<i32>,
    c2: Option<i32>,
    row_ids: Option<Vec<i32>>,
    col_ids: Option<Vec<i32>>,
    original_range: Option<String>,
    quote: String,
    tab_name: String,
    status: String,
    resolved_by: Option<Uuid>,
    resolved_at: Option<OffsetDateTime>,
    created_by: Uuid,
    created_at: OffsetDateTime,
}

impl ThreadRow {
    fn anchor(&self) -> Anchor {
        let u = |v: Option<i32>| v.and_then(|v| u32::try_from(v).ok());
        let list = |v: &Option<Vec<i32>>| v.as_ref().map(|l| l.iter().filter_map(|&i| u32::try_from(i).ok()).collect());
        Anchor {
            tab: u32::try_from(self.tab_id).unwrap_or(u32::MAX),
            r1: u(self.r1),
            r2: u(self.r2),
            c1: u(self.c1),
            c2: u(self.c2),
            row_ids: list(&self.row_ids),
            col_ids: list(&self.col_ids),
        }
    }
}

const COMMENT_COLUMNS: &str = "id, thread_id, author, body, mentions, created_at, edited_at";

#[derive(Clone, Debug, sqlx::FromRow)]
struct CommentRow {
    id: Uuid,
    thread_id: Uuid,
    author: Uuid,
    body: String,
    mentions: Vec<Uuid>,
    created_at: OffsetDateTime,
    edited_at: Option<OffsetDateTime>,
}

async fn thread_row(state: &AppState, id: Uuid) -> ApiResult<Option<ThreadRow>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("select {THREAD_COLUMNS} from threads where id = $1")))
        .bind(id)
        .fetch_optional(&state.db)
        .await?)
}

async fn comment_row(state: &AppState, id: Uuid) -> ApiResult<Option<CommentRow>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {COMMENT_COLUMNS} from comments where id = $1 and deleted_at is null"
    )))
    .bind(id)
    .fetch_optional(&state.db)
    .await?)
}

fn to_i32(v: Option<u32>) -> Option<i32> {
    v.and_then(|v| i32::try_from(v).ok())
}

fn to_i32s(v: &Option<Vec<u32>>) -> Option<Vec<i32>> {
    v.as_ref().map(|l| l.iter().filter_map(|&i| i32::try_from(i).ok()).collect())
}

// ------------------------------------------------------------------------------------------------
// The sheet, the line, and who may do what
// ------------------------------------------------------------------------------------------------

struct Ctx {
    access: SheetAccess,
    line: LineRow,
}

impl Ctx {
    fn copy_id(&self) -> Option<&str> {
        (!self.line.is_main).then_some(self.line.id.as_str())
    }

    /// Commenting, replying, reacting, resolving: commenters and up, outside the trash, on a line still open.
    fn require_write(&self) -> ApiResult<()> {
        self.access.require_comment()?;
        self.access.require_live()?;
        if self.line.status != "open" {
            return Err(ApiError::conflict(
                format!(
                    "The copy \"{}\" was {}, so its comments can be read but not added to.",
                    self.line.name.clone().unwrap_or_default(),
                    self.line.status
                ),
                "Comment on the sheet it was merged into, or on another copy.",
            ));
        }
        Ok(())
    }
}

async fn ctx_for(state: &AppState, caller: Option<&Caller>, sheet: &str, copy: Option<&str>) -> ApiResult<Ctx> {
    let access = perms::sheet_access(state, caller, sheet).await?;
    let line = lines::line_for(state, &access.sheet.id, copy).await?;
    Ok(Ctx { access, line })
}

/// A thread and its context, as long as the caller can open its sheet.
async fn thread_ctx(state: &AppState, caller: Option<&Caller>, thread: Uuid) -> ApiResult<(Ctx, ThreadRow)> {
    let Some(t) = thread_row(state, thread).await? else { return Err(ApiError::not_found("This comment thread")) };
    let access = perms::sheet_access(state, caller, &t.sheet_id).await.map_err(|e| {
        if e.code == "not_found" { ApiError::not_found("This comment thread") } else { e }
    })?;
    let copy = (t.line_id != crate::ids::main_line(&t.sheet_id)).then(|| t.line_id.clone());
    let line = lines::line_for(state, &t.sheet_id, copy.as_deref()).await?;
    Ok((Ctx { access, line }, t))
}

fn parse_uuid(id: &str, what: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(id.trim()).map_err(|_| ApiError::not_found(what.to_owned()))
}

// ------------------------------------------------------------------------------------------------
// Bodies and mentions
// ------------------------------------------------------------------------------------------------

fn clean_body(body: Option<&str>) -> ApiResult<String> {
    let b = body.map(str::trim).unwrap_or("");
    if b.is_empty() {
        return Err(ApiError::invalid_field("body", "A comment needs some text."));
    }
    if b.chars().count() > MAX_BODY {
        return Err(ApiError::invalid_field("body", format!("A comment can be at most {MAX_BODY} characters.")));
    }
    Ok(b.to_owned())
}

/// The ids a comment mentions (`@c:ada`, `@si:analyst`), lowercased, each once, in order.
pub fn mention_ids(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let is_id_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && (i == 0 || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '.' | '_' | '-' | '@'))) {
            let rest: String = chars[i + 1..].iter().take(80).collect();
            let lower = rest.to_ascii_lowercase();
            for prefix in ["c:", "si:"] {
                if let Some(after) = lower.strip_prefix(prefix) {
                    let handle: String = after.chars().take_while(|&c| is_id_char(c)).collect();
                    let handle = handle.trim_end_matches(['.', '_', '-']);
                    if !handle.is_empty() {
                        let id = format!("{prefix}{handle}");
                        if !out.contains(&id) {
                            out.push(id);
                        }
                    }
                }
            }
        }
        i += 1;
    }
    out
}

/// The accounts a comment mentions; 422 naming any id no account has.
async fn resolve_mentions(state: &AppState, body: &str) -> ApiResult<Vec<Account>> {
    let mut found = Vec::new();
    let mut unknown = Vec::new();
    for id in mention_ids(body) {
        match state.accounts.by_public_id(&id).await {
            Ok(Some(a)) => {
                if !found.iter().any(|f: &Account| f.uuid == a.uuid) {
                    found.push(a);
                }
            }
            Ok(None) => unknown.push(id),
            Err(e) if e.code == "invalid_field" => unknown.push(id),
            Err(e) => return Err(e),
        }
    }
    if !unknown.is_empty() {
        let list = unknown.iter().map(|u| format!("@{u}")).collect::<Vec<_>>().join(", ");
        let message = if unknown.len() == 1 {
            format!("No Carbon or Silicon has the id {list}, so it can't be mentioned.")
        } else {
            format!("No Carbon or Silicon has these ids, so they can't be mentioned: {list}.")
        };
        return Err(ApiError::invalid_field("body", message.clone())
            .with_hint("Check the ids (calc accounts lookup c:…), or take the mentions out.")
            .with_details(json!({"fields": {"body": message}, "unknown": unknown})));
    }
    Ok(found)
}

// ------------------------------------------------------------------------------------------------
// JSON
// ------------------------------------------------------------------------------------------------

type Reaction = (Uuid, Uuid, String);

fn comment_json(c: &CommentRow, reactions: &[Reaction], acct: &dyn Fn(Uuid) -> Account) -> Value {
    let mut grouped: Vec<(String, Vec<Account>)> = Vec::new();
    for (comment, account, emoji) in reactions.iter().filter(|r| r.0 == c.id) {
        let _ = comment;
        match grouped.iter_mut().find(|(e, _)| e == emoji) {
            Some((_, accounts)) => accounts.push(acct(*account)),
            None => grouped.push((emoji.clone(), vec![acct(*account)])),
        }
    }
    json!({
        "id": c.id,
        "thread_id": c.thread_id,
        "author": acct(c.author),
        "body": c.body,
        "mentions": c.mentions.iter().map(|&m| acct(m)).collect::<Vec<_>>(),
        "reactions": grouped.into_iter().map(|(emoji, accounts)| json!({"emoji": emoji, "accounts": accounts})).collect::<Vec<_>>(),
        "created_at": rfc3339(c.created_at),
        "edited_at": rfc3339_opt(c.edited_at),
    })
}

/// docs/API.md `Thread` for each row (all on `ctx`'s line), with where it is now.
async fn threads_json(state: &AppState, ctx: &Ctx, rows: &[ThreadRow], located: &[Located]) -> ApiResult<Vec<Value>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().map(|t| t.id).collect();
    let comments: Vec<CommentRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {COMMENT_COLUMNS} from comments where thread_id = any($1) and deleted_at is null order by created_at, id"
    )))
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let comment_ids: Vec<Uuid> = comments.iter().map(|c| c.id).collect();
    let reactions: Vec<Reaction> = sqlx::query_as(
        "select comment_id, account, emoji from reactions where comment_id = any($1) order by created_at, emoji",
    )
    .bind(&comment_ids)
    .fetch_all(&state.db)
    .await?;
    let mut uuids: Vec<Uuid> = Vec::new();
    for t in rows {
        uuids.push(t.created_by);
        uuids.extend(t.resolved_by);
    }
    for c in &comments {
        uuids.push(c.author);
        uuids.extend(c.mentions.iter().copied());
    }
    uuids.extend(reactions.iter().map(|r| r.1));
    let accounts = state.accounts.get_map(uuids).await;
    let acct = |u: Uuid| accounts.get(&u).cloned().unwrap_or_else(|| Account::unknown(u));
    let copy_id = ctx.copy_id();
    Ok(rows
        .iter()
        .zip(located)
        .map(|(t, at)| {
            let tab = u32::try_from(t.tab_id).unwrap_or(0);
            json!({
                "id": t.id,
                "sheet_id": t.sheet_id,
                "copy_id": copy_id,
                "tab": tab,
                "tab_name": at.tab_name.clone().unwrap_or_else(|| t.tab_name.clone()),
                "range": at.range,
                "original_range": t.original_range,
                "quote": t.quote,
                "detached": at.detached,
                "status": t.status,
                "resolved_by": t.resolved_by.map(acct),
                "resolved_at": rfc3339_opt(t.resolved_at),
                "comments": comments.iter().filter(|c| c.thread_id == t.id).map(|c| comment_json(c, &reactions, &acct)).collect::<Vec<_>>(),
                "created_at": rfc3339(t.created_at),
                "created_by": acct(t.created_by),
                "url": state.cfg.sheet_place_url(&t.sheet_id, copy_id, Some(tab), at.range.as_deref(), Some(&t.id.to_string())),
            })
        })
        .collect())
}

/// Where each thread is now, read from the line.
async fn locate_all(handle: &LineHandle, rows: &[ThreadRow]) -> ApiResult<Vec<Located>> {
    let anchors: Vec<Anchor> = rows.iter().map(ThreadRow::anchor).collect();
    handle.read(move |wb, _| anchors.iter().map(|a| locate(wb, a)).collect()).await
}

/// One thread as JSON, read fresh.
async fn one_thread(state: &AppState, ctx: &Ctx, handle: &LineHandle, id: Uuid) -> ApiResult<Value> {
    let Some(t) = thread_row(state, id).await? else { return Err(ApiError::not_found("This comment thread")) };
    let rows = vec![t];
    let located = locate_all(handle, &rows).await?;
    Ok(threads_json(state, ctx, &rows, &located).await?.pop().unwrap_or(Value::Null))
}

/// Tells everyone watching the line about the thread.
fn tell_viewers(handle: &LineHandle, thread: &Value) {
    handle.publish(LineEvent::Comment(json!({"thread": thread})));
}

// ------------------------------------------------------------------------------------------------
// Tings
// ------------------------------------------------------------------------------------------------

/// What every comment ting carries.
struct TingCtx {
    sheet_id: String,
    sheet_title: String,
    copy: Option<(String, String)>,
    tab: u32,
    tab_name: String,
    range: Option<String>,
    quote: String,
    thread_id: Uuid,
    url: String,
}

impl TingCtx {
    fn new(state: &AppState, ctx: &Ctx, t: &ThreadRow, at: &Located) -> Self {
        let tab = u32::try_from(t.tab_id).unwrap_or(0);
        Self {
            sheet_id: ctx.access.sheet.id.clone(),
            sheet_title: ctx.access.sheet.title.clone(),
            copy: (!ctx.line.is_main).then(|| (ctx.line.id.clone(), ctx.line.name.clone().unwrap_or_default())),
            tab,
            tab_name: at.tab_name.clone().unwrap_or_else(|| t.tab_name.clone()),
            range: at.range.clone(),
            quote: t.quote.clone(),
            thread_id: t.id,
            url: state.cfg.sheet_place_url(&t.sheet_id, ctx.copy_id(), Some(tab), at.range.as_deref(), Some(&t.id.to_string())),
        }
    }

    fn data(&self, comment: &CommentRow, author: &Account) -> Value {
        json!({
            "sheet": {"id": self.sheet_id, "title": self.sheet_title},
            "copy": self.copy.as_ref().map(|(id, name)| json!({"id": id, "name": name})),
            "tab": {"id": self.tab, "name": self.tab_name},
            "range": self.range,
            "quote": self.quote,
            "thread_id": self.thread_id,
            "comment": {"id": comment.id, "body": comment.body, "author": author},
            "url": self.url,
            "cli": format!("calc comments show {}", self.thread_id),
        })
    }
}

/// Who to tell about a new comment, and the mentioned who can't open the sheet (they aren't told; the answer lists
/// them so the author can share it with them).
async fn recipients(
    state: &AppState,
    sheet: &perms::SheetRow,
    author: Uuid,
    mentioned: &[Account],
    participants: Option<&[Uuid]>,
) -> ApiResult<(Vec<(Uuid, &'static str)>, Vec<Account>)> {
    let mut told: HashSet<Uuid> = HashSet::from([author]);
    let mut out = Vec::new();
    let mut without = Vec::new();
    for m in mentioned {
        if told.contains(&m.uuid) {
            continue;
        }
        told.insert(m.uuid);
        if perms::sheet_role_of(state, m.uuid, &sheet.id).await?.is_some() {
            out.push((m.uuid, "calc.mention.received"));
        } else {
            without.push(m.clone());
        }
    }
    if let Some(participants) = participants {
        for &p in participants {
            if told.insert(p) && perms::sheet_role_of(state, p, &sheet.id).await?.is_some() {
                out.push((p, "calc.comment.replied"));
            }
        }
    }
    if told.insert(sheet.owner) {
        out.push((sheet.owner, "calc.comment.created"));
    }
    Ok((out, without))
}

async fn queue_comment_tings(
    tx: &mut sqlx::PgConnection,
    tc: &TingCtx,
    comment: &CommentRow,
    author: &Account,
    to: &[(Uuid, &'static str)],
) -> ApiResult<()> {
    let data = tc.data(comment, author);
    for (recipient, kind) in to {
        let key = format!("{kind}:{}:{recipient}", comment.id);
        notify::enqueue(tx, kind, *recipient, &key, data.clone()).await?;
    }
    Ok(())
}

/// Everyone in a thread: who started it, who wrote in it and who was mentioned in it.
async fn participants(state: &AppState, t: &ThreadRow) -> ApiResult<Vec<Uuid>> {
    let rows: Vec<(Uuid, Vec<Uuid>)> =
        sqlx::query_as("select author, mentions from comments where thread_id = $1 and deleted_at is null order by created_at")
            .bind(t.id)
            .fetch_all(&state.db)
            .await?;
    let mut out = vec![t.created_by];
    for (author, mentions) in rows {
        for u in std::iter::once(author).chain(mentions) {
            if !out.contains(&u) {
                out.push(u);
            }
        }
    }
    Ok(out)
}

fn with_unnotified(mut v: Value, without: &[Account]) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.insert("mentioned_without_access".into(), json!(without));
    }
    v
}

// ------------------------------------------------------------------------------------------------
// Routes on a line
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    tab: Option<String>,
}

async fn list(state: AppState, caller: Option<Caller>, sheet: String, copy: Option<String>, q: ListQuery) -> ApiResult<Json<Value>> {
    let ctx = ctx_for(&state, caller.as_ref(), &sheet, copy.as_deref()).await?;
    let status = match q.status.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("open") {
        "all" => None,
        s @ ("open" | "resolved") => Some(s.to_owned()),
        other => return Err(ApiError::invalid_field("status", format!("There is no status \"{other}\"; use open, resolved or all."))),
    };
    let rows: Vec<ThreadRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {THREAD_COLUMNS} from threads where line_id = $1 and ($2::text is null or status = $2) order by created_at, id"
    )))
    .bind(&ctx.line.id)
    .bind(&status)
    .fetch_all(&state.db)
    .await?;
    if rows.is_empty() {
        return Ok(Json(json!({"threads": []})));
    }
    let tab_filter = q.tab.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| TabRef::Name(t.to_owned()));
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    let anchors: Vec<Anchor> = rows.iter().map(ThreadRow::anchor).collect();
    let (located, tab_id) = handle
        .read(move |wb, _| {
            let tab_id = match &tab_filter {
                Some(r) => Some(find_tab(wb, r).map(|t| t.id)),
                None => None,
            };
            (anchors.iter().map(|a| locate(wb, a)).collect::<Vec<_>>(), tab_id)
        })
        .await?;
    let tab_id = tab_id.transpose()?;
    let mut picked: Vec<(ThreadRow, Located)> = rows
        .into_iter()
        .zip(located)
        .filter(|(t, _)| tab_id.is_none_or(|id| u32::try_from(t.tab_id).ok() == Some(id)))
        .collect();
    // In sheet order: by tab, then position; detached threads after the rest of their tab.
    picked.sort_by_key(|(t, at)| (at.tab_index.unwrap_or(usize::MAX), at.detached, at.top, t.created_at));
    let (rows, located): (Vec<ThreadRow>, Vec<Located>) = picked.into_iter().unzip();
    Ok(Json(json!({"threads": threads_json(&state, &ctx, &rows, &located).await?})))
}

async fn list_main(State(state): State<AppState>, MaybeCaller(caller): MaybeCaller, Path(sheet): Path<String>, QueryParams(q): QueryParams<ListQuery>) -> ApiResult<Json<Value>> {
    list(state, caller, sheet, None, q).await
}

async fn list_copy(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path((sheet, copy)): Path<(String, String)>,
    QueryParams(q): QueryParams<ListQuery>,
) -> ApiResult<Json<Value>> {
    list(state, caller, sheet, Some(copy), q).await
}

#[derive(Deserialize)]
struct NewThread {
    tab: Option<Value>,
    range: Option<String>,
    body: Option<String>,
}

async fn create(state: AppState, caller: Caller, sheet: String, copy: Option<String>, body: NewThread) -> ApiResult<(StatusCode, Json<Value>)> {
    let ctx = ctx_for(&state, Some(&caller), &sheet, copy.as_deref()).await?;
    ctx.require_write()?;
    let text = clean_body(body.body.as_deref())?;
    let (tab_in_range, target) = Target::parse(body.range.as_deref())?;
    let mut tab = TabRef::from_json(body.tab.as_ref())?;
    if let Some(name) = tab_in_range {
        match &tab {
            TabRef::First => tab = TabRef::Name(name),
            TabRef::Name(n) if n.eq_ignore_ascii_case(&name) => {}
            _ => {
                return Err(ApiError::invalid_field(
                    "range",
                    format!("The range names the tab \"{name}\" but the request's tab is another one."),
                ));
            }
        }
    }
    let mentioned = resolve_mentions(&state, &text).await?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    let placed = place_on_line(&handle, &caller, tab, target).await?;
    let author = sheets::caller_account(&state, &caller).await;
    let (to, without) = recipients(&state, &ctx.access.sheet, caller.uuid, &mentioned, None).await?;
    let thread_id = Uuid::now_v7();
    let now = OffsetDateTime::now_utc();
    let comment = CommentRow {
        id: Uuid::now_v7(),
        thread_id,
        author: caller.uuid,
        body: text,
        mentions: mentioned.iter().map(|m| m.uuid).collect(),
        created_at: now,
        edited_at: None,
    };
    let a = &placed.anchor;
    let row = ThreadRow {
        id: thread_id,
        sheet_id: ctx.access.sheet.id.clone(),
        line_id: ctx.line.id.clone(),
        tab_id: i32::try_from(a.tab).map_err(ApiError::internal)?,
        r1: to_i32(a.r1),
        c1: to_i32(a.c1),
        r2: to_i32(a.r2),
        c2: to_i32(a.c2),
        row_ids: to_i32s(&a.row_ids),
        col_ids: to_i32s(&a.col_ids),
        original_range: placed.range.clone(),
        quote: placed.quote.clone(),
        tab_name: placed.tab_name.clone(),
        status: "open".into(),
        resolved_by: None,
        resolved_at: None,
        created_by: caller.uuid,
        created_at: now,
    };
    let at = Located {
        tab_name: Some(placed.tab_name.clone()),
        tab_index: None,
        range: placed.range.clone(),
        detached: false,
        top: (0, 0),
    };
    let tc = TingCtx::new(&state, &ctx, &row, &at);
    let mut tx = state.db.begin().await?;
    sqlx::query(
        "insert into threads (id, sheet_id, line_id, tab_id, r1, c1, r2, c2, row_ids, col_ids, original_range, quote,
                              tab_name, status, created_by, created_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'open', $14, $15)",
    )
    .bind(row.id)
    .bind(&row.sheet_id)
    .bind(&row.line_id)
    .bind(row.tab_id)
    .bind(row.r1)
    .bind(row.c1)
    .bind(row.r2)
    .bind(row.c2)
    .bind(&row.row_ids)
    .bind(&row.col_ids)
    .bind(&row.original_range)
    .bind(&row.quote)
    .bind(&row.tab_name)
    .bind(row.created_by)
    .bind(row.created_at)
    .execute(&mut *tx)
    .await?;
    insert_comment(&mut tx, &comment).await?;
    queue_comment_tings(&mut tx, &tc, &comment, &author, &to).await?;
    tx.commit().await?;
    state.notify.wake();
    let thread = one_thread(&state, &ctx, &handle, thread_id).await?;
    tell_viewers(&handle, &thread);
    Ok((StatusCode::CREATED, Json(with_unnotified(thread, &without))))
}

async fn insert_comment(tx: &mut sqlx::PgConnection, c: &CommentRow) -> ApiResult<()> {
    sqlx::query("insert into comments (id, thread_id, author, body, mentions, created_at) values ($1, $2, $3, $4, $5, $6)")
        .bind(c.id)
        .bind(c.thread_id)
        .bind(c.author)
        .bind(&c.body)
        .bind(&c.mentions)
        .bind(c.created_at)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

async fn create_main(State(state): State<AppState>, caller: Caller, Path(sheet): Path<String>, JsonBody(body): JsonBody<NewThread>) -> ApiResult<(StatusCode, Json<Value>)> {
    create(state, caller, sheet, None, body).await
}

async fn create_copy(
    State(state): State<AppState>,
    caller: Caller,
    Path((sheet, copy)): Path<(String, String)>,
    JsonBody(body): JsonBody<NewThread>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    create(state, caller, sheet, Some(copy), body).await
}

// ------------------------------------------------------------------------------------------------
// Routes on a thread or a comment
// ------------------------------------------------------------------------------------------------

/// `GET /v1/comments/{id}`: a thread by its id (or by the id of a comment in it), for `calc comments show`.
async fn get_thread(State(state): State<AppState>, MaybeCaller(caller): MaybeCaller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let id = parse_uuid(&id, "This comment thread")?;
    let thread_id = match thread_row(&state, id).await? {
        Some(t) => t.id,
        None => match comment_row(&state, id).await? {
            Some(c) => c.thread_id,
            None => return Err(ApiError::not_found("This comment thread")),
        },
    };
    let (ctx, _) = thread_ctx(&state, caller.as_ref(), thread_id).await?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    Ok(Json(one_thread(&state, &ctx, &handle, thread_id).await?))
}

#[derive(Deserialize)]
struct BodyOnly {
    body: Option<String>,
}

async fn reply(State(state): State<AppState>, caller: Caller, Path(id): Path<String>, JsonBody(body): JsonBody<BodyOnly>) -> ApiResult<(StatusCode, Json<Value>)> {
    let thread_id = parse_uuid(&id, "This comment thread")?;
    let (ctx, t) = thread_ctx(&state, Some(&caller), thread_id).await?;
    ctx.require_write()?;
    let text = clean_body(body.body.as_deref())?;
    let mentioned = resolve_mentions(&state, &text).await?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    let in_thread = participants(&state, &t).await?;
    let (to, without) = recipients(&state, &ctx.access.sheet, caller.uuid, &mentioned, Some(&in_thread)).await?;
    let author = sheets::caller_account(&state, &caller).await;
    let comment = CommentRow {
        id: Uuid::now_v7(),
        thread_id,
        author: caller.uuid,
        body: text,
        mentions: mentioned.iter().map(|m| m.uuid).collect(),
        created_at: OffsetDateTime::now_utc(),
        edited_at: None,
    };
    let at = locate_all(&handle, std::slice::from_ref(&t)).await?.pop().ok_or_else(|| ApiError::internal("no place for a thread"))?;
    let tc = TingCtx::new(&state, &ctx, &t, &at);
    let mut tx = state.db.begin().await?;
    insert_comment(&mut tx, &comment).await?;
    queue_comment_tings(&mut tx, &tc, &comment, &author, &to).await?;
    tx.commit().await?;
    state.notify.wake();
    let thread = one_thread(&state, &ctx, &handle, thread_id).await?;
    tell_viewers(&handle, &thread);
    let reply = thread["comments"].as_array().and_then(|cs| cs.iter().find(|c| c["id"] == json!(comment.id)).cloned());
    let reply = reply.unwrap_or_else(|| comment_json(&comment, &[], &|u| if u == author.uuid { author.clone() } else { Account::unknown(u) }));
    Ok((StatusCode::CREATED, Json(with_unnotified(reply, &without))))
}

/// A live comment, its thread and its context.
async fn comment_ctx(state: &AppState, caller: &Caller, id: &str) -> ApiResult<(Ctx, ThreadRow, CommentRow)> {
    let id = parse_uuid(id, "This comment")?;
    let Some(c) = comment_row(state, id).await? else { return Err(ApiError::not_found("This comment")) };
    let (ctx, t) = thread_ctx(state, Some(caller), c.thread_id).await.map_err(|e| {
        if e.code == "not_found" { ApiError::not_found("This comment") } else { e }
    })?;
    Ok((ctx, t, c))
}

/// The comment as JSON, from the thread read fresh (and the thread, to tell viewers).
async fn comment_answer(state: &AppState, ctx: &Ctx, handle: &LineHandle, thread_id: Uuid, comment_id: Uuid) -> ApiResult<(Value, Value)> {
    let thread = one_thread(state, ctx, handle, thread_id).await?;
    let comment = thread["comments"]
        .as_array()
        .and_then(|cs| cs.iter().find(|c| c["id"] == json!(comment_id)).cloned())
        .unwrap_or(Value::Null);
    Ok((comment, thread))
}

async fn edit_comment(State(state): State<AppState>, caller: Caller, Path(id): Path<String>, JsonBody(body): JsonBody<BodyOnly>) -> ApiResult<Json<Value>> {
    let (ctx, t, c) = comment_ctx(&state, &caller, &id).await?;
    ctx.require_write()?;
    if c.author != caller.uuid {
        return Err(ApiError::not_allowed("Only a comment's author can edit it.", "Reply to it instead."));
    }
    let text = clean_body(body.body.as_deref())?;
    let mentioned = resolve_mentions(&state, &text).await?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    // Only the newly mentioned hear about it.
    let fresh: Vec<Account> = mentioned.iter().filter(|m| !c.mentions.contains(&m.uuid)).cloned().collect();
    let mut to = Vec::new();
    let mut without = Vec::new();
    for m in &fresh {
        if m.uuid == caller.uuid {
            continue;
        }
        if perms::sheet_role_of(&state, m.uuid, &ctx.access.sheet.id).await?.is_some() {
            to.push((m.uuid, "calc.mention.received"));
        } else {
            without.push(m.clone());
        }
    }
    let author = sheets::caller_account(&state, &caller).await;
    let updated = CommentRow { body: text, mentions: mentioned.iter().map(|m| m.uuid).collect(), ..c.clone() };
    let at = locate_all(&handle, std::slice::from_ref(&t)).await?.pop().ok_or_else(|| ApiError::internal("no place for a thread"))?;
    let tc = TingCtx::new(&state, &ctx, &t, &at);
    let mut tx = state.db.begin().await?;
    sqlx::query("update comments set body = $2, mentions = $3, edited_at = now() where id = $1")
        .bind(c.id)
        .bind(&updated.body)
        .bind(&updated.mentions)
        .execute(&mut *tx)
        .await?;
    queue_comment_tings(&mut tx, &tc, &updated, &author, &to).await?;
    tx.commit().await?;
    state.notify.wake();
    let (comment, thread) = comment_answer(&state, &ctx, &handle, t.id, c.id).await?;
    tell_viewers(&handle, &thread);
    Ok(Json(with_unnotified(comment, &without)))
}

async fn delete_comment(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let (ctx, t, c) = comment_ctx(&state, &caller, &id).await?;
    if c.author != caller.uuid && ctx.access.role != Role::Owner {
        return Err(ApiError::not_allowed(
            format!("Only a comment's author, or the owner of \"{}\", can delete it.", ctx.access.sheet.title),
            "Resolve the thread instead, or ask its author.",
        ));
    }
    ctx.access.require_live()?;
    sqlx::query("update comments set deleted_at = now() where id = $1 and deleted_at is null")
        .bind(c.id)
        .execute(&state.db)
        .await?;
    if let Ok(handle) = state.lines.open(&state, &ctx.line.id).await {
        match one_thread(&state, &ctx, &handle, t.id).await {
            Ok(thread) => tell_viewers(&handle, &thread),
            Err(e) => tracing::warn!(error = %e.message, "telling viewers about a deleted comment failed"),
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn set_status(state: AppState, caller: Caller, id: String, resolve: bool) -> ApiResult<Json<Value>> {
    let thread_id = parse_uuid(&id, "This comment thread")?;
    let (ctx, t) = thread_ctx(&state, Some(&caller), thread_id).await?;
    ctx.require_write()?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    let want = if resolve { "resolved" } else { "open" };
    if t.status != want {
        let mut tx = state.db.begin().await?;
        let changed: Option<(Option<OffsetDateTime>,)> = if resolve {
            sqlx::query_as(
                "update threads set status = 'resolved', resolved_by = $2, resolved_at = now()
                 where id = $1 and status = 'open' returning resolved_at",
            )
            .bind(t.id)
            .bind(caller.uuid)
            .fetch_optional(&mut *tx)
            .await?
        } else {
            sqlx::query_as(
                "update threads set status = 'open', resolved_by = null, resolved_at = null
                 where id = $1 and status = 'resolved' returning resolved_at",
            )
            .bind(t.id)
            .fetch_optional(&mut *tx)
            .await?
        };
        if resolve
            && let Some((Some(at),)) = changed
            && t.created_by != caller.uuid
            && perms::sheet_role_of(&state, t.created_by, &ctx.access.sheet.id).await?.is_some()
        {
            let first: Option<CommentRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "select {COMMENT_COLUMNS} from comments where thread_id = $1 and deleted_at is null order by created_at, id limit 1"
            )))
            .bind(t.id)
            .fetch_optional(&mut *tx)
            .await?;
            let place = locate_all(&handle, std::slice::from_ref(&t)).await?.pop().ok_or_else(|| ApiError::internal("no place for a thread"))?;
            let tc = TingCtx::new(&state, &ctx, &t, &place);
            let resolver = sheets::caller_account(&state, &caller).await;
            let mut data = match &first {
                Some(c) => {
                    let first_author = state.accounts.get(c.author).await;
                    tc.data(c, &first_author)
                }
                None => {
                    let mut d = tc.data(
                        &CommentRow {
                            id: t.id,
                            thread_id: t.id,
                            author: t.created_by,
                            body: String::new(),
                            mentions: Vec::new(),
                            created_at: t.created_at,
                            edited_at: None,
                        },
                        &state.accounts.get(t.created_by).await,
                    );
                    d["comment"] = Value::Null;
                    d
                }
            };
            data["resolved_by"] = json!(resolver);
            let key = format!("calc.thread.resolved:{}:{}", t.id, at.unix_timestamp_nanos() / 1000);
            notify::enqueue(&mut tx, "calc.thread.resolved", t.created_by, &key, data).await?;
        }
        tx.commit().await?;
        state.notify.wake();
    }
    let thread = one_thread(&state, &ctx, &handle, t.id).await?;
    if t.status != want {
        tell_viewers(&handle, &thread);
    }
    Ok(Json(thread))
}

async fn resolve(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    set_status(state, caller, id, true).await
}

async fn reopen(State(state): State<AppState>, caller: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    set_status(state, caller, id, false).await
}

#[derive(Deserialize)]
struct EmojiBody {
    emoji: Option<String>,
}

fn clean_emoji(e: Option<&str>) -> ApiResult<String> {
    let e = e.map(str::trim).unwrap_or("");
    if e.is_empty() || e.chars().count() > 16 || e.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::invalid_field("emoji", "A reaction is one emoji (or a short word), like 👍."));
    }
    Ok(e.to_owned())
}

async fn react(State(state): State<AppState>, caller: Caller, Path(id): Path<String>, JsonBody(body): JsonBody<EmojiBody>) -> ApiResult<Json<Value>> {
    let (ctx, t, c) = comment_ctx(&state, &caller, &id).await?;
    ctx.require_write()?;
    let emoji = clean_emoji(body.emoji.as_deref())?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    sqlx::query("insert into reactions (comment_id, account, emoji) values ($1, $2, $3) on conflict do nothing")
        .bind(c.id)
        .bind(caller.uuid)
        .bind(&emoji)
        .execute(&state.db)
        .await?;
    let (comment, thread) = comment_answer(&state, &ctx, &handle, t.id, c.id).await?;
    tell_viewers(&handle, &thread);
    Ok(Json(comment))
}

async fn unreact(State(state): State<AppState>, caller: Caller, Path((id, emoji)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    let (ctx, t, c) = comment_ctx(&state, &caller, &id).await?;
    ctx.require_write()?;
    let handle = state.lines.open(&state, &ctx.line.id).await?;
    sqlx::query("delete from reactions where comment_id = $1 and account = $2 and emoji = $3")
        .bind(c.id)
        .bind(caller.uuid)
        .bind(emoji.trim())
        .execute(&state.db)
        .await?;
    let (comment, thread) = comment_answer(&state, &ctx, &handle, t.id, c.id).await?;
    tell_viewers(&handle, &thread);
    Ok(Json(comment))
}

/// Contract: every open thread of a line as JSON (for reading cells "with comments shown"), in sheet order.
pub async fn line_threads(state: &AppState, caller: Option<&Caller>, sheet_id: &str, copy_id: Option<&str>) -> ApiResult<Vec<Value>> {
    let Json(v) = list(state.clone(), caller.cloned(), sheet_id.to_owned(), copy_id.map(str::to_owned), ListQuery { status: None, tab: None }).await?;
    Ok(v["threads"].as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_are_found() {
        assert_eq!(mention_ids("@si:analyst why did March drop?"), vec!["si:analyst"]);
        assert_eq!(mention_ids("ask @C:Ada and @si:ana-lyst."), vec!["c:ada", "si:ana-lyst"]);
        assert_eq!(mention_ids("mail me at x@c:ada"), Vec::<String>::new());
        assert_eq!(mention_ids("@c:ada, @c:ada!"), vec!["c:ada"]);
        assert_eq!(mention_ids("@c: nothing @si:"), Vec::<String>::new());
    }

    #[test]
    fn targets_parse() {
        assert_eq!(Target::parse(None).unwrap().1, Target::Tab);
        assert_eq!(Target::parse(Some("B2:B9")).unwrap().1, Target::Cells(Rect::new(1, 1, 8, 1)));
        assert_eq!(Target::parse(Some("B:B")).unwrap().1, Target::Cols(1, 1));
        assert_eq!(Target::parse(Some("D:B")).unwrap().1, Target::Cols(1, 3));
        assert_eq!(Target::parse(Some("3:3")).unwrap().1, Target::Rows(2, 2));
        assert_eq!(Target::parse(Some("Sales!$B$7")).unwrap(), (Some("Sales".into()), Target::Cells(Rect::new(6, 1, 6, 1))));
        assert!(Target::parse(Some("A1:A")).is_err());
        assert!(Target::parse(Some("0:3")).is_err());
        assert_eq!(range_text(Some((2, 4)), None).as_deref(), Some("3:5"));
        assert_eq!(range_text(None, Some((1, 3))).as_deref(), Some("B:D"));
        assert_eq!(range_text(Some((1, 8)), Some((1, 1))).as_deref(), Some("B2:B9"));
    }

    #[test]
    fn threads_follow_their_cells() {
        let mut wb = Workbook::new();
        let tab = wb.tabs[0].id;
        {
            let t = wb.tab_mut(tab).unwrap();
            t.rows.ensure_len(10);
            t.cols.ensure_len(3);
        }
        let Placing::Ready(p) = place(&wb, &TabRef::Id(tab), Target::parse(Some("B2:B4")).unwrap().1).unwrap() else {
            panic!("ids exist")
        };
        assert_eq!(locate(&wb, &p.anchor).range.as_deref(), Some("B2:B4"));
        // Two rows inserted above: it moves down.
        let t = wb.tab_mut(tab).unwrap();
        t.rows.insert(0, 2);
        assert_eq!(locate(&wb, &p.anchor).range.as_deref(), Some("B4:B6"));
        // Its first row deleted: it keeps the rest.
        let t = wb.tab_mut(tab).unwrap();
        t.rows.remove(3, 1);
        assert_eq!(locate(&wb, &p.anchor).range.as_deref(), Some("B4:B5"));
        // All of its rows deleted: detached.
        let t = wb.tab_mut(tab).unwrap();
        t.rows.remove(3, 2);
        let at = locate(&wb, &p.anchor);
        assert!(at.detached);
        assert_eq!(at.range, None);
        // Never-written cells need ids first.
        assert!(matches!(place(&wb, &TabRef::First, Target::parse(Some("Z500")).unwrap().1).unwrap(), Placing::Extend { rows: 500, cols: 26, .. }));
    }
}
