//! Applying one change to a line's workbook, atomically, and describing what it changed.
//!
//! Everything here runs on a blocking thread inside the line's task. A change is one or more stages (one per edit
//! of a request); each stage builds its operation from the state the earlier stages left. Atomicity:
//! - one light operation is checked first (formulas that can't be read, protections, the engine's own refusals
//!   happen before it changes anything) and then applied in place, under the line's write lock;
//! - several stages, or a heavy operation, are applied to a copy of the workbook that replaces the line's only when
//!   everything worked (readers keep reading the old state meanwhile).
//!
//! If the engine still fails part way through an operation applied in place, the line is rebuilt from what is
//! saved, so a failed change never leaves anything behind.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::http::StatusCode;
use calc_engine::{
    CellPos, ColId, Op, OpError, Rect, RowId, TabId, Workbook,
    formula::parse::parse_formula,
    input::{Entry, parse_entry},
    model::{Protection, unpack_meta},
    ops::{Actor, Applied, CellWrite, Overwrite},
};
use serde_json::{Value, json};

use super::{OpKind, registry::LineState, task::Stage, view};
use crate::error::{ApiError, ApiResult};

/// Operations writing more cells than this are kept in history as one bulk change per tab, not cell by cell.
pub(crate) const HISTORY_CELLS: usize = 2_000;
/// Change events carry the new contents of at most this many cells.
pub(crate) const EVENT_CELLS: usize = 2_000;
/// Above this many ranges per tab, a change is described by the area around them.
const MAX_RANGES: usize = 64;

/// Who is applying a change and how history names it.
#[derive(Clone, Debug)]
pub(crate) struct Ctx {
    pub author: String,
    pub owner: String,
    /// Unix seconds.
    pub at: i64,
    /// Errors name the failing edit (`details.index`) and answer 422.
    pub index_errors: bool,
    pub kind: OpKind,
    pub summary: Option<String>,
}

impl Ctx {
    fn actor(&self) -> Actor<'_> {
        Actor { account: &self.author, at: self.at }
    }
}

pub(crate) enum Mode {
    /// Applied in place, under the line's write lock.
    Light,
    /// Applied to a copy that replaces the workbook afterwards.
    Heavy,
}

/// A change that was applied (in memory) and must now be saved.
pub(crate) struct Done {
    pub op: Op,
    pub inverse: Option<Op>,
    /// Several stages, each applied on its own (replay applies them the same way).
    pub staged: bool,
    pub info: Info,
}

/// What a change did, for saving it, telling viewers and answering.
pub(crate) struct Info {
    /// docs/API.md `Change` objects.
    pub changes: Vec<Value>,
    /// The new contents of the changed cells, when there are few enough.
    pub event_cells: Option<EventCells>,
    /// What each changed cell held before and after (empty for bulk changes).
    pub history: Vec<HistoryRow>,
    pub op_tabs: Vec<OpTab>,
    /// `{"cells": {"<tab name>": n}, "phrases": ["…"]}`: what versions say.
    pub facts: Value,
    pub summary: String,
    pub cells_changed: i64,
    pub skipped: usize,
    pub tabs_changed: Vec<TabId>,
    /// Cells whose earlier contents this change replaced (for overwrite reports).
    pub candidates: Vec<Candidate>,
    pub used: Vec<UsedChange>,
}

pub(crate) struct EventCells {
    pub cells: Vec<Value>,
    pub styles: Value,
    pub uuids: BTreeSet<String>,
}

pub(crate) struct HistoryRow {
    pub tab: TabId,
    pub row: RowId,
    pub col: ColId,
    pub before: Value,
    pub after: Value,
}

pub(crate) struct OpTab {
    pub tab: TabId,
    /// Too many cells changed to list them.
    pub bulk: bool,
    /// For bulk changes: the area's corner ids (row, col, row, col); none is the whole tab.
    pub area: Option<(RowId, ColId, RowId, ColId)>,
}

/// A cell whose content this change replaced.
pub(crate) struct Candidate {
    pub tab: TabId,
    pub row: RowId,
    pub col: ColId,
    pub address: String,
    pub before: calc_engine::Value,
    /// Who last changed the cell before this change, and when (Unix seconds), as the cell itself remembers.
    pub prev_author: Option<String>,
    pub prev_at: Option<i64>,
}

pub(crate) enum UsedChange {
    Invalidate(TabId),
    /// Positions written, and whether each holds something now.
    Cells(TabId, Vec<(u32, u32, bool)>),
}

pub(crate) struct Failure {
    pub error: ApiError,
    /// The workbook may have been changed part way: the line must be rebuilt from what is saved.
    pub mutated: bool,
}

/// Builds and checks a single-stage change.
pub(crate) fn plan(wb: &Workbook, stage: Stage, ctx: &Ctx) -> ApiResult<Option<(Op, Mode)>> {
    let op = match stage(wb) {
        Ok(Some(op)) => op,
        Ok(None) => return Ok(None),
        Err(e) => return Err(indexed(e, 0, ctx)),
    };
    if is_noop(wb, &op) {
        return Ok(None);
    }
    check(wb, &op, ctx).map_err(|e| indexed(e, 0, ctx))?;
    let mode = classify(wb, &op);
    Ok(Some((op, mode)))
}

