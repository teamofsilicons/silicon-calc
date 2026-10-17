//! Lines in Postgres and blob storage: the operations log (small operations inline as JSON, big ones as
//! MessagePack + zstd blobs), snapshots, the cells each operation changed (for cell history and overwrite reports),
//! and loading a line (latest snapshot, then the operations after it).

use std::collections::{HashMap, HashSet};

use calc_engine::{Op, TabId, Workbook, snapshot};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    OpKind,
    apply::{self, Candidate, Info},
    registry::Shared,
    task::{TxHook, blocking},
};
use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

/// Operations whose JSON is at least this big are kept in blob storage.
const INLINE_MAX: usize = 64 * 1024;
/// Operations with more cell entries than this skip trying JSON at all.
const INLINE_CELLS: usize = 2_000;
const OP_MAGIC: &[u8; 8] = b"SICALCOP";

/// Runs CPU work on a borrowed value: on a blocking-capable worker when the runtime has them, else in place.
pub(crate) fn blocking_ref<R>(f: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// The same JSON with every NUL character replaced (Postgres' jsonb can't hold one).
pub(crate) fn jsonb_safe(v: Value) -> Value {
    match v {
        Value::String(s) if s.contains('\0') => Value::String(s.replace('\0', "\u{FFFD}")),
        Value::Array(a) => Value::Array(a.into_iter().map(jsonb_safe).collect()),
        Value::Object(m) => Value::Object(m.into_iter().map(|(k, v)| (k.replace('\0', "\u{FFFD}"), jsonb_safe(v))).collect()),
        other => other,
    }
}

/// An operation ready to save.
pub(crate) enum Encoded {
    Inline(Value),
    Blob(Vec<u8>),
}

fn op_entries(op: &Op) -> usize {
    match op {
        Op::Batch { ops } => ops.iter().map(op_entries).sum(),
        Op::SetCells { cells, .. } => cells.len(),
        Op::RestoreRows { cells, formulas, .. } | Op::RestoreCols { cells, formulas, .. } => cells.len() + formulas.len(),
        Op::ReplaceTab { content, .. } | Op::RestoreTab { content, .. } => {
            content.columns.iter().map(|c| c.values.len() + c.formulas.len() + c.styles.len()).sum::<usize>() + 1
        }
        Op::Reorder { ids, .. } | Op::DeleteRows { ids, .. } | Op::DeleteCols { ids, .. } => ids.len() / 8,
        _ => 1,
    }
}

/// Small operations as JSON (when it reads back exactly), others as MessagePack + zstd.
pub(crate) fn encode_op(op: &Op) -> ApiResult<Encoded> {
    if op_entries(op) <= INLINE_CELLS
        && let Ok(text) = serde_json::to_string(op)
        && text.len() < INLINE_MAX
        && !text.contains("\\u0000")
        && let Ok(v) = serde_json::from_str::<Value>(&text)
        // Only what reads back exactly (numbers JSON can't hold go to a blob).
        && serde_json::from_value::<Op>(v.clone()).is_ok_and(|back| back == *op)
    {
        return Ok(Encoded::Inline(v));
    }
    let raw = rmp_serde::to_vec_named(op).map_err(|e| ApiError::internal(format!("encoding an operation: {e}")))?;
    let mut out = Vec::with_capacity(raw.len() / 3 + OP_MAGIC.len());
    out.extend_from_slice(OP_MAGIC);
    out.extend(zstd::bulk::compress(&raw, 3).map_err(|e| ApiError::internal(format!("compressing an operation: {e}")))?);
    Ok(Encoded::Blob(out))
}

fn decode_blob(bytes: &[u8]) -> ApiResult<Op> {
    let body = bytes.strip_prefix(OP_MAGIC.as_slice()).ok_or_else(|| ApiError::internal("an operation blob has no header"))?;
    let raw = zstd::stream::decode_all(body).map_err(|e| ApiError::internal(format!("decompressing an operation: {e}")))?;
    rmp_serde::from_slice(&raw).map_err(|e| ApiError::internal(format!("decoding an operation: {e}")))
}

/// Reads a saved operation (or inverse) back.
pub(crate) async fn decode_op(app: &AppState, json: Option<Value>, blob: Option<String>) -> ApiResult<Op> {
    if let Some(v) = json {
        return serde_json::from_value(v).map_err(|e| ApiError::internal(format!("reading a saved operation: {e}")));
    }
    let key = blob.ok_or_else(|| ApiError::internal("a saved operation has neither JSON nor a blob"))?;
    let bytes = app.blobs.get(&key).await?;
    blocking(move || decode_blob(&bytes)).await?
}

async fn store_encoded(app: &AppState, e: Encoded) -> ApiResult<(Option<Value>, Option<String>)> {
    Ok(match e {
        Encoded::Inline(v) => (Some(v), None),
        Encoded::Blob(b) => (None, Some(app.blobs.put(&b).await?)),
    })
}

/// A line's state as loaded.
pub(crate) struct Loaded {
    pub wb: Workbook,
    pub version: i64,
    /// The version of the snapshot it was loaded from.
    pub snapshot: i64,
}

/// The latest snapshot, then every operation after it.
pub(crate) async fn load(app: &AppState, line_id: &str) -> ApiResult<Loaded> {
    let (snap, wb) = snapshot_at_most(app, line_id, None).await?;
    let (wb, version) = replay_ops(app, line_id, wb, snap, None).await?;
    Ok(Loaded { wb, version, snapshot: snap })
}

/// The latest snapshot of a line at or before `at_most` (any version when None).
pub(crate) async fn snapshot_at_most(app: &AppState, line_id: &str, at_most: Option<i64>) -> ApiResult<(i64, Workbook)> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "select seq, blob from snapshots where line_id = $1 and ($2::bigint is null or seq <= $2) order by seq desc limit 1",
    )
    .bind(line_id)
    .bind(at_most)
    .fetch_optional(&app.db)
    .await?;
    let Some((seq, key)) = row else {
        return Err(ApiError::internal(format!("line {line_id} has no snapshot at or before {at_most:?}")));
    };
    let bytes = app.blobs.get(&key).await?;
    let wb = blocking(move || snapshot::decode(&bytes))
        .await?
        .map_err(|e| ApiError::internal(format!("reading the snapshot of {line_id} at {seq}: {e}")))?;
    Ok((seq, wb))
}