/// Applies a checked light operation in place.
pub(crate) fn in_place(st: &mut LineState, op: Op, ctx: &Ctx) -> Result<Done, Failure> {
    match st.wb.apply(&op, &ctx.actor()) {
        Ok(applied) => {
            st.version += 1;
            let merged = merge(vec![applied]);
            let info = describe(&st.wb, std::slice::from_ref(&op), &merged, ctx);
            update_used(st, &info.used);
            Ok(Done { op, inverse: merged.inverse, staged: false, info })
        }
        Err(e) => {
            // The engine refuses before changing anything, except for formulas (caught by the checks) and steps
            // of a batch after the first.
            let mutated = matches!(e, OpError::Formula { .. }) || matches!(op, Op::Batch { .. });
            Err(Failure { error: indexed(e.into(), 0, ctx), mutated })
        }
    }
}

/// Applies a checked operation to a copy of the workbook.
pub(crate) fn on_copy(wb: &mut Workbook, op: Op, ctx: &Ctx) -> ApiResult<Done> {
    let applied = wb.apply(&op, &ctx.actor()).map_err(|e| indexed(e.into(), 0, ctx))?;
    let merged = merge(vec![applied]);
    let info = describe(wb, std::slice::from_ref(&op), &merged, ctx);
    Ok(Done { op, inverse: merged.inverse, staged: false, info })
}

/// Applies every stage, in order, to a copy of the workbook.
pub(crate) fn staged(wb: &mut Workbook, stages: Vec<Stage>, ctx: &Ctx) -> ApiResult<Option<Done>> {
    let mut ops = Vec::new();
    let mut applied = Vec::new();
    for (i, stage) in stages.into_iter().enumerate() {
        let op = match stage(wb) {
            Ok(Some(op)) => op,
            Ok(None) => continue,
            Err(e) => return Err(indexed(e, i, ctx)),
        };
        if is_noop(wb, &op) {
            continue;
        }
        check(wb, &op, ctx).map_err(|e| indexed(e, i, ctx))?;
        let a = wb.apply(&op, &ctx.actor()).map_err(|e| indexed(e.into(), i, ctx))?;
        ops.push(op);
        applied.push(a);
    }
    if ops.is_empty() {
        return Ok(None);
    }
    let merged = merge(applied);
    let info = describe(wb, &ops, &merged, ctx);
    let staged = ops.len() > 1;
    let op = if ops.len() == 1 { ops.pop().expect("one") } else { Op::Batch { ops } };
    Ok(Some(Done { op, inverse: merged.inverse, staged, info }))
}

/// Replays a saved operation (loading a line, rebuilding a version).
pub(crate) fn replay(wb: &mut Workbook, op: &Op, staged: bool, account: &str, at: i64) -> Result<(), OpError> {
    let actor = Actor { account, at };
    match (staged, op) {
        (true, Op::Batch { ops }) => {
            for o in ops {
                wb.apply(o, &actor)?;
            }
            Ok(())
        }
        _ => wb.apply(op, &actor).map(|_| ()),
    }
}

/// Adds `details.index` (the failing edit) and answers 422, except for refusals (403).
pub(crate) fn indexed(mut e: ApiError, index: usize, ctx: &Ctx) -> ApiError {
    if !ctx.index_errors {
        return e;
    }
    let mut details = match e.details.take() {
        Some(Value::Object(m)) => m,
        Some(other) => {
            let mut m = serde_json::Map::new();
            m.insert("detail".into(), other);
            m
        }
        None => serde_json::Map::new(),
    };
    details.insert("index".into(), json!(index));
    e.details = Some(Value::Object(details));
    if e.status != StatusCode::FORBIDDEN && e.status != StatusCode::UNPROCESSABLE_ENTITY {
        e.status = StatusCode::UNPROCESSABLE_ENTITY;
    }
    if !e.message.starts_with("Edit ") {
        e.message = format!("Edit {index}: {}", e.message);
    }
    e
}

pub(crate) fn flatten<'a>(op: &'a Op, out: &mut Vec<&'a Op>) {
    match op {
        Op::Batch { ops } => {
            for o in ops {
                flatten(o, out);
            }
        }
        other => out.push(other),
    }
}

/// An operation that changes nothing at all: no new version. Giving cells ids (`Extend`) is a change even though
/// nobody sees it: comments are anchored to those ids, so it is saved like any other.
pub(crate) fn is_noop(wb: &Workbook, op: &Op) -> bool {
    match op {
        Op::Batch { ops } => ops.iter().all(|o| is_noop(wb, o)),
        Op::Extend { tab, rows, cols } => wb.tab(*tab).is_some_and(|t| *rows <= t.rows.len() && *cols <= t.cols.len()),
        Op::SetCells { cells, .. } => cells.is_empty(),
        Op::DeleteRows { ids, .. }
        | Op::DeleteCols { ids, .. }
        | Op::InsertRows { ids, .. }
        | Op::InsertCols { ids, .. }
        | Op::Hide { ids, .. } => ids.is_empty(),
        Op::Reorder { tab, rows, ids } => {
            ids.is_empty()
                || wb.tab(*tab).is_some_and(|t| {
                    let axis = if *rows { &t.rows } else { &t.cols };
                    let mut pos: Vec<u32> = ids.iter().filter_map(|&i| axis.pos_of(i)).collect();
                    if pos.len() != ids.len() {
                        return false;
                    }
                    pos.sort_unstable();
                    pos.iter().zip(ids).all(|(&p, &id)| axis.id_at(p) == Some(id))
                })
        }
        Op::ColWidths { widths, .. } => widths.is_empty(),
        Op::RowHeights { heights, .. } => heights.is_empty(),
        Op::RenameTab { tab, name } => wb.tab(*tab).is_some_and(|t| t.name == *name),
        Op::MoveTab { tab, index } => {
            wb.tabs.iter().position(|t| t.id == *tab).is_some_and(|p| p == (*index).min(wb.tabs.len().saturating_sub(1)))
        }
        _ => false,
    }
}

/// Checks what the engine would only find out part way: formulas that can't be read, and protections.
pub(crate) fn check(wb: &Workbook, op: &Op, ctx: &Ctx) -> ApiResult<()> {
    validate_formulas(wb, op)?;
    protection(wb, op, ctx)
}

/// Positions of ids as they will be once the `Extend`s of an operation ran: ids not given out yet come after the
/// last position, in order.
struct Virtual<'a> {
    wb: &'a Workbook,
}

impl Virtual<'_> {
    fn axis_pos(axis: &calc_engine::model::Axis, id: u32) -> u32 {
        axis.pos_of(id).unwrap_or_else(|| if id >= axis.next_id() { axis.len() + (id - axis.next_id()) } else { u32::MAX })
    }

    fn pos(&self, tab: TabId, row: RowId, col: ColId) -> Option<(u32, u32)> {
        let t = self.wb.tab(tab)?;
        let (r, c) = (Self::axis_pos(&t.rows, row), Self::axis_pos(&t.cols, col));
        (r != u32::MAX && c != u32::MAX).then_some((r, c))
    }

    fn rect(&self, tab: TabId, a: &calc_engine::model::IdRect) -> Option<Rect> {
        let (r1, c1) = self.pos(tab, a.r1, a.c1)?;
        let (r2, c2) = self.pos(tab, a.r2, a.c2)?;
        Some(Rect::new(r1, c1, r2, c2))
    }
}

fn check_write(wb: &Workbook, tab: TabId, w: &CellWrite) -> ApiResult<()> {
    let at = Virtual { wb }.pos(tab, w.row, w.col);
    let formula = if let Some(input) = &w.input {
        match parse_entry(input) {
            Entry::Formula(f) => at.map(|p| (f, p)),
            _ => None,
        }
    } else {
        w.formula.as_ref().map(|f| (f.text.clone(), (f.row, f.col)))
    };
    if let Some((text, (r, c))) = formula
        && let Err(e) = parse_formula(&text, r, c, &wb.names())
    {
        let (pr, pc) = at.unwrap_or((r, c));
        let cell = CellPos::new(pr, pc).to_string();
        return Err(ApiError::unprocessable(
            "bad_formula",
            format!("The formula in {cell} can't be read: {}", e.message),
            "Fix the formula; formulas use Excel's syntax, like =SUM(A1:A9).",
        )
        .with_details(json!({"cell": cell, "reason": e.message, "at": e.at})));
    }
    Ok(())
}