/// The nearest snapshot version at or before `at_most`.
pub(crate) async fn snapshot_version_at_most(app: &AppState, line_id: &str, at_most: i64) -> ApiResult<Option<i64>> {
    let row: Option<(i64,)> =
        sqlx::query_as("select seq from snapshots where line_id = $1 and seq <= $2 order by seq desc limit 1")
            .bind(line_id)
            .bind(at_most)
            .fetch_optional(&app.db)
            .await?;
    Ok(row.map(|r| r.0))
}

#[derive(sqlx::FromRow)]
struct OpRow {
    seq: i64,
    author: Uuid,
    at: OffsetDateTime,
    op: Option<Value>,
    op_blob: Option<String>,
    staged: bool,
}

/// One saved operation, decoded.
pub(crate) struct SavedOp {
    pub seq: i64,
    pub author: Uuid,
    pub at: i64,
    pub staged: bool,
    pub op: Op,
}

/// The saved operations in `(after, to]` (`to` none: all), oldest first, at most `limit`.
pub(crate) async fn ops_page(app: &AppState, line_id: &str, after: i64, to: Option<i64>, limit: i64) -> ApiResult<Vec<SavedOp>> {
    let rows: Vec<OpRow> = sqlx::query_as(
        "select seq, author, at, op, op_blob, staged from ops
         where line_id = $1 and seq > $2 and ($3::bigint is null or seq <= $3) order by seq limit $4",
    )
    .bind(line_id)
    .bind(after)
    .bind(to)
    .bind(limit)
    .fetch_all(&app.db)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let op = decode_op(app, r.op, r.op_blob).await?;
        out.push(SavedOp { seq: r.seq, author: r.author, at: r.at.unix_timestamp(), staged: r.staged, op });
    }
    Ok(out)
}

/// Applies the operations after `from` (up to `to`, or all) to `wb`. Answers the workbook and its version.
pub(crate) async fn replay_ops(app: &AppState, line_id: &str, mut wb: Workbook, from: i64, to: Option<i64>) -> ApiResult<(Workbook, i64)> {
    const PAGE: i64 = 500;
    let mut last = from;
    loop {
        let page = ops_page(app, line_id, last, to, PAGE).await?;
        let n = page.len() as i64;
        if n == 0 {
            break;
        }
        last = page.last().map(|d| d.seq).unwrap_or(last);
        let line = line_id.to_owned();
        wb = blocking(move || {
            for s in &page {
                if let Err(e) = apply::replay(&mut wb, &s.op, s.staged, &s.author.to_string(), s.at) {
                    tracing::error!(line = %line, seq = s.seq, error = %e, "an operation failed to apply again");
                }
            }
            drop(page);
            wb
        })
        .await?;
        if n < PAGE {
            break;
        }
    }
    Ok((wb, last))
}