fn validate_formulas(wb: &Workbook, op: &Op) -> ApiResult<()> {
    let mut flat = Vec::new();
    flatten(op, &mut flat);
    for o in flat {
        match o {
            Op::SetCells { tab, cells } => {
                for w in cells {
                    check_write(wb, *tab, w)?;
                }
            }
            Op::RestoreRows { tab, cells, formulas, .. } | Op::RestoreCols { tab, cells, formulas, .. } => {
                for w in cells {
                    check_write(wb, *tab, w)?;
                }
                for (t, w) in formulas {
                    check_write(wb, *t, w)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The tab an operation changes, for protections covering a whole tab.
pub(crate) fn op_tab(o: &Op) -> Option<TabId> {
    match o {
        Op::Extend { tab, .. }
        | Op::SetCells { tab, .. }
        | Op::StyleArea { tab, .. }
        | Op::ClearArea { tab, .. }
        | Op::InsertRows { tab, .. }
        | Op::InsertCols { tab, .. }
        | Op::DeleteRows { tab, .. }
        | Op::DeleteCols { tab, .. }
        | Op::RestoreRows { tab, .. }
        | Op::RestoreCols { tab, .. }
        | Op::Reorder { tab, .. }
        | Op::DeleteTab { tab }
        | Op::RenameTab { tab, .. }
        | Op::MoveTab { tab, .. }
        | Op::ReplaceTab { tab, .. }
        | Op::TabProps { tab, .. }
        | Op::ColWidths { tab, .. }
        | Op::RowHeights { tab, .. }
        | Op::Hide { tab, .. }
        | Op::Merge { tab, .. }
        | Op::Unmerge { tab, .. }
        | Op::AddTab { tab, .. }
        | Op::RestoreTab { tab, .. } => Some(*tab),
        Op::PutChart { chart } => Some(chart.tab),
        Op::PutProtection { protection } => Some(protection.tab),
        Op::SetNames { .. } | Op::DeleteChart { .. } | Op::DeleteProtection { .. } | Op::Settings { .. } | Op::Batch { .. } => None,
    }
}

fn has_extend(op: &Op) -> bool {
    match op {
        Op::Extend { .. } => true,
        Op::Batch { ops } => ops.iter().any(has_extend),
        _ => false,
    }
}

/// `Workbook::protection_blocking`, for operations that give cells ids first: positions are read as they will be
/// once the ids exist, so writing next to a protected range isn't mistaken for writing into it.
fn blocking_with_new_ids<'a>(wb: &'a Workbook, op: &Op, account: &str) -> Option<&'a Protection> {
    let v = Virtual { wb };
    let check = |tab: TabId, rect: Option<Rect>| {
        wb.protections.iter().find(|p| {
            if p.tab != tab || p.editors.iter().any(|e| e == account) {
                return false;
            }
            match (&p.area, rect) {
                (None, _) => true,
                (Some(a), Some(r)) => wb.tab(tab).and_then(|t| t.pos_rect(a)).is_some_and(|pr| pr.intersects(&r)),
                (Some(_), None) => true,
            }
        })
    };
    let mut flat = Vec::new();
    flatten(op, &mut flat);
    for o in flat {
        let found = match o {
            Op::Extend { .. } => None,
            Op::SetCells { tab, cells } => {
                cells.iter().find_map(|w| v.pos(*tab, w.row, w.col).and_then(|(r, c)| check(*tab, Some(Rect::new(r, c, r, c)))))
            }
            Op::StyleArea { tab, area, .. } | Op::ClearArea { tab, area, .. } | Op::Merge { tab, area } | Op::Unmerge { tab, area } => {
                check(*tab, v.rect(*tab, area))
            }
            Op::InsertRows { tab, .. }
            | Op::InsertCols { tab, .. }
            | Op::DeleteRows { tab, .. }
            | Op::DeleteCols { tab, .. }
            | Op::RestoreRows { tab, .. }
            | Op::RestoreCols { tab, .. }
            | Op::Reorder { tab, .. }
            | Op::DeleteTab { tab }
            | Op::RenameTab { tab, .. }
            | Op::ReplaceTab { tab, .. }
            | Op::TabProps { tab, .. }
            | Op::Hide { tab, .. } => check(*tab, None),
            Op::PutProtection { .. } | Op::DeleteProtection { .. } => wb.protections.first(),
            _ => None,
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

fn protection(wb: &Workbook, op: &Op, ctx: &Ctx) -> ApiResult<()> {
    if ctx.author == ctx.owner || wb.protections.is_empty() {
        return Ok(());
    }
    let found = if has_extend(op) {
        blocking_with_new_ids(wb, op, &ctx.author)
    } else {
        wb.protection_blocking(op, &ctx.author, &ctx.owner)
    };
    if let Some(p) = found {
        return Err(protected(wb, p));
    }
    // A protection on a whole tab covers everything on the tab, sizes and merges included.
    let mut flat = Vec::new();
    flatten(op, &mut flat);
    for o in flat {
        if matches!(o, Op::Extend { .. } | Op::MoveTab { .. }) {
            continue;
        }
        if let Some(t) = op_tab(o)
            && let Some(p) =
                wb.protections.iter().find(|p| p.tab == t && p.area.is_none() && !p.editors.iter().any(|e| *e == ctx.author))
        {
            return Err(protected(wb, p));
        }
    }
    Ok(())
}

/// 403 `protected`, naming the protection.
pub(crate) fn protected(wb: &Workbook, p: &Protection) -> ApiError {
    let tab_name = wb.tab(p.tab).map(|t| t.name.clone()).unwrap_or_default();
    let place = match p.area.as_ref().and_then(|a| wb.tab(p.tab).and_then(|t| t.pos_rect(a))) {
        Some(r) => format!("{r} in {tab_name}"),
        None => format!("the whole tab {tab_name}"),
    };
    let what = if p.description.trim().is_empty() { String::new() } else { format!(" (\"{}\")", p.description.trim()) };
    ApiError::new(
        StatusCode::FORBIDDEN,
        "protected",
        format!(
            "This change touches a protected range{what}: {place}. Only the Carbons and Silicons its owner chose, and the sheet's owner, can change it."
        ),
        "Ask the sheet's owner to add you to the protection, or change cells outside it.",
    )
    .with_details(json!({"protection": view::protection_json(wb, p)}))
}

fn workbook_size(wb: &Workbook) -> u64 {
    wb.tabs.iter().map(|t| t.rows.len() as u64 * t.cols.len().max(1) as u64).sum()
}

/// How many cells an operation writes or reads, roughly.
fn op_cost(wb: &Workbook, o: &Op) -> u64 {
    match o {
        Op::SetCells { cells, .. } => cells.len() as u64,
        Op::StyleArea { tab, area, .. } | Op::ClearArea { tab, area, .. } => wb
            .tab(*tab)
            .and_then(|t| {
                let r = Virtual { wb }.rect(*tab, area)?;
                let rows = (r.r2.min(t.rows.len().saturating_sub(1)) + 1).saturating_sub(r.r1) as u64;
                let cols = (r.c2.min(t.cols.len().saturating_sub(1)) + 1).saturating_sub(r.c1) as u64;
                Some(rows * cols)
            })
            .unwrap_or(1),
        Op::ReplaceTab { content, .. } | Op::RestoreTab { content, .. } => content.rows as u64 * content.cols.max(1) as u64,
        Op::RestoreRows { cells, .. } | Op::RestoreCols { cells, .. } => cells.len() as u64,
        Op::DeleteTab { tab } => wb.tab(*tab).map_or(1, |t| t.rows.len() as u64 * t.cols.len().max(1) as u64),
        _ => 1,
    }
}

/// Light operations are applied in place: one step that can't fail once checked, or steps that only write cells.
fn classify(wb: &Workbook, op: &Op) -> Mode {
    let mut flat = Vec::new();
    flatten(op, &mut flat);
    let steps: Vec<&&Op> = flat.iter().filter(|o| !matches!(o, Op::Extend { .. })).collect();
    if steps.iter().any(|o| matches!(o, Op::RestoreTab { .. } | Op::RestoreRows { .. } | Op::RestoreCols { .. })) {
        return Mode::Heavy;
    }
    if steps.len() > 1
        && !steps.iter().all(|o| {
            matches!(o, Op::SetCells { tab, .. } | Op::StyleArea { tab, .. } | Op::ClearArea { tab, .. } if wb.tab(*tab).is_some())
        })
    {
        return Mode::Heavy;
    }
    let cost: u64 = flat.iter().map(|o| op_cost(wb, o)).sum();
    // A big change on the side lets readers keep reading meanwhile, when copying the workbook is the cheaper part.
    if cost > 100_000 && cost * 2 > workbook_size(wb) { Mode::Heavy } else { Mode::Light }
}

/// The engine's answers for several operations applied one after another, as one.
pub(crate) struct Merged {
    pub inverse: Option<Op>,
    pub cells: Vec<(TabId, Rect)>,
    pub tabs_reload: Vec<TabId>,
    pub tab_list: bool,
    pub meta: bool,
    pub overwritten: Vec<Overwrite>,
    pub skipped: usize,
    pub recalc: Vec<(TabId, u32, u32)>,
    pub overflow: Vec<TabId>,
}

pub(crate) fn merge(applied: Vec<Applied>) -> Merged {
    let mut m = Merged {
        inverse: None,
        cells: Vec::new(),
        tabs_reload: Vec::new(),
        tab_list: false,
        meta: false,
        overwritten: Vec::new(),
        skipped: 0,
        recalc: Vec::new(),
        overflow: Vec::new(),
    };
    let mut inverses = Vec::new();
    for a in applied {
        if let Some(inv) = a.inverse {
            inverses.push(inv);
        }
        m.cells.extend(a.changes.cells);
        m.tabs_reload.extend(a.changes.tabs_reload);
        m.tab_list |= a.changes.tab_list;
        m.meta |= a.changes.meta;
        m.overwritten.extend(a.overwritten);
        m.skipped += a.skipped;
        m.recalc.extend(a.recalc.cells);
        m.overflow.extend(a.recalc.overflow);
    }
    // Undoing runs the last step's inverse first.
    inverses.reverse();
    m.inverse = match inverses.len() {
        0 => None,
        1 => inverses.pop(),
        _ => Some(Op::Batch { ops: inverses }),
    };
    m.tabs_reload.sort_unstable();
    m.tabs_reload.dedup();
    m.overflow.sort_unstable();
    m.overflow.dedup();
    m
}

/// What a cell holds, as history shows it.
#[derive(Clone, Debug, PartialEq)]
enum Content {
    Empty,
    Value(calc_engine::Value),
    Formula(String),
}

/// Postgres' jsonb can't hold a NUL character.
fn safe_text(s: &str) -> String {
    s.replace('\0', "\u{FFFD}")
}

impl Content {
    fn of_value(v: &calc_engine::Value) -> Self {
        if v.is_empty() { Self::Empty } else { Self::Value(v.clone()) }
    }

    fn json(&self) -> Value {
        match self {
            Self::Empty => json!({"t": "empty"}),
            Self::Value(calc_engine::Value::Text(s)) if s.contains('\0') => json!({"t": "text", "v": safe_text(s)}),
            Self::Value(v) => view::value_json(v),
            Self::Formula(f) => Value::String(safe_text(f)),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    fn as_value(&self) -> calc_engine::Value {
        match self {
            Self::Empty => calc_engine::Value::Empty,
            Self::Value(v) => v.clone(),
            Self::Formula(f) => calc_engine::Value::from(f.as_str()),
        }
    }
}

fn content_now(wb: &Workbook, tab: TabId, row: RowId, col: ColId) -> Content {
    let Some(t) = wb.tab(tab) else { return Content::Empty };
    let (Some(r), Some(c)) = (t.rows.pos_of(row), t.cols.pos_of(col)) else { return Content::Empty };
    let cell = t.cell_by_id(row, col);
    if cell.formula.is_some() {
        return wb.formula_text(tab, r, c).map(|f| Content::Formula(format!("={f}"))).unwrap_or(Content::Empty);
    }
    if cell.spilled {
        return Content::Empty;
    }
    Content::of_value(&cell.value)
}

/// What an inverse write says the cell held; None when it only kept the cell's look (the content didn't change).
fn captured(w: &CellWrite) -> Option<Content> {
    if let Some(f) = &w.formula {
        return Some(Content::Formula(format!("={}", f.text)));
    }
    match &w.value {
        Some(v) => Some(Content::of_value(v)),
        None if w.clear => Some(Content::Empty),
        None => None,
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// A tab's name now, or the name it had when the change removed it.
fn tab_name(wb: &Workbook, inverse: &[&Op], tab: TabId) -> String {
    if let Some(t) = wb.tab(tab) {
        return t.name.clone();
    }
    for o in inverse {
        if let Op::RestoreTab { tab: t, name, .. } = o
            && *t == tab
        {
            return name.clone();
        }
    }
    format!("tab {tab}")
}

/// Joins the cell counts and phrases of one or more operations into words: "changed 340 cells in Sales".
pub(crate) fn compose(cells: &BTreeMap<String, u64>, phrases: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let total: u64 = cells.values().sum();
    if total > 0 {
        let names: Vec<&String> = cells.keys().collect();
        let place = match names.len() {
            1 => names[0].clone(),
            n => format!("{} and {}", names[..n - 1].iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "), names[n - 1]),
        };
        parts.push(format!("changed {} in {place}", plural(total, "cell", "cells")));
    }
    let mut seen = HashSet::new();
    let distinct: Vec<&String> = phrases.iter().filter(|p| seen.insert(p.as_str())).collect();
    for p in distinct.iter().take(3) {
        parts.push((*p).clone());
    }
    if distinct.len() > 3 {
        parts.push(format!("made {} more changes", distinct.len() - 3));
    }
    if parts.is_empty() {
        return "made a change".to_owned();
    }
    parts.join(", ")
}

fn phrase(wb: &Workbook, o: &Op, inverse: &[&Op]) -> Option<String> {
    let name = |t: TabId| tab_name(wb, inverse, t);
    Some(match o {
        Op::InsertRows { tab, ids, .. } => format!("inserted {} in {}", plural(ids.len() as u64, "row", "rows"), name(*tab)),
        Op::InsertCols { tab, ids, .. } => format!("inserted {} in {}", plural(ids.len() as u64, "column", "columns"), name(*tab)),
        Op::DeleteRows { tab, ids } => format!("deleted {} in {}", plural(ids.len() as u64, "row", "rows"), name(*tab)),
        Op::DeleteCols { tab, ids } => format!("deleted {} in {}", plural(ids.len() as u64, "column", "columns"), name(*tab)),
        Op::RestoreRows { tab, positions, .. } => {
            format!("brought back {} in {}", plural(positions.len() as u64, "row", "rows"), name(*tab))
        }
        Op::RestoreCols { tab, positions, .. } => {
            format!("brought back {} in {}", plural(positions.len() as u64, "column", "columns"), name(*tab))
        }
        Op::Reorder { tab, rows, .. } => format!("reordered the {} of {}", if *rows { "rows" } else { "columns" }, name(*tab)),
        Op::AddTab { name: n, .. } => format!("added the tab {n}"),
        Op::DeleteTab { tab } => format!("deleted the tab {}", name(*tab)),
        Op::RestoreTab { tab, .. } => format!("brought back the tab {}", name(*tab)),
        Op::RenameTab { tab, name: new } => {
            let old = inverse.iter().find_map(|i| match i {
                Op::RenameTab { tab: t, name } if t == tab => Some(name.clone()),
                _ => None,
            });
            match old {
                Some(old) => format!("renamed the tab {old} to {new}"),
                None => format!("renamed a tab to {new}"),
            }
        }
        Op::MoveTab { tab, .. } => format!("moved the tab {}", name(*tab)),
        Op::ReplaceTab { tab, .. } => format!("replaced the contents of {}", name(*tab)),
        Op::TabProps { tab, props } => {
            let mut what = Vec::new();
            if props.frozen_rows.is_some() || props.frozen_cols.is_some() {
                what.push("changed the frozen panes");
            }
            if props.color.is_some() {
                what.push("changed the colour");
            }
            if props.code.is_some() {
                what.push("changed the code");
            }
            if what.is_empty() {
                return None;
            }
            format!("{} of {}", what.join(" and "), name(*tab))
        }
        Op::ColWidths { tab, .. } => format!("resized columns in {}", name(*tab)),
        Op::RowHeights { tab, .. } => format!("resized rows in {}", name(*tab)),
        Op::Hide { tab, rows, hidden, .. } => format!(
            "{} {} in {}",
            if *hidden { "hid" } else { "showed" },
            if *rows { "rows" } else { "columns" },
            name(*tab)
        ),
        Op::Merge { tab, .. } => format!("merged cells in {}", name(*tab)),
        Op::Unmerge { tab, .. } => format!("unmerged cells in {}", name(*tab)),
        Op::SetNames { .. } => "changed the named ranges".to_owned(),
        Op::PutChart { chart } => {
            let added = inverse.iter().any(|i| matches!(i, Op::DeleteChart { id } if *id == chart.id));
            if added { format!("added a chart to {}", name(chart.tab)) } else { format!("changed a chart in {}", name(chart.tab)) }
        }
        Op::DeleteChart { .. } => "deleted a chart".to_owned(),
        Op::PutProtection { protection } => {
            let added = inverse.iter().any(|i| matches!(i, Op::DeleteProtection { id } if *id == protection.id));
            let place = protection
                .area
                .as_ref()
                .and_then(|a| wb.tab(protection.tab).and_then(|t| t.pos_rect(a)))
                .map(|r| format!("{r} in {}", name(protection.tab)))
                .unwrap_or_else(|| name(protection.tab));
            if added { format!("protected {place}") } else { format!("changed the protection of {place}") }
        }
        Op::DeleteProtection { .. } => "removed a protection".to_owned(),
        Op::Settings { .. } => "changed the sheet's settings".to_owned(),
        Op::Extend { .. } | Op::SetCells { .. } | Op::StyleArea { .. } | Op::ClearArea { .. } | Op::Batch { .. } => {
            return None;
        }
    })
}

fn rect_cells(r: &Rect) -> u64 {
    r.rows() as u64 * r.cols() as u64
}

fn bounding(rects: &[Rect]) -> Rect {
    let mut b = rects[0];
    for r in &rects[1..] {
        b = Rect::new(b.r1.min(r.r1), b.c1.min(r.c1), b.r2.max(r.r2), b.c2.max(r.c2));
    }
    b
}

/// Whether an operation changes what cells hold (not only how they look or how big they are), for code tabs that
/// read them.
fn changes_contents(o: &Op) -> Option<TabId> {
    match o {
        Op::SetCells { tab, .. }
        | Op::ClearArea { tab, .. }
        | Op::InsertRows { tab, .. }
        | Op::InsertCols { tab, .. }
        | Op::DeleteRows { tab, .. }
        | Op::DeleteCols { tab, .. }
        | Op::RestoreRows { tab, .. }
        | Op::RestoreCols { tab, .. }
        | Op::Reorder { tab, .. }
        | Op::ReplaceTab { tab, .. }
        | Op::RestoreTab { tab, .. } => Some(*tab),
        _ => None,
    }
}

/// Describes an applied change, reading the workbook as it is now.
pub(crate) fn describe(wb: &Workbook, ops: &[Op], m: &Merged, ctx: &Ctx) -> Info {
    let mut flat = Vec::new();
    for op in ops {
        flatten(op, &mut flat);
    }
    let mut inv_flat = Vec::new();
    if let Some(inv) = &m.inverse {
        flatten(inv, &mut inv_flat);
    }

    // What each written cell held before: the inverse's captures. The inverse runs last step first, so its last
    // capture of a cell is the oldest state.
    let mut before: HashMap<(TabId, RowId, ColId), &CellWrite> = HashMap::new();
    for o in &inv_flat {
        if let Op::SetCells { tab, cells } = o {
            for w in cells {
                before.insert((*tab, w.row, w.col), w);
            }
        }
    }
    // The engine's record of each overwritten cell; the first one is what the cell held before the change.
    let overwritten: HashMap<(TabId, RowId, ColId), &Overwrite> =
        m.overwritten.iter().rev().map(|o| ((o.tab, o.row, o.col), o)).collect();

    // Every cell the change may have written.
    let mut touched: Vec<(TabId, RowId, ColId)> = Vec::new();
    let mut seen: HashSet<(TabId, RowId, ColId)> = HashSet::new();
    for o in &flat {
        match o {
            Op::SetCells { tab, cells } => {
                for w in cells {
                    if seen.insert((*tab, w.row, w.col)) {
                        touched.push((*tab, w.row, w.col));
                    }
                }
            }
            Op::RestoreRows { tab, cells, formulas, .. } | Op::RestoreCols { tab, cells, formulas, .. } => {
                for w in cells {
                    if seen.insert((*tab, w.row, w.col)) {
                        touched.push((*tab, w.row, w.col));
                    }
                }
                for (t, w) in formulas {
                    if seen.insert((*t, w.row, w.col)) {
                        touched.push((*t, w.row, w.col));
                    }
                }
            }
            _ => {}
        }
    }
    let mut more: Vec<(TabId, RowId, ColId)> = before.keys().filter(|k| !seen.contains(*k)).copied().collect();
    more.sort_unstable();
    touched.extend(more);

    let bulk_cells = touched.len() > HISTORY_CELLS;
    let mut per_tab: BTreeMap<TabId, u64> = BTreeMap::new();
    let mut history = Vec::new();
    let mut candidates = Vec::new();
    for &(tab, row, col) in &touched {
        let Some(t) = wb.tab(tab) else { continue };
        let (Some(r), Some(c)) = (t.rows.pos_of(row), t.cols.pos_of(col)) else { continue };
        if bulk_cells {
            *per_tab.entry(tab).or_default() += 1;
            continue;
        }
        let after = content_now(wb, tab, row, col);
        let capture = before.get(&(tab, row, col));
        let was = capture.and_then(|w| captured(w)).unwrap_or_else(|| after.clone());
        let style_changed = capture
            .and_then(|w| w.style.as_ref())
            .is_some_and(|s| s != wb.style_of(t.cell_by_id(row, col).style));
        if after != was || style_changed {
            *per_tab.entry(tab).or_default() += 1;
        }
        if after == was {
            continue;
        }
        if !was.is_empty() {
            let prev = overwritten.get(&(tab, row, col));
            let author = prev.and_then(|o| unpack_meta(o.prev_meta)).and_then(|(a, at)| wb.authors.get(a).map(|u| (u.to_owned(), at)));
            candidates.push(Candidate {
                tab,
                row,
                col,
                address: CellPos::new(r, c).to_string(),
                before: prev.map(|o| o.prev_value.clone()).unwrap_or_else(|| was.as_value()),
                prev_author: author.as_ref().map(|a| a.0.clone()),
                prev_at: author.map(|a| a.1),
            });
        }
        history.push(HistoryRow { tab, row, col, before: was.json(), after: after.json() });
    }

    // The tabs it touched.
    let mut op_tabs: BTreeMap<TabId, OpTab> = BTreeMap::new();
    for o in &flat {
        let bulk = matches!(o, Op::ReplaceTab { .. } | Op::RestoreTab { .. });
        if let Some(t) = op_tab(o) {
            let e = op_tabs.entry(t).or_insert(OpTab { tab: t, bulk: false, area: None });
            if bulk {
                e.bulk = true;
                e.area = None;
            }
        }
    }
    if bulk_cells {
        // The area around the cells written, by corner ids, so history can tell which cells it covers.
        let mut by_tab: BTreeMap<TabId, Vec<Rect>> = BTreeMap::new();
        for &(tab, row, col) in &touched {
            if let Some(t) = wb.tab(tab)
                && let (Some(r), Some(c)) = (t.rows.pos_of(row), t.cols.pos_of(col))
            {
                by_tab.entry(tab).or_default().push(Rect::new(r, c, r, c));
            }
        }
        for (tab, rects) in by_tab {
            let b = bounding(&rects);
            let t = wb.tab(tab).expect("checked");
            let area = match (t.rows.id_at(b.r1), t.cols.id_at(b.c1), t.rows.id_at(b.r2), t.cols.id_at(b.c2)) {
                (Some(a), Some(b2), Some(c), Some(d)) => Some((a, b2, c, d)),
                _ => None,
            };
            let e = op_tabs.entry(tab).or_insert(OpTab { tab, bulk: false, area: None });
            if !e.bulk {
                e.bulk = true;
                e.area = area;
            }
        }
    }

    // Changes, for viewers and the answer.
    let reload: BTreeSet<TabId> = m.tabs_reload.iter().chain(m.overflow.iter()).copied().collect();
    let mut rects: BTreeMap<TabId, Vec<Rect>> = BTreeMap::new();
    for (t, r) in &m.cells {
        rects.entry(*t).or_default().push(*r);
    }
    for &(t, r, c) in &m.recalc {
        rects.entry(t).or_default().push(Rect::new(r, c, r, c));
    }
    let mut changes = Vec::new();
    let mut event_positions: BTreeSet<(TabId, u32, u32)> = BTreeSet::new();
    let mut event_too_many = false;
    let mut used = Vec::new();
    for (t, list) in &mut rects {
        if reload.contains(t) || wb.tab(*t).is_none() {
            continue;
        }
        list.sort_unstable_by_key(|r| (r.r1, r.c1, r.r2, r.c2));
        list.dedup();
        let total: u64 = list.iter().map(rect_cells).sum();
        if !event_too_many && event_positions.len() as u64 + total <= EVENT_CELLS as u64 {
            for r in list.iter() {
                for row in r.r1..=r.r2 {
                    for col in r.c1..=r.c2 {
                        event_positions.insert((*t, row, col));
                    }
                }
            }
        } else {
            event_too_many = true;
        }
        let tab = wb.tab(*t).expect("checked");
        if total > 2_000 {
            used.push(UsedChange::Invalidate(*t));
        } else {
            let mut cells = Vec::new();
            for r in list.iter() {
                for row in r.r1..=r.r2 {
                    for col in r.c1..=r.c2 {
                        let cell = tab.cell_at(CellPos::new(row, col));
                        cells.push((row, col, !cell.value.is_empty() || cell.formula.is_some()));
                    }
                }
            }
            used.push(UsedChange::Cells(*t, cells));
        }
        let shown: Vec<Rect> = if list.len() > MAX_RANGES { vec![bounding(list)] } else { list.clone() };
        for r in shown {
            changes.push(json!({"tab": t, "range": r.to_string()}));
        }
    }
    for t in &reload {
        if wb.tab(*t).is_some() {
            changes.push(json!({"tab": t, "reload": true}));
        }
        used.push(UsedChange::Invalidate(*t));
    }
    if m.tab_list {
        let mut added = false;
        for o in &flat {
            if let Op::AddTab { tab, .. } = o
                && let Some(t) = wb.tab(*tab)
            {
                changes.push(json!({"tabs": true, "tab": tab, "name": t.name}));
                added = true;
            }
            if let Op::RestoreTab { tab, .. } | Op::DeleteTab { tab } = o {
                used.push(UsedChange::Invalidate(*tab));
            }
        }
        if !added {
            changes.push(json!({"tabs": true}));
        }
    }
    if m.meta {
        changes.push(json!({"meta": true}));
    }

    let event_cells = (!event_too_many).then(|| {
        let (cells, styles, uuids) = view::event_cells(wb, &event_positions);
        EventCells { cells, styles, uuids }
    });

    // What history says.
    let mut phrases = Vec::new();
    for o in &flat {
        if let Some(p) = phrase(wb, o, &inv_flat) {
            phrases.push(p);
        }
    }
    let mut cells_by_name: BTreeMap<String, u64> = BTreeMap::new();
    for (t, n) in &per_tab {
        if *n > 0 {
            *cells_by_name.entry(tab_name(wb, &inv_flat, *t)).or_default() += n;
        }
    }
    let mut cells_changed: i64 = per_tab.values().sum::<u64>() as i64;
    for o in &flat {
        if let Op::ReplaceTab { content, .. } | Op::RestoreTab { content, .. } = o {
            let n: i64 = content.columns.iter().map(|c| c.values.iter().filter(|v| !v.is_empty()).count() as i64).sum();
            cells_changed += n;
            if let Some(t) = op_tab(o) {
                *cells_by_name.entry(tab_name(wb, &inv_flat, t)).or_default() += n as u64;
            }
        }
    }
    let (summary, facts) = match &ctx.summary {
        Some(s) => (s.clone(), json!({"cells": {}, "phrases": [safe_text(s)], "given": true})),
        None => {
            let names: serde_json::Map<String, Value> = cells_by_name.iter().map(|(k, v)| (safe_text(k), json!(v))).collect();
            let ph: Vec<String> = phrases.iter().map(|p| safe_text(p)).collect();
            (compose(&cells_by_name, &phrases), json!({"cells": names, "phrases": ph}))
        }
    };

    let mut tabs_changed: BTreeSet<TabId> = BTreeSet::new();
    for o in &flat {
        if let Some(t) = changes_contents(o) {
            tabs_changed.insert(t);
        }
    }
    for (t, _, _) in &m.recalc {
        tabs_changed.insert(*t);
    }
    tabs_changed.extend(m.overflow.iter().copied());
    let tabs_changed: Vec<TabId> = tabs_changed.into_iter().filter(|t| wb.tab(*t).is_some()).collect();

    Info {
        changes,
        event_cells,
        history,
        op_tabs: op_tabs.into_values().collect(),
        facts,
        summary: safe_text(&summary),
        cells_changed,
        skipped: m.skipped,
        tabs_changed,
        candidates,
        used,
    }
}

/// Keeps the cached used areas right after a change applied in place.
pub(crate) fn update_used(st: &LineState, changes: &[UsedChange]) {
    let mut used = st.used.lock().expect("used lock");
    for ch in changes {
        match ch {
            UsedChange::Invalidate(t) => {
                used.remove(t);
            }
            UsedChange::Cells(t, cells) => {
                let Some(mut cur) = used.get(t).copied() else { continue };
                let mut stale = false;
                for &(r, c, holds) in cells {
                    if holds {
                        cur = Some(match cur {
                            Some(u) => Rect::new(0, 0, u.r2.max(r), u.c2.max(c)),
                            None => Rect::new(0, 0, r, c),
                        });
                    } else if let Some(u) = cur
                        && (r == u.r2 || c == u.c2)
                    {
                        // Something on the edge was emptied: the area may shrink.
                        stale = true;
                    }
                }
                if stale {
                    used.remove(t);
                } else {
                    used.insert(*t, cur);
                }
            }
        }
    }
}