/// What is saved about a change besides its operation.
pub(crate) struct OpMeta {
    pub seq: i64,
    pub author: Uuid,
    pub at: OffsetDateTime,
    pub kind: OpKind,
    pub client_id: Option<String>,
    pub undoes: Option<i64>,
    pub redoes: Option<i64>,
    pub staged: bool,
}

/// Saves one applied change: the operation, its inverse, what it changed, and the line's new head, in one
/// transaction (with the hook, when given).
pub(crate) async fn persist(
    app: &AppState,
    shared: &Shared,
    meta: &OpMeta,
    op: Op,
    inverse: Option<Op>,
    info: &Info,
    hook: Option<TxHook>,
) -> ApiResult<()> {
    let (op_enc, inv_enc) = blocking(move || -> ApiResult<(Encoded, Option<Encoded>)> {
        let a = encode_op(&op)?;
        let b = inverse.as_ref().map(encode_op).transpose()?;
        Ok((a, b))
    })
    .await??;
    let (op_json, op_blob) = store_encoded(app, op_enc).await?;
    let (inv_json, inv_blob) = match inv_enc {
        Some(e) => store_encoded(app, e).await?,
        None => (None, None),
    };
    let line = &*shared.line_id;
    let mut tx = app.db.begin().await?;
    sqlx::query(
        "insert into ops (line_id, seq, author, at, kind, summary, op, op_blob, inverse, inverse_blob, cells_changed,
                          client_id, undoes, redoes, changes, facts, staged)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(line)
    .bind(meta.seq)
    .bind(meta.author)
    .bind(meta.at)
    .bind(meta.kind.as_str())
    .bind(&info.summary)
    .bind(&op_json)
    .bind(&op_blob)
    .bind(&inv_json)
    .bind(&inv_blob)
    .bind(info.cells_changed)
    .bind(meta.client_id.as_deref().map(|c| c.replace('\0', "")))
    .bind(meta.undoes)
    .bind(meta.redoes)
    .bind(jsonb_safe(Value::Array(info.changes.clone())))
    .bind(jsonb_safe(info.facts.clone()))
    .bind(meta.staged)
    .execute(&mut *tx)
    .await?;
    if !info.history.is_empty() {
        let tabs: Vec<i32> = info.history.iter().map(|h| h.tab as i32).collect();
        let rows: Vec<i64> = info.history.iter().map(|h| h.row as i64).collect();
        let cols: Vec<i64> = info.history.iter().map(|h| h.col as i64).collect();
        let before: Vec<Value> = info.history.iter().map(|h| h.before.clone()).collect();
        let after: Vec<Value> = info.history.iter().map(|h| h.after.clone()).collect();
        sqlx::query(
            "insert into op_cells (line_id, tab_id, row_id, col_id, seq, before, after)
             select $1, t.tab, t.r, t.c, $2, t.b, t.a
             from unnest($3::int[], $4::bigint[], $5::bigint[], $6::jsonb[], $7::jsonb[]) as t(tab, r, c, b, a)
             on conflict do nothing",
        )
        .bind(line)
        .bind(meta.seq)
        .bind(tabs)
        .bind(rows)
        .bind(cols)
        .bind(before)
        .bind(after)
        .execute(&mut *tx)
        .await?;
    }
    if !info.op_tabs.is_empty() {
        let tabs: Vec<i32> = info.op_tabs.iter().map(|t| t.tab as i32).collect();
        let bulk: Vec<bool> = info.op_tabs.iter().map(|t| t.bulk).collect();
        let r1: Vec<Option<i64>> = info.op_tabs.iter().map(|t| t.area.map(|a| a.0 as i64)).collect();
        let c1: Vec<Option<i64>> = info.op_tabs.iter().map(|t| t.area.map(|a| a.1 as i64)).collect();
        let r2: Vec<Option<i64>> = info.op_tabs.iter().map(|t| t.area.map(|a| a.2 as i64)).collect();
        let c2: Vec<Option<i64>> = info.op_tabs.iter().map(|t| t.area.map(|a| a.3 as i64)).collect();
        sqlx::query(
            "insert into op_tabs (line_id, seq, tab_id, bulk, r1, c1, r2, c2)
             select $1, $2, t.tab, t.bulk, t.r1, t.c1, t.r2, t.c2
             from unnest($3::int[], $4::bool[], $5::bigint[], $6::bigint[], $7::bigint[], $8::bigint[])
               as t(tab, bulk, r1, c1, r2, c2)
             on conflict do nothing",
        )
        .bind(line)
        .bind(meta.seq)
        .bind(tabs)
        .bind(bulk)
        .bind(r1)
        .bind(c1)
        .bind(r2)
        .bind(c2)
        .execute(&mut *tx)
        .await?;
    }
    let moved = sqlx::query("update lines set head = $2 where id = $1 and head = $3")
        .bind(line)
        .bind(meta.seq)
        .bind(meta.seq - 1)
        .execute(&mut *tx)
        .await?;
    if moved.rows_affected() != 1 {
        return Err(ApiError::internal(format!("line {line} was changed elsewhere while saving version {}", meta.seq)));
    }
    if shared.is_main {
        sqlx::query("update sheets set updated_at = $2, updated_by = $3 where id = $1")
            .bind(&*shared.sheet_id)
            .bind(meta.at)
            .bind(meta.author)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(h) = hook {
        h(&mut tx, meta.seq).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Writes a snapshot of `wb` (a line at `version`) and, for a main line, refreshes the sheet's search text.
/// Answers the version it holds.
pub(crate) async fn write_snapshot(
    app: &AppState,
    line_id: &str,
    sheet_id: &str,
    is_main: bool,
    wb: Workbook,
    version: i64,
) -> ApiResult<i64> {
    let (bytes, text) = blocking(move || {
        let bytes = snapshot::encode(&wb);
        let text = is_main.then(|| crate::search::search_text_of(&wb));
        drop(wb);
        (bytes, text)
    })
    .await?;
    let key = app.blobs.put(&bytes).await?;
    sqlx::query("insert into snapshots (line_id, seq, blob, bytes) values ($1, $2, $3, $4) on conflict (line_id, seq) do nothing")
        .bind(line_id)
        .bind(version)
        .bind(&key)
        .bind(bytes.len() as i64)
        .execute(&app.db)
        .await?;
    if let Some(text) = text {
        crate::search::set_search_text(app, sheet_id, &text).await?;
    }
    Ok(version)
}

/// The change an undo (or redo) takes back, with its saved inverse.
pub(crate) struct UndoTarget {
    pub seq: i64,
    pub summary: String,
    pub inverse: Op,
}

/// What a change was, without the "undid"/"redid" around it.
pub(crate) fn core_summary(s: &str) -> &str {
    let mut s = s;
    loop {
        let inner = s.strip_prefix("undid \"").or_else(|| s.strip_prefix("redid \"")).and_then(|r| r.strip_suffix('"'));
        match inner {
            Some(i) => s = i,
            None => return s,
        }
    }
}

/// The caller's own change an undo takes back (their latest change not undone yet), or the undo a redo takes back
/// (their latest undo not redone yet, when they made no new change since).
pub(crate) async fn undo_target(app: &AppState, line_id: &str, author: Uuid, redo: bool) -> ApiResult<Option<UndoTarget>> {
    let rows: Vec<(i64, String, String, Option<i64>, Option<i64>, bool)> = sqlx::query_as(
        "select seq, kind, summary, undoes, redoes, (inverse is not null or inverse_blob is not null)
         from ops where line_id = $1 and author = $2 order by seq desc limit 5000",
    )
    .bind(line_id)
    .bind(author)
    .fetch_all(&app.db)
    .await?;
    let mut undone: HashSet<i64> = HashSet::new();
    let mut redone: HashSet<i64> = HashSet::new();
    let mut target: Option<(i64, String)> = None;
    for (seq, kind, summary, undoes, redoes, has_inverse) in rows {
        let kind = OpKind::parse(&kind).unwrap_or(OpKind::Edit);
        if redo {
            match kind {
                OpKind::Redo => {
                    if let Some(u) = redoes {
                        redone.insert(u);
                    }
                }
                OpKind::Undo => {
                    if !redone.contains(&seq) && has_inverse {
                        target = Some((seq, summary));
                        break;
                    }
                }
                // A new change since the undo: nothing to redo.
                _ => break,
            }
        } else {
            match kind {
                OpKind::Undo => {
                    if let Some(x) = undoes {
                        undone.insert(x);
                    }
                }
                _ => {
                    if !undone.contains(&seq) && has_inverse {
                        target = Some((seq, summary));
                        break;
                    }
                }
            }
        }
    }
    let Some((seq, summary)) = target else { return Ok(None) };
    let (json, blob): (Option<Value>, Option<String>) =
        sqlx::query_as("select inverse, inverse_blob from ops where line_id = $1 and seq = $2")
            .bind(line_id)
            .bind(seq)
            .fetch_one(&app.db)
            .await?;
    let inverse = decode_op(app, json, blob).await?;
    Ok(Some(UndoTarget { seq, summary: core_summary(&summary).to_owned(), inverse }))
}

/// A cell someone else changed after the version a change was based on, which the change replaced.
pub(crate) struct Overwritten {
    pub tab: TabId,
    pub address: String,
    pub by: Uuid,
    pub at: OffsetDateTime,
    pub before: Value,
    pub version: i64,
}

/// Of the cells a change replaced, those whose latest change in `(base, head]` was by someone else.
pub(crate) async fn overwrites(
    app: &AppState,
    line_id: &str,
    base: i64,
    head: i64,
    author: Uuid,
    candidates: &[Candidate],
) -> ApiResult<Vec<Overwritten>> {
    let candidates = &candidates[..candidates.len().min(apply::HISTORY_CELLS)];
    let base = base.max(head - 5_000);
    let tabs: Vec<i32> = candidates.iter().map(|c| c.tab as i32).collect();
    let rows: Vec<i64> = candidates.iter().map(|c| c.row as i64).collect();
    let cols: Vec<i64> = candidates.iter().map(|c| c.col as i64).collect();
    let exact: Vec<(i32, i64, i64, i64, Uuid, OffsetDateTime)> = sqlx::query_as(
        "select distinct on (c.tab_id, c.row_id, c.col_id) c.tab_id, c.row_id, c.col_id, c.seq, o.author, o.at
         from op_cells c
         join unnest($4::int[], $5::bigint[], $6::bigint[]) as w(tab, r, k) on c.tab_id = w.tab and c.row_id = w.r and c.col_id = w.k
         join ops o on o.line_id = c.line_id and o.seq = c.seq
         where c.line_id = $1 and c.seq > $2 and c.seq <= $3
         order by c.tab_id, c.row_id, c.col_id, c.seq desc",
    )
    .bind(line_id)
    .bind(base)
    .bind(head)
    .bind(&tabs)
    .bind(&rows)
    .bind(&cols)
    .fetch_all(&app.db)
    .await?;
    let bulk: Vec<(i32, i64, Uuid, OffsetDateTime)> = sqlx::query_as(
        "select t.tab_id, t.seq, o.author, o.at from op_tabs t join ops o on o.line_id = t.line_id and o.seq = t.seq
         where t.line_id = $1 and t.bulk and t.seq > $2 and t.seq <= $3 and t.tab_id = any($4)
         order by t.seq desc",
    )
    .bind(line_id)
    .bind(base)
    .bind(head)
    .bind(&tabs)
    .fetch_all(&app.db)
    .await?;
    let exact: HashMap<(u32, u32, u32), (i64, Uuid, OffsetDateTime)> =
        exact.into_iter().map(|(t, r, c, s, a, at)| ((t as u32, r as u32, c as u32), (s, a, at))).collect();
    let mut out = Vec::new();
    for c in candidates {
        let mut latest = exact.get(&(c.tab, c.row, c.col)).copied();
        // A bulk change (an import, a big paste) counts when it was what last wrote the cell: the cell remembers
        // its author and the second it was written.
        if let (Some(prev), Some(prev_at)) = (&c.prev_author, c.prev_at)
            && let Some(b) = bulk
                .iter()
                .find(|b| b.0 as u32 == c.tab && b.2.to_string() == *prev && b.3.unix_timestamp() == prev_at)
            && latest.is_none_or(|l| b.1 > l.0)
        {
            latest = Some((b.1, b.2, b.3));
        }
        if let Some((seq, by, at)) = latest
            && by != author
        {
            out.push(Overwritten {
                tab: c.tab,
                address: c.address.clone(),
                by,
                at,
                before: serde_json::to_value(&c.before).unwrap_or(Value::Null),
                version: seq,
            });
        }
    }
    Ok(out)
}

/// One change to one cell, for its history.
pub(crate) struct CellChange {
    pub version: i64,
    pub by: Uuid,
    pub at: OffsetDateTime,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub summary: String,
    /// For a bulk change: the area it covered (corner ids), none for the whole tab. `None` for a change recorded
    /// cell by cell.
    pub bulk: Option<Option<(u32, u32, u32, u32)>>,
}

/// The latest changes to one cell at or before `up_to`, and the bulk changes to its tab (which the caller checks
/// against the cell's position), newest first.
pub(crate) async fn cell_changes(
    app: &AppState,
    line_id: &str,
    tab: TabId,
    row: u32,
    col: u32,
    up_to: i64,
    limit: i64,
) -> ApiResult<Vec<CellChange>> {
    let exact: Vec<(i64, Uuid, OffsetDateTime, Option<Value>, Option<Value>, String)> = sqlx::query_as(
        "select c.seq, o.author, o.at, c.before, c.after, o.summary
         from op_cells c join ops o on o.line_id = c.line_id and o.seq = c.seq
         where c.line_id = $1 and c.tab_id = $2 and c.row_id = $3 and c.col_id = $4 and c.seq <= $5
         order by c.seq desc limit $6",
    )
    .bind(line_id)
    .bind(tab as i32)
    .bind(row as i64)
    .bind(col as i64)
    .bind(up_to)
    .bind(limit)
    .fetch_all(&app.db)
    .await?;
    type BulkRow = (i64, Uuid, OffsetDateTime, String, Option<i64>, Option<i64>, Option<i64>, Option<i64>);
    let bulk: Vec<BulkRow> = sqlx::query_as(
        "select t.seq, o.author, o.at, o.summary, t.r1, t.c1, t.r2, t.c2
         from op_tabs t join ops o on o.line_id = t.line_id and o.seq = t.seq
         where t.line_id = $1 and t.tab_id = $2 and t.bulk and t.seq <= $3
         order by t.seq desc limit $4",
    )
    .bind(line_id)
    .bind(tab as i32)
    .bind(up_to)
    .bind(limit * 3)
    .fetch_all(&app.db)
    .await?;
    let mut out: Vec<CellChange> = exact
        .into_iter()
        .map(|(version, by, at, before, after, summary)| CellChange { version, by, at, before, after, summary, bulk: None })
        .collect();
    for (version, by, at, summary, r1, c1, r2, c2) in bulk {
        let area = match (r1, c1, r2, c2) {
            (Some(a), Some(b), Some(c), Some(d)) => Some((a as u32, b as u32, c as u32, d as u32)),
            _ => None,
        };
        out.push(CellChange { version, by, at, before: None, after: None, summary, bulk: Some(area) });
    }
    out.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(out)
}

/// One saved operation, as version lists need it.
#[derive(Clone, Debug, sqlx::FromRow)]
pub(crate) struct OpListing {
    pub seq: i64,
    pub author: Uuid,
    pub at: OffsetDateTime,
    pub kind: String,
    pub summary: String,
    pub facts: Option<Value>,
    pub cells_changed: i64,
}

/// Operations before `before` (exclusive), newest first.
pub(crate) async fn ops_before(app: &AppState, line_id: &str, before: i64, limit: i64) -> ApiResult<Vec<OpListing>> {
    Ok(sqlx::query_as(
        "select seq, author, at, kind, summary, facts, cells_changed from ops
         where line_id = $1 and seq < $2 order by seq desc limit $3",
    )
    .bind(line_id)
    .bind(before)
    .bind(limit)
    .fetch_all(&app.db)
    .await?)
}

/// A saved change, as a live stream replays it.
#[derive(sqlx::FromRow)]
pub(crate) struct ChangeRow {
    pub seq: i64,
    pub author: Uuid,
    pub at: OffsetDateTime,
    pub kind: String,
    pub summary: String,
    pub changes: Option<Value>,
    pub client_id: Option<String>,
}

/// The saved changes after `since` (at most `limit`), for viewers who reconnect.
pub(crate) async fn changes_since(app: &AppState, line_id: &str, since: i64, limit: i64) -> ApiResult<Vec<ChangeRow>> {
    Ok(sqlx::query_as(
        "select seq, author, at, kind, summary, changes, client_id from ops where line_id = $1 and seq > $2 order by seq limit $3",
    )
    .bind(line_id)
    .bind(since)
    .bind(limit)
    .fetch_all(&app.db)
    .await?)
}
