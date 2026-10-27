//! Operations: every change to a workbook is one `Op`, applied in order. Each application returns the operation
//! that undoes it, what changed (for viewers) and which cells it overwrote (so no change silently replaces
//! another).
//!
//! Operations name cells by row and column **ids**, so they mean the same thing however rows move around
//! them. The service turns positions into ids with the `prepare_*` helpers right before applying, inside the one
//! task that owns the workbook, so ids for rows that don't exist yet are allocated deterministically (`Extend`).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{
    addr::{CellPos, MAX_COLS, MAX_ROWS, Rect},
    formula::{
        ast::{Expr, TabId},
        parse::to_a1,
    },
    input::{Entry, parse_entry},
    model::{Chart, CodeSource, ColId, DefinedName, IdRect, Meta, Protection, RowId, Tab, pack_meta},
    style::{Style, StylePatch},
    value::Value,
    workbook::{AxisChange, Recalc, Settings, Workbook},
};

/// A formula with the position it was written at, so its relative references keep their meaning wherever the
/// cell has moved to since.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormulaAt {
    /// Without the leading `=`.
    pub text: String,
    pub row: u32,
    pub col: u32,
}

/// What to write into one cell. Fields are applied in this order: `clear`, then `input` / `formula` / `value`
/// (the first present), then `style`, then `style_patch`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CellWrite {
    pub row: RowId,
    pub col: ColId,
    /// Remove the value and formula first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear: bool,
    /// What someone typed, read the way typing into the cell reads it: `=A1*2`, `12%`, `2026-10-17`, `'0042`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<FormulaAt>,
    /// A value taken as it is (imports, code results, restores).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The cell's whole style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<Style>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_patch: Option<StylePatch>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClearWhat {
    /// Values and formulas.
    Contents,
    /// Styles.
    Formats,
    All,
}

/// A whole tab's contents, column by column (imports, code results, restoring a deleted tab).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabContent {
    pub rows: u32,
    pub cols: u32,
    pub columns: Vec<ColumnContent>,
    /// Styles the cells point at by index.
    #[serde(default)]
    pub styles: Vec<Style>,
    #[serde(default)]
    pub col_widths: Vec<(u32, f32)>,
    #[serde(default)]
    pub row_heights: Vec<(u32, f32)>,
    #[serde(default)]
    pub frozen_rows: u32,
    #[serde(default)]
    pub frozen_cols: u32,
    /// Merged areas by position.
    #[serde(default)]
    pub merges: Vec<Rect>,
    #[serde(default)]
    pub hidden_rows: Vec<u32>,
    #[serde(default)]
    pub hidden_cols: Vec<u32>,
    #[serde(default)]
    pub code: Option<CodeSource>,
    #[serde(default)]
    pub format_rules: Vec<crate::conditional::PositionRule>,
}

/// One column of a `TabContent`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnContent {
    /// Values from row 0 down; may be shorter than the tab (the rest is empty).
    pub values: Vec<Value>,
    /// `(row, formula text written at that row and this column)`.
    #[serde(default)]
    pub formulas: Vec<(u32, String)>,
    /// `(row, index into TabContent::styles)`.
    #[serde(default)]
    pub styles: Vec<(u32, u32)>,
}

impl TabContent {
    /// A content of plain values, row by row (what a CSV or a query result gives).
    pub fn from_rows(rows: Vec<Vec<Value>>) -> Self {
        let n_rows = rows.len() as u32;
        let n_cols = rows.iter().map(Vec::len).max().unwrap_or(0) as u32;
        let mut columns: Vec<ColumnContent> = (0..n_cols).map(|_| ColumnContent::default()).collect();
        for col in columns.iter_mut() {
            col.values.reserve(rows.len());
        }
        for row in rows {
            let len = row.len();
            for (c, v) in row.into_iter().enumerate() {
                columns[c].values.push(v);
            }
            for col in columns.iter_mut().skip(len) {
                col.values.push(Value::Empty);
            }
        }
        for col in columns.iter_mut() {
            while col.values.last().is_some_and(Value::is_empty) {
                col.values.pop();
            }
        }
        Self { rows: n_rows, cols: n_cols, columns, ..Default::default() }
    }
}

/// Properties of a tab that change as one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabProps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_rows: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_cols: Option<u32>,
    /// `Some(None)` removes the colour.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::style::double_option")]
    pub color: Option<Option<String>>,
    /// `Some(None)` turns a code-made tab into an ordinary one.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::style::double_option")]
    pub code: Option<Option<CodeSource>>,
    #[serde(default)]
    pub format_rules: Option<Vec<crate::conditional::Rule>>,
}

/// One change to a workbook.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Gives positions up to `rows` × `cols` ids (rows and columns that were empty grid until now).
    Extend { tab: TabId, rows: u32, cols: u32 },
    SetCells { tab: TabId, cells: Vec<CellWrite> },
    StyleArea { tab: TabId, area: IdRect, patch: StylePatch },
    ClearArea { tab: TabId, area: IdRect, what: ClearWhat },
    InsertRows { tab: TabId, before: Option<RowId>, ids: Vec<RowId> },
    InsertCols { tab: TabId, before: Option<ColId>, ids: Vec<ColId> },
    DeleteRows { tab: TabId, ids: Vec<RowId> },
    DeleteCols { tab: TabId, ids: Vec<ColId> },
    /// Puts deleted rows back at their positions with their cells, then restores formulas elsewhere that the
    /// deletion had turned into `#REF!`.
    RestoreRows { tab: TabId, positions: Vec<(u32, RowId)>, cells: Vec<CellWrite>, formulas: Vec<(TabId, CellWrite)> },
    RestoreCols { tab: TabId, positions: Vec<(u32, ColId)>, cells: Vec<CellWrite>, formulas: Vec<(TabId, CellWrite)> },
    /// The positions currently holding `ids` get them in this order (sorting, moving).
    Reorder { tab: TabId, rows: bool, ids: Vec<u32> },
    AddTab { tab: TabId, name: String, index: Option<usize> },
    DeleteTab { tab: TabId },
    /// Brings a deleted tab back, with the formulas elsewhere that pointed at it.
    RestoreTab { tab: TabId, name: String, index: usize, content: Box<TabContent>, formulas: Vec<(TabId, CellWrite)> },
    RenameTab { tab: TabId, name: String },
    MoveTab { tab: TabId, index: usize },
    ReplaceTab { tab: TabId, content: Box<TabContent> },
    TabProps { tab: TabId, props: TabProps },
    ColWidths { tab: TabId, widths: Vec<(ColId, Option<f32>)> },
    RowHeights { tab: TabId, heights: Vec<(RowId, Option<f32>)> },
    Hide { tab: TabId, rows: bool, ids: Vec<u32>, hidden: bool },
    Merge { tab: TabId, area: IdRect },
    Unmerge { tab: TabId, area: IdRect },
    SetNames { names: Vec<DefinedName> },
    SetFunctions { functions: Vec<crate::javascript::Function> },
    JavascriptResults { epoch: u64, results: Vec<(u64, crate::javascript::Answer)> },
    PutChart { chart: Chart },
    DeleteChart { id: String },
    PutProtection { protection: Protection },
    DeleteProtection { id: String },
    Settings { settings: Settings },
    Batch { ops: Vec<Op> },
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum OpError {
    #[error("There is no tab with id {0} in this sheet.")]
    NoSuchTab(TabId),
    #[error("A tab named \"{0}\" already exists in this sheet.")]
    TabNameTaken(String),
    #[error("A tab name can't be empty, longer than 100 characters, or contain any of : \\ / ? * [ ].")]
    BadTabName,
    #[error("A sheet needs at least one tab.")]
    LastTab,
    #[error("The formula in {cell} can't be read: {message}")]
    Formula { cell: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

/// A cell overwritten by an operation, with who had changed it last and when.
#[derive(Clone, Debug, PartialEq)]
pub struct Overwrite {
    pub tab: TabId,
    pub row: RowId,
    pub col: ColId,
    pub prev_meta: Meta,
    pub prev_value: Value,
}

/// What changed, for telling viewers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    /// Cells written directly (by position).
    pub cells: Vec<(TabId, Rect)>,
    /// Tabs whose rows or columns moved, or whose content was replaced: viewers reload them.
    pub tabs_reload: Vec<TabId>,
    /// The list of tabs (names, order) changed.
    pub tab_list: bool,
    /// Names, charts, protections or settings changed.
    pub meta: bool,
}

/// The result of applying an operation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// The operation that undoes this one.
    pub inverse: Option<Op>,
    pub changes: Changes,
    pub overwritten: Vec<Overwrite>,
    /// Cell writes skipped because their row or column no longer exists.
    pub skipped: usize,
    pub recalc: Recalc,
}

/// Who is applying an operation and when.
#[derive(Clone, Debug)]
pub struct Actor<'a> {
    /// Account uuid.
    pub account: &'a str,
    /// Unix seconds.
    pub at: i64,
}

pub fn valid_tab_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= 100 && !n.contains([':', '\\', '/', '?', '*', '[', ']']) && n == name
}

/// A formula's text, as `FormulaAt`, for a cell at a position.
fn formula_at(expr: &Expr, row: u32, col: u32, wb: &Workbook) -> FormulaAt {
    FormulaAt { text: to_a1(expr, row, col, &wb.names()), row, col }
}

impl Workbook {
    fn tab_or(&self, tab: TabId) -> Result<&Tab, OpError> {
        self.tab(tab).ok_or(OpError::NoSuchTab(tab))
    }

    fn tab_mut_or(&mut self, tab: TabId) -> Result<&mut Tab, OpError> {
        self.tab_mut(tab).ok_or(OpError::NoSuchTab(tab))
    }

    /// The whole content of a cell, as a write that recreates it.
    pub(crate) fn capture_cell(&self, tab: TabId, row: RowId, col: ColId) -> CellWrite {
        let Some(t) = self.tab(tab) else { return CellWrite { row, col, clear: true, ..Default::default() } };
        let cell = t.cell_by_id(row, col);
        let formula = match (cell.formula, t.rows.pos_of(row), t.cols.pos_of(col)) {
            (Some(g), Some(r), Some(c)) => self.groups.get(g).map(|g| formula_at(&g.expr, r, c, self)),
            _ => None,
        };
        CellWrite {
            row,
            col,
            clear: true,
            input: None,
            value: if formula.is_none() && !cell.spilled { Some(cell.value.clone()) } else { None },
            formula,
            style: Some(self.styles.get(cell.style).clone()),
            style_patch: None,
        }
    }

    /// A tab's whole content.
    pub fn capture_tab(&self, tab: TabId) -> Option<TabContent> {
        let t = self.tab(tab)?;
        let mut styles: Vec<Style> = Vec::new();
        let mut style_ix: ahash::HashMap<u32, u32> = Default::default();
        let (rows, cols) = (t.rows.len(), t.cols.len());
        let mut columns = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            let mut col = ColumnContent::default();
            let cid = t.cols.id_at(c).expect("in range");
            for r in 0..rows {
                let rid = t.rows.id_at(r).expect("in range");
                let cell = t.cell_by_id(rid, cid);
                if let Some(g) = cell.formula {
                    if let Some(g) = self.groups.get(g) {
                        col.formulas.push((r, to_a1(&g.expr, r, c, &self.names())));
                    }
                }
                let v = if cell.spilled { Value::Empty } else { cell.value };
                if !v.is_empty() || cell.formula.is_some() {
                    while (col.values.len() as u32) < r {
                        col.values.push(Value::Empty);
                    }
                    col.values.push(v);
                }
                if cell.style != 0 {
                    let next = styles.len() as u32;
                    let ix = *style_ix.entry(cell.style).or_insert_with(|| {
                        styles.push(self.styles.get(cell.style).clone());
                        next
                    });
                    col.styles.push((r, ix));
                }
            }
            columns.push(col);
        }
        let pos_list = |ids: &[u32], axis: &crate::model::Axis| -> Vec<u32> { ids.iter().filter_map(|&i| axis.pos_of(i)).collect() };
        Some(TabContent {
            rows,
            cols,
            columns,
            styles,
            col_widths: t.col_widths.iter().filter_map(|(&id, &w)| Some((t.cols.pos_of(id)?, w))).collect(),
            row_heights: t.row_heights.iter().filter_map(|(&id, &h)| Some((t.rows.pos_of(id)?, h))).collect(),
            frozen_rows: t.frozen_rows,
            frozen_cols: t.frozen_cols,
            merges: t.merges.iter().filter_map(|m| t.pos_rect(m)).collect(),
            hidden_rows: pos_list(&t.hidden_rows, &t.rows),
            hidden_cols: pos_list(&t.hidden_cols, &t.cols),
            code: t.code.clone(),
            format_rules: t.format_rules.iter().filter_map(|r| Some(crate::conditional::PositionRule {id:r.id.clone(),range:t.pos_rect(&r.area)?,kind:r.kind.clone()})).collect(),
        })
    }

    /// Marks every formula that lives in or reads `tab` for recalculation.
    fn dirty_tab_formulas(&mut self, tab: TabId) {
        let ids: Vec<u32> = self
            .groups
            .ids()
            .filter(|&g| self.groups.get(g).is_some_and(|g| g.tab == tab || g.refs.iter().any(|p| p.tab.unwrap_or(g.tab) == tab)))
            .collect();
        for gid in ids {
            let Some(g) = self.groups.get(gid) else { continue };
            let gtab = g.tab;
            let members: Vec<(RowId, ColId)> = g.members.iter().copied().collect();
            let Some(t) = self.tab(gtab) else { continue };
            let keys: Vec<(TabId, u32, u32)> = members
                .into_iter()
                .filter_map(|(r, c)| Some((gtab, t.cols.pos_of(c)?, t.rows.pos_of(r)?)))
                .collect();
            self.dirty.extend(keys);
        }
    }

    /// Applies one operation and recalculates.
    pub fn apply(&mut self, op: &Op, actor: &Actor<'_>) -> Result<Applied, OpError> {
        let reads_tabs = self.javascript.lock().unwrap().calls.iter().any(|c| c.result.as_ref().and_then(|a| a.dependencies.as_ref()).is_some_and(|d| d.tabs));
        let tab_metadata = |wb: &Workbook| wb.tabs.iter().map(|t| (t.id, t.name.clone(), t.used_rect())).collect::<Vec<_>>();
        let mut metadata = reads_tabs.then(|| tab_metadata(self));
        self.set_clock(actor.at as f64);
        let mut out = Applied::default();
        let inverse = self.apply_inner(op, actor, &mut out)?;
        out.inverse = inverse;
        let changes = std::mem::take(&mut out.changes.cells);
        // Big writes are reported as their bounding rectangle per tab.
        let changes = if changes.len() > 50_000 { bounding(&changes) } else { changes };
        {
            let mut js = self.javascript.lock().unwrap();
            if !matches!(op, Op::JavascriptResults { .. }) { js.epoch += 1; }
            if resets_javascript(op) { js.calls.clear(); js.bytes = 0; }
            else if changes_javascript_inputs(op) { js.invalidate_reads(&changes, out.changes.tab_list, true); }
            js.used.clear();
        }
        self.format_stats.lock().unwrap().clear();
        out.recalc = self.recalc(changes.clone());
        // A range read can depend on a normal formula or another JavaScript result.
        // Propagate actual value changes until every cached read is up to date.
        let mut changed = out.recalc.clone();
        loop {
            let next_metadata = metadata.as_ref().map(|_| tab_metadata(self));
            let metadata_changed = metadata != next_metadata;
            metadata = next_metadata;
            let mut reads: Vec<_> = changed.cells.iter().map(|&(t,r,c)| (t, Rect::new(r,c,r,c))).collect();
            reads.extend(changed.overflow.iter().map(|&t| (t, Rect::new(0,0,MAX_ROWS-1,MAX_COLS-1))));
            if !self.javascript.lock().unwrap().invalidate_reads(&reads, metadata_changed, false) { break; }
            self.javascript.lock().unwrap().used.clear();
            changed = self.recalc(Vec::new());
            out.recalc.cells.extend(&changed.cells);
            out.recalc.overflow.extend(&changed.overflow);
            out.recalc.evaluated += changed.evaluated;
        }
        self.javascript.lock().unwrap().prune();
        out.changes.tabs_reload.extend(self.tabs.iter().filter(|t|!t.format_rules.is_empty()).map(|t|t.id));
        out.changes.cells = changes;
        out.changes.tabs_reload.sort_unstable();
        out.changes.tabs_reload.dedup();
        Ok(out)
    }

    fn apply_inner(&mut self, op: &Op, actor: &Actor<'_>, out: &mut Applied) -> Result<Option<Op>, OpError> {
        let author = self.author_index(actor.account);
        let meta = pack_meta(author, actor.at);
        match op {
            Op::Extend { tab, rows, cols } => {
                let t = self.tab_mut_or(*tab)?;
                t.rows.ensure_len((*rows).min(MAX_ROWS));
                t.cols.ensure_len((*cols).min(MAX_COLS));
                // Ids are never given back, so there is nothing to undo.
                Ok(None)
            }
            Op::SetCells { tab, cells } => {
                self.tab_or(*tab)?;
                let mut inverse = Vec::with_capacity(cells.len());
                for w in cells {
                    let t = self.tab(*tab).expect("checked");
                    let (Some(rp), Some(cp)) = (t.rows.pos_of(w.row), t.cols.pos_of(w.col)) else {
                        out.skipped += 1;
                        continue;
                    };
                    let prev = t.cell_by_id(w.row, w.col);
                    inverse.push(self.capture_cell(*tab, w.row, w.col));
                    out.overwritten.push(Overwrite { tab: *tab, row: w.row, col: w.col, prev_meta: prev.meta, prev_value: prev.value });
                    self.write_cell(*tab, w, (rp, cp), Some(meta))?;
                    out.changes.cells.push((*tab, Rect::new(rp, cp, rp, cp)));
                }
                Ok(Some(Op::SetCells { tab: *tab, cells: inverse }))
            }
            Op::StyleArea { tab, area, patch } => {
                patch.validate().map_err(OpError::Invalid)?;
                let t = self.tab_or(*tab)?;
                let Some(rect) = t.pos_rect(area) else { return Ok(None) };
                let rect = clamp_rect(rect, t);
                let mut inverse = Vec::new();
                for r in rect.r1..=rect.r2 {
                    for c in rect.c1..=rect.c2 {
                        let t = self.tab(*tab).expect("checked");
                        let (rid, cid) = (t.rows.id_at(r).expect("clamped"), t.cols.id_at(c).expect("clamped"));
                        let cell = t.cell_by_id(rid, cid);
                        let old = self.styles.get(cell.style).clone();
                        let new = patch.apply(&old);
                        if new == old {
                            continue;
                        }
                        inverse.push(self.capture_cell(*tab, rid, cid));
                        let initial = if cell.value.is_empty() && cell.formula.is_none() && patch.control.is_some() {
                            new.control.as_ref().and_then(|control| control.unchecked_value())
                        } else { None };
                        let id = self.styles.intern(new);
                        let t = self.tab_mut(*tab).expect("checked");
                        t.set_style_by_id(rid, cid, id);
                        if let Some(value) = initial { t.set_value_by_id(rid, cid, &value, false, Some(meta)); }
                        t.set_meta_by_id(rid, cid, meta);
                    }
                }
                out.changes.cells.push((*tab, rect));
                Ok(Some(Op::SetCells { tab: *tab, cells: inverse }))
            }
            Op::ClearArea { tab, area, what } => {
                let t = self.tab_or(*tab)?;
                let Some(rect) = t.pos_rect(area) else { return Ok(None) };
                let rect = clamp_rect(rect, t);
                let mut inverse = Vec::new();
                let occupied: Vec<(RowId, ColId)> = {
                    let t = self.tab(*tab).expect("checked");
                    let mut v = Vec::new();
                    t.for_each_occupied(|rid, cid, _, _| {
                        if let (Some(r), Some(c)) = (t.rows.pos_of(rid), t.cols.pos_of(cid))
                            && rect.contains(CellPos::new(r, c))
                        {
                            v.push((rid, cid));
                        }
                    });
                    v
                };
                for (rid, cid) in occupied {
                    let before = self.capture_cell(*tab, rid, cid);
                    let t = self.tab(*tab).expect("checked");
                    let (r, c) = (t.rows.pos_of(rid).expect("exists"), t.cols.pos_of(cid).expect("exists"));
                    let w = match what {
                        ClearWhat::Contents => CellWrite { row: rid, col: cid, clear: true, ..Default::default() },
                        ClearWhat::Formats => CellWrite { row: rid, col: cid, style: Some(Style {
                            control: before.style.as_ref().and_then(|s| s.control.clone()), ..Default::default()
                        }), ..Default::default() },
                        ClearWhat::All => CellWrite { row: rid, col: cid, clear: true, style: Some(Style::default()), ..Default::default() },
                    };
                    self.write_cell(*tab, &w, (r, c), Some(meta))?;
                    inverse.push(before);
                }
                out.changes.cells.push((*tab, rect));
                Ok(Some(Op::SetCells { tab: *tab, cells: inverse }))
            }
            Op::InsertRows { tab, before, ids } | Op::InsertCols { tab, before, ids } => {
                let rows = matches!(op, Op::InsertRows { .. });
                let t = self.tab_mut_or(*tab)?;
                let axis = if rows { &mut t.rows } else { &mut t.cols };
                let at = before.and_then(|b| axis.pos_of(b)).unwrap_or(axis.len());
                axis.insert_ids(at, ids);
                self.rewrite_after_move(*tab, &AxisChange::Insert { rows, at, n: ids.len() as u32 });
                self.dirty_tab_formulas(*tab);
                out.changes.tabs_reload.push(*tab);
                Ok(Some(if rows {
                    Op::DeleteRows { tab: *tab, ids: ids.clone() }
                } else {
                    Op::DeleteCols { tab: *tab, ids: ids.clone() }
                }))
            }
            Op::DeleteRows { tab, ids } | Op::DeleteCols { tab, ids } => {
                let rows = matches!(op, Op::DeleteRows { .. });
                let t = self.tab_or(*tab)?;
                let axis = if rows { &t.rows } else { &t.cols };
                let mut positions: Vec<(u32, u32)> = ids.iter().filter_map(|&id| axis.pos_of(id).map(|p| (p, id))).collect();
                positions.sort_unstable();
                if positions.is_empty() {
                    return Ok(None);
                }
                // Everything in the deleted rows, to put back on undo.
                let mut cells = Vec::new();
                let occupied: Vec<(RowId, ColId)> = {
                    let set: ahash::HashSet<u32> = positions.iter().map(|&(_, id)| id).collect();
                    let mut v = Vec::new();
                    t.for_each_occupied(|rid, cid, _, _| {
                        let id = if rows { rid } else { cid };
                        if set.contains(&id) {
                            v.push((rid, cid));
                        }
                    });
                    v
                };
                for &(rid, cid) in &occupied {
                    cells.push(self.capture_cell(*tab, rid, cid));
                }
                // Clear formulas in the deleted cells so their groups forget them.
                for &(rid, cid) in &occupied {
                    self.clear_formula(*tab, rid, cid);
                }
                // Formulas elsewhere that will change, captured before the rewrite.
                let before_formulas = self.formulas_reading(*tab);
                // Delete from the bottom up, one contiguous run at a time.
                let mut runs: Vec<(u32, u32)> = Vec::new();
                for &(p, _) in &positions {
                    match runs.last_mut() {
                        Some((s, n)) if *s + *n == p => *n += 1,
                        _ => runs.push((p, 1)),
                    }
                }
                for &(start, n) in runs.iter().rev() {
                    let t = self.tab_mut(*tab).expect("checked");
                    let axis = if rows { &mut t.rows } else { &mut t.cols };
                    axis.remove(start, n);
                    self.rewrite_after_move(*tab, &AxisChange::Delete { rows, at: start, n });
                }
                let removed: Vec<u32> = positions.iter().map(|&(_, id)| id).collect();
                let t = self.tab_mut(*tab).expect("checked");
                if rows {
                    t.drop_rows(&removed);
                    t.hidden_rows.retain(|r| !removed.contains(r));
                    for r in &removed {
                        t.row_heights.remove(r);
                    }
                } else {
                    t.drop_cols(&removed);
                    t.hidden_cols.retain(|c| !removed.contains(c));
                    for c in &removed {
                        t.col_widths.remove(c);
                    }
                }
                let formulas = self.changed_formulas(before_formulas);
                self.dirty_tab_formulas(*tab);
                out.changes.tabs_reload.push(*tab);
                Ok(Some(if rows {
                    Op::RestoreRows { tab: *tab, positions, cells, formulas }
                } else {
                    Op::RestoreCols { tab: *tab, positions, cells, formulas }
                }))
            }
            Op::RestoreRows { tab, positions, cells, formulas } | Op::RestoreCols { tab, positions, cells, formulas } => {
                let rows = matches!(op, Op::RestoreRows { .. });
                self.tab_or(*tab)?;
                let mut sorted = positions.clone();
                sorted.sort_unstable();
                for &(p, id) in &sorted {
                    let t = self.tab_mut(*tab).expect("checked");
                    let axis = if rows { &mut t.rows } else { &mut t.cols };
                    let at = p.min(axis.len());
                    axis.insert_ids(at, &[id]);
                    self.rewrite_after_move(*tab, &AxisChange::Insert { rows, at, n: 1 });
                }
                for w in cells {
                    if let Some(pos) = self.pos_of_ids(*tab, w.row, w.col) {
                        self.write_cell(*tab, w, pos, None)?;
                    }
                }
                for (ftab, w) in formulas {
                    if let Some(pos) = self.pos_of_ids(*ftab, w.row, w.col) {
                        self.write_cell(*ftab, w, pos, None)?;
                    }
                }
                self.dirty_tab_formulas(*tab);
                out.changes.tabs_reload.push(*tab);
                let ids = sorted.iter().map(|&(_, id)| id).collect();
                Ok(Some(if rows { Op::DeleteRows { tab: *tab, ids } } else { Op::DeleteCols { tab: *tab, ids } }))
            }
            Op::Reorder { tab, rows, ids } => {
                let t = self.tab_mut_or(*tab)?;
                let axis = if *rows { &mut t.rows } else { &mut t.cols };
                let mut positions: Vec<u32> = ids.iter().filter_map(|&id| axis.pos_of(id)).collect();
                if positions.len() != ids.len() {
                    return Err(OpError::Invalid("Some of the rows or columns to reorder no longer exist.".into()));
                }
                positions.sort_unstable();
                let previous: Vec<u32> = positions.iter().map(|&p| axis.id_at(p).expect("exists")).collect();
                for (p, &id) in positions.iter().zip(ids) {
                    let start = *p;
                    axis.reorder(start, &[id]);
                }
                self.rewrite_after_move(*tab, &AxisChange::Permute { rows: *rows, start: 0, old_of_new: Vec::new() });
                self.dirty_tab_formulas(*tab);
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::Reorder { tab: *tab, rows: *rows, ids: previous }))
            }
            Op::AddTab { tab, name, index } => {
                if !valid_tab_name(name) {
                    return Err(OpError::BadTabName);
                }
                if self.tab_by_name(name).is_some() {
                    return Err(OpError::TabNameTaken(name.clone()));
                }
                if self.tab(*tab).is_some() {
                    return Err(OpError::Invalid(format!("Tab id {tab} is already used.")));
                }
                self.insert_tab(Tab::new(*tab, name.clone()), *index);
                out.changes.tab_list = true;
                Ok(Some(Op::DeleteTab { tab: *tab }))
            }
            Op::DeleteTab { tab } => {
                self.tab_or(*tab)?;
                if self.tabs.len() <= 1 {
                    return Err(OpError::LastTab);
                }
                let content = self.capture_tab(*tab).expect("exists");
                let index = self.tabs.iter().position(|t| t.id == *tab).expect("exists");
                let name = self.tab(*tab).expect("exists").name.clone();
                // Formulas in the tab go with it.
                let members: Vec<(RowId, ColId)> = self
                    .groups
                    .ids()
                    .filter_map(|g| self.groups.get(g))
                    .filter(|g| g.tab == *tab)
                    .flat_map(|g| g.members.iter().copied().collect::<Vec<_>>())
                    .collect();
                for (r, c) in members {
                    self.clear_formula(*tab, r, c);
                }
                let before_formulas = self.formulas_reading(*tab);
                self.tabs.retain(|t| t.id != *tab);
                self.reindex_tabs();
                self.spills.retain(|k, _| k.0 != *tab);
                self.protections.retain(|p| p.tab != *tab);
                self.charts.retain(|c| c.tab != *tab);
                // References to the tab become #REF!.
                let affected: Vec<(TabId, RowId, ColId, Arc<Expr>)> = before_formulas.iter().map(|(t, r, c, e, _, _)| (*t, *r, *c, e.clone())).collect();
                for (ftab, r, c, expr) in affected {
                    let new = expr.map_refs(&mut |e| match e {
                        Expr::Cell(cr) | Expr::Spill(cr) if cr.tab == Some(*tab) => Some(Expr::Error(crate::value::ErrorKind::Ref)),
                        Expr::Range(rr) if rr.tab == Some(*tab) => Some(Expr::Error(crate::value::ErrorKind::Ref)),
                        _ => None,
                    });
                    self.put_expr(ftab, r, c, new);
                    if let Some((rp, cp)) = self.pos_of_ids(ftab, r, c) {
                        self.dirty.insert((ftab, cp, rp));
                    }
                }
                let formulas = self.changed_formulas(before_formulas);
                out.changes.tab_list = true;
                Ok(Some(Op::RestoreTab { tab: *tab, name, index, content: Box::new(content), formulas }))
            }
            Op::RestoreTab { tab, name, index, content, formulas } => {
                if self.tab(*tab).is_some() {
                    return Err(OpError::Invalid(format!("Tab id {tab} is already used.")));
                }
                let name = if self.tab_by_name(name).is_some() { self.unused_tab_name(name) } else { name.clone() };
                self.insert_tab(Tab::new(*tab, name), Some(*index));
                self.fill_tab(*tab, content, meta)?;
                for (ftab, w) in formulas {
                    if let Some(pos) = self.pos_of_ids(*ftab, w.row, w.col) {
                        self.write_cell(*ftab, w, pos, None)?;
                    }
                }
                self.dirty_tab_formulas(*tab);
                out.changes.tab_list = true;
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::DeleteTab { tab: *tab }))
            }
            Op::RenameTab { tab, name } => {
                if !valid_tab_name(name) {
                    return Err(OpError::BadTabName);
                }
                if self.tab_by_name(name).is_some_and(|t| t.id != *tab) {
                    return Err(OpError::TabNameTaken(name.clone()));
                }
                let t = self.tab_mut_or(*tab)?;
                let old = std::mem::replace(&mut t.name, name.clone());
                out.changes.tab_list = true;
                // Formulas show the new name; INDIRECT and names may now resolve differently.
                self.dirty_all_volatile_names();
                Ok(Some(Op::RenameTab { tab: *tab, name: old }))
            }
            Op::MoveTab { tab, index } => {
                let from = self.tabs.iter().position(|t| t.id == *tab).ok_or(OpError::NoSuchTab(*tab))?;
                let t = self.tabs.remove(from);
                let to = (*index).min(self.tabs.len());
                self.tabs.insert(to, t);
                self.reindex_tabs();
                out.changes.tab_list = true;
                Ok(Some(Op::MoveTab { tab: *tab, index: from }))
            }
            Op::ReplaceTab { tab, content } => {
                self.tab_or(*tab)?;
                let previous = self.capture_tab(*tab).expect("exists");
                self.fill_tab(*tab, content, meta)?;
                self.dirty_tab_formulas(*tab);
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::ReplaceTab { tab: *tab, content: Box::new(previous) }))
            }
            Op::TabProps { tab, props } => {
                if let Some(rules)=&props.format_rules {
                    if rules.len()>100 {return Err(OpError::Invalid("Use at most 100 formatting rules per tab.".into()));}
                    for rule in rules {rule.kind.validate().map_err(OpError::Invalid)?;}
                }
                let t = self.tab_mut_or(*tab)?;
                let mut inv = TabProps::default();
                if let Some(f) = props.frozen_rows {
                    inv.frozen_rows = Some(std::mem::replace(&mut t.frozen_rows, f));
                }
                if let Some(f) = props.frozen_cols {
                    inv.frozen_cols = Some(std::mem::replace(&mut t.frozen_cols, f));
                }
                if let Some(c) = &props.color {
                    inv.color = Some(std::mem::replace(&mut t.color, c.clone()));
                }
                if let Some(c) = &props.code {
                    inv.code = Some(std::mem::replace(&mut t.code, c.clone()));
                }
                if let Some(rules)=&props.format_rules { inv.format_rules=Some(std::mem::replace(&mut t.format_rules,rules.clone())); out.changes.tabs_reload.push(*tab); }
                out.changes.tab_list = true;
                Ok(Some(Op::TabProps { tab: *tab, props: inv }))
            }
            Op::ColWidths { tab, widths } => {
                let t = self.tab_mut_or(*tab)?;
                let mut inv = Vec::new();
                for &(id, w) in widths {
                    let old = match w {
                        Some(w) => t.col_widths.insert(id, w),
                        None => t.col_widths.remove(&id),
                    };
                    inv.push((id, old));
                }
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::ColWidths { tab: *tab, widths: inv }))
            }
            Op::RowHeights { tab, heights } => {
                let t = self.tab_mut_or(*tab)?;
                let mut inv = Vec::new();
                for &(id, h) in heights {
                    let old = match h {
                        Some(h) => t.row_heights.insert(id, h),
                        None => t.row_heights.remove(&id),
                    };
                    inv.push((id, old));
                }
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::RowHeights { tab: *tab, heights: inv }))
            }
            Op::Hide { tab, rows, ids, hidden } => {
                let t = self.tab_mut_or(*tab)?;
                let list = if *rows { &mut t.hidden_rows } else { &mut t.hidden_cols };
                let mut changed = Vec::new();
                for &id in ids {
                    let present = list.contains(&id);
                    if *hidden && !present {
                        list.push(id);
                        changed.push(id);
                    } else if !*hidden && present {
                        list.retain(|&x| x != id);
                        changed.push(id);
                    }
                }
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::Hide { tab: *tab, rows: *rows, ids: changed, hidden: !*hidden }))
            }
            Op::Merge { tab, area } => {
                let t = self.tab_mut_or(*tab)?;
                if t.merges.contains(area) {
                    return Ok(None);
                }
                t.merges.push(area.clone());
                out.changes.tabs_reload.push(*tab);
                Ok(Some(Op::Unmerge { tab: *tab, area: area.clone() }))
            }
            Op::Unmerge { tab, area } => {
                let t = self.tab_mut_or(*tab)?;
                let before = t.merges.len();
                t.merges.retain(|m| m != area);
                out.changes.tabs_reload.push(*tab);
                Ok((t.merges.len() != before).then(|| Op::Merge { tab: *tab, area: area.clone() }))
            }
            Op::SetFunctions { functions } => {
                if functions.len() > 256 { return Err(OpError::Invalid("A sheet may hold at most 256 JavaScript functions.".into())); }
                let mut keys=std::collections::HashSet::new();
                for f in functions { if !keys.insert((f.name.to_ascii_uppercase(),f.account.clone())) {
                    return Err(OpError::Invalid(format!("The JavaScript function {} already has a different definition. Import this workbook as a new sheet, or rename the existing function first.",f.name)));
                } }
                for f in functions { f.validate().map_err(OpError::Invalid)?; }
                let old = std::mem::replace(&mut self.functions, functions.clone());
                out.changes.meta = true;
                self.dirty_all_volatile_names();
                Ok(Some(Op::SetFunctions { functions: old }))
            }
            Op::JavascriptResults { epoch, results } => {
                for (_, answer) in results { answer.validate().map_err(OpError::Invalid)?; }
                {
                    let mut state = self.javascript.lock().unwrap();
                    if state.epoch != *epoch { return Err(OpError::Invalid("The JavaScript inputs changed. Recalculate with the latest values.".into())); }
                    if results.iter().any(|(id,_)| !state.calls.iter().any(|c| c.id == *id && c.result.is_none())) {
                        return Err(OpError::Invalid("This JavaScript call is no longer pending.".into()));
                    }
                    let bytes=serde_json::to_vec(results).map_or(usize::MAX,|s|s.len());
                    if state.bytes.saturating_add(bytes)>32*1024*1024 {return Err(OpError::Invalid("The JavaScript calculation is too large. Use smaller ranges or results.".into()));}
                    state.bytes += bytes;
                    for (id, answer) in results {
                        state.calls.iter_mut().find(|c| c.id == *id).unwrap().result = Some(answer.clone());
                    }
                }
                self.dirty_all_volatile_names();
                Ok(None)
            }
            Op::SetNames { names } => {
                let old = std::mem::replace(&mut self.names, names.clone());
                out.changes.meta = true;
                self.dirty_all_volatile_names();
                Ok(Some(Op::SetNames { names: old }))
            }
            Op::PutChart { chart } => {
                let old = self.charts.iter().position(|c| c.id == chart.id).map(|i| self.charts.remove(i));
                self.charts.push(chart.clone());
                out.changes.meta = true;
                Ok(Some(match old {
                    Some(c) => Op::PutChart { chart: c },
                    None => Op::DeleteChart { id: chart.id.clone() },
                }))
            }
            Op::DeleteChart { id } => {
                let old = self.charts.iter().position(|c| c.id == *id).map(|i| self.charts.remove(i));
                out.changes.meta = true;
                Ok(old.map(|chart| Op::PutChart { chart }))
            }
            Op::PutProtection { protection } => {
                self.tab_or(protection.tab)?;
                let old = self.protections.iter().position(|p| p.id == protection.id).map(|i| self.protections.remove(i));
                self.protections.push(protection.clone());
                out.changes.meta = true;
                Ok(Some(match old {
                    Some(p) => Op::PutProtection { protection: p },
                    None => Op::DeleteProtection { id: protection.id.clone() },
                }))
            }
            Op::DeleteProtection { id } => {
                let old = self.protections.iter().position(|p| p.id == *id).map(|i| self.protections.remove(i));
                out.changes.meta = true;
                Ok(old.map(|protection| Op::PutProtection { protection }))
            }
            Op::Settings { settings } => {
                let old = std::mem::replace(&mut self.settings, settings.clone());
                out.changes.meta = true;
                self.dirty_all_volatile_names();
                Ok(Some(Op::Settings { settings: old }))
            }
            Op::Batch { ops } => {
                let mut inverses = Vec::new();
                for o in ops {
                    if let Some(inv) = self.apply_inner(o, actor, out)? {
                        inverses.push(inv);
                    }
                }
                inverses.reverse();
                Ok((!inverses.is_empty()).then_some(Op::Batch { ops: inverses }))
            }
        }
    }

    fn dirty_all_volatile_names(&mut self) {
        // Formulas using names or INDIRECT are volatile and recalculate anyway; tab names are shown, not stored.
    }

    pub(crate) fn pos_of_ids(&self, tab: TabId, row: RowId, col: ColId) -> Option<(u32, u32)> {
        let t = self.tab(tab)?;
        Some((t.rows.pos_of(row)?, t.cols.pos_of(col)?))
    }

    /// Every formula cell that reads `tab` (from any tab), with its tree, position and text, before a change.
    fn formulas_reading(&self, tab: TabId) -> Vec<(TabId, RowId, ColId, Arc<Expr>, (u32, u32), String)> {
        let mut out = Vec::new();
        for gid in self.groups.ids() {
            let Some(g) = self.groups.get(gid) else { continue };
            if !g.refs.iter().any(|p| p.tab.unwrap_or(g.tab) == tab) {
                continue;
            }
            for &(r, c) in &g.members {
                if let Some(pos) = self.pos_of_ids(g.tab, r, c) {
                    let text = to_a1(&g.expr, pos.0, pos.1, &self.names());
                    out.push((g.tab, r, c, g.expr.clone(), pos, text));
                }
            }
        }
        out
    }

    /// Of the formulas captured before a change, the ones whose formula changed, as writes restoring them (with
    /// the text they had, which names tabs that may no longer exist).
    fn changed_formulas(&self, before: Vec<(TabId, RowId, ColId, Arc<Expr>, (u32, u32), String)>) -> Vec<(TabId, CellWrite)> {
        let mut out = Vec::new();
        for (tab, r, c, _expr, (rp, cp), text) in before {
            let now = self.tab(tab).map(|t| t.cell_by_id(r, c).formula).unwrap_or(None).and_then(|g| self.groups.get(g));
            let same = now.is_some_and(|g| {
                // Same formula at the cell's current position?
                let Some((nr, nc)) = self.pos_of_ids(tab, r, c) else { return false };
                (nr, nc) == (rp, cp) && to_a1(&g.expr, nr, nc, &self.names()) == text
            });
            if !same {
                out.push((tab, CellWrite {
                    row: r,
                    col: c,
                    clear: true,
                    formula: Some(FormulaAt { text, row: rp, col: cp }),
                    ..Default::default()
                }));
            }
        }
        out
    }

    /// Writes one cell (at position `pos`, ids `w.row`/`w.col`).
    fn write_cell(&mut self, tab: TabId, w: &CellWrite, pos: (u32, u32), meta: Option<Meta>) -> Result<(), OpError> {
        let (rid, cid) = (w.row, w.col);
        if w.clear {
            self.clear_formula(tab, rid, cid);
            self.tab_mut(tab).expect("tab").set_value_by_id(rid, cid, &Value::Empty, false, None);
        }
        let mut implied_format: Option<String> = None;
        if let Some(input) = &w.input {
            let current = self.tab(tab).expect("tab").cell_by_id(rid, cid).style;
            let plain = w.style_patch.as_ref().and_then(|p| p.num_fmt.as_ref())
                .map(|f| f.as_deref() == Some("@"))
                .unwrap_or_else(|| w.style.as_ref().unwrap_or(self.styles.get(current)).num_fmt.as_deref() == Some("@"));
            let entry = if plain && !input.is_empty() { Entry::Text(input.strip_prefix('\'').unwrap_or(input).to_owned()) } else { parse_entry(input) };
            match entry {
                Entry::Formula(f) => {
                    self.put_formula(tab, rid, cid, &f, pos).map_err(|e| OpError::Formula {
                        cell: CellPos::new(pos.0, pos.1).to_string(),
                        message: e.message,
                    })?;
                }
                other => {
                    self.clear_formula(tab, rid, cid);
                    let v = match other {
                        Entry::Empty => Value::Empty,
                        Entry::Number(n, fmt) => {
                            implied_format = fmt;
                            Value::Number(n)
                        }
                        Entry::Bool(b) => Value::Bool(b),
                        Entry::Error(e) => Value::Error(e),
                        Entry::Text(s) => Value::from(s),
                        Entry::Formula(_) => unreachable!(),
                    };
                    self.tab_mut(tab).expect("tab").set_value_by_id(rid, cid, &v, false, None);
                }
            }
        } else if let Some(f) = &w.formula {
            self.put_formula(tab, rid, cid, &f.text, (f.row, f.col)).map_err(|e| OpError::Formula {
                cell: CellPos::new(pos.0, pos.1).to_string(),
                message: e.message,
            })?;
        } else if let Some(v) = &w.value {
            self.clear_formula(tab, rid, cid);
            self.tab_mut(tab).expect("tab").set_value_by_id(rid, cid, v, false, None);
        }
        if let Some(s) = &w.style {
            let id = self.styles.intern(s.clone());
            self.tab_mut(tab).expect("tab").set_style_by_id(rid, cid, id);
        }
        if let Some(p) = &w.style_patch {
            p.validate().map_err(OpError::Invalid)?;
            let cur = self.tab(tab).expect("tab").cell_by_id(rid, cid).style;
            let new = p.apply(self.styles.get(cur));
            let id = self.styles.intern(new);
            self.tab_mut(tab).expect("tab").set_style_by_id(rid, cid, id);
        }
        if let Some(fmt) = implied_format {
            // Typing "12%" into a General cell makes it a percentage, as in Excel.
            let cur = self.tab(tab).expect("tab").cell_by_id(rid, cid).style;
            if self.styles.get(cur).num_fmt.is_none() {
                let mut s = self.styles.get(cur).clone();
                s.num_fmt = Some(fmt);
                let id = self.styles.intern(s);
                self.tab_mut(tab).expect("tab").set_style_by_id(rid, cid, id);
            }
        }
        if let Some(m) = meta {
            self.tab_mut(tab).expect("tab").set_meta_by_id(rid, cid, m);
        }
        Ok(())
    }

    /// Replaces a tab's cells, sizes, merges and code with `content`. Rows and columns get new ids.
    fn fill_tab(&mut self, tab: TabId, content: &TabContent, meta: Meta) -> Result<(), OpError> {
        // Forget every formula in the tab.
        let members: Vec<(RowId, ColId)> = self
            .groups
            .ids()
            .filter_map(|g| self.groups.get(g))
            .filter(|g| g.tab == tab)
            .flat_map(|g| g.members.iter().copied().collect::<Vec<_>>())
            .collect();
        for (r, c) in members {
            self.clear_formula(tab, r, c);
        }
        self.spills.retain(|k, _| k.0 != tab);
        let style_ids: Vec<u32> = content.styles.iter().map(|s| self.styles.intern(s.clone())).collect();
        let t = self.tab_mut(tab).expect("tab");
        let fresh = {
            let mut nt = Tab::new(t.id, t.name.clone());
            // Keep id counters moving forward so ids are never reused.
            nt.rows = crate::model::Axis::default();
            nt.cols = crate::model::Axis::default();
            for _ in 0..t.rows.next_id() {
                // Burn the old ids.
            }
            nt
        };
        let row_base = t.rows.next_id();
        let col_base = t.cols.next_id();
        let color = t.color.clone();
        *t = fresh;
        t.color = color;
        // Allocate ids starting after every id the tab ever used.
        t.rows.insert_ids(0, &(row_base..row_base + content.rows).collect::<Vec<_>>());
        t.cols.insert_ids(0, &(col_base..col_base + content.cols).collect::<Vec<_>>());
        let mut formulas: Vec<(RowId, ColId, String, u32, u32)> = Vec::new();
        for (c, col) in content.columns.iter().enumerate() {
            let Some(cid) = t.cols.id_at(c as u32) else { continue };
            for (r, v) in col.values.iter().enumerate() {
                if v.is_empty() {
                    continue;
                }
                let Some(rid) = t.rows.id_at(r as u32) else { continue };
                t.set_value_by_id(rid, cid, v, false, Some(meta));
            }
            for (r, f) in &col.formulas {
                if let Some(rid) = t.rows.id_at(*r) {
                    formulas.push((rid, cid, f.clone(), *r, c as u32));
                }
            }
            for &(r, s) in &col.styles {
                if let (Some(rid), Some(&sid)) = (t.rows.id_at(r), style_ids.get(s as usize)) {
                    t.set_style_by_id(rid, cid, sid);
                    t.set_meta_by_id(rid, cid, meta);
                }
            }
        }
        for &(p, w) in &content.col_widths {
            if let Some(id) = t.cols.id_at(p) {
                t.col_widths.insert(id, w);
            }
        }
        for &(p, h) in &content.row_heights {
            if let Some(id) = t.rows.id_at(p) {
                t.row_heights.insert(id, h);
            }
        }
        t.hidden_rows = content.hidden_rows.iter().filter_map(|&p| t.rows.id_at(p)).collect();
        t.hidden_cols = content.hidden_cols.iter().filter_map(|&p| t.cols.id_at(p)).collect();
        t.frozen_rows = content.frozen_rows;
        t.frozen_cols = content.frozen_cols;
        t.code = content.code.clone();
        t.format_rules = content.format_rules.iter().map(|r| crate::conditional::Rule {id:r.id.clone(),area:t.id_rect(&r.range),kind:r.kind.clone()}).collect();
        let merges: Vec<IdRect> = content.merges.iter().filter_map(|m| {
            Some(IdRect { r1: t.rows.id_at(m.r1)?, c1: t.cols.id_at(m.c1)?, r2: t.rows.id_at(m.r2)?, c2: t.cols.id_at(m.c2)? })
        }).collect();
        t.merges = merges;
        for (rid, cid, text, r, c) in formulas {
            let text = text.strip_prefix('=').unwrap_or(&text).to_owned();
            if let Err(e) = self.put_formula(tab, rid, cid, &text, (r, c)) {
                // A formula that can't be read stays as its text, so nothing is lost.
                let t = self.tab_mut(tab).expect("tab");
                t.set_value_by_id(rid, cid, &Value::from(format!("={text}")), false, Some(meta));
                let _ = e;
            } else {
                self.tab_mut(tab).expect("tab").set_meta_by_id(rid, cid, meta);
            }
        }
        Ok(())
    }

    // ---- preparing operations from positions ---------------------------------------------------------------

    /// The id a position will have: existing, or the one `Extend` will allocate.
    fn peek_ids(&self, tab: TabId, row: u32, col: u32) -> Option<(RowId, ColId)> {
        let t = self.tab(tab)?;
        let r = t.rows.id_at(row).unwrap_or(t.rows.next_id() + (row - t.rows.len().min(row)));
        let c = t.cols.id_at(col).unwrap_or(t.cols.next_id() + (col - t.cols.len().min(col)));
        Some((r, c))
    }

    /// `Extend` when positions up to (`rows`, `cols`) have no ids yet.
    fn extend_for(&self, tab: TabId, rows: u32, cols: u32) -> Option<Op> {
        let t = self.tab(tab)?;
        (rows > t.rows.len() || cols > t.cols.len()).then(|| Op::Extend { tab, rows: rows.max(t.rows.len()), cols: cols.max(t.cols.len()) })
    }

    fn with_extend(&self, tab: TabId, rows: u32, cols: u32, op: Op) -> Op {
        match self.extend_for(tab, rows, cols) {
            Some(ext) => Op::Batch { ops: vec![ext, op] },
            None => op,
        }
    }

    /// Writes by position: `(row, col, write)` where the write's `row`/`col` are ignored.
    pub fn prepare_writes(&self, tab: TabId, writes: Vec<(u32, u32, CellWrite)>) -> Result<Op, OpError> {
        self.tab_or(tab)?;
        let (mut max_r, mut max_c) = (0u32, 0u32);
        let mut cells = Vec::with_capacity(writes.len());
        for (r, c, mut w) in writes {
            if r >= MAX_ROWS || c >= MAX_COLS {
                return Err(OpError::Invalid(format!("{} is outside the grid.", CellPos::new(r.min(MAX_ROWS - 1), c.min(MAX_COLS - 1)))));
            }
            let (rid, cid) = self.peek_ids(tab, r, c).expect("tab exists");
            w.row = rid;
            w.col = cid;
            max_r = max_r.max(r + 1);
            max_c = max_c.max(c + 1);
            cells.push(w);
        }
        Ok(self.with_extend(tab, max_r, max_c, Op::SetCells { tab, cells }))
    }

    /// An id rectangle for a position rectangle, with the `Extend` it needs.
    fn area_ids(&self, tab: TabId, rect: Rect) -> Result<(IdRect, Option<Op>), OpError> {
        if rect.r1>rect.r2 || rect.c1>rect.c2 || rect.r2>=MAX_ROWS || rect.c2>=MAX_COLS {return Err(OpError::Invalid("The range is outside the grid.".into()));}
        let t = self.tab_or(tab)?;
        // Whole columns and rows stop at the cells that exist.
        let r2 = if rect.r2 == MAX_ROWS - 1 { t.rows.len().saturating_sub(1).max(rect.r1) } else { rect.r2 };
        let c2 = if rect.c2 == MAX_COLS - 1 { t.cols.len().saturating_sub(1).max(rect.c1) } else { rect.c2 };
        let (a, b) = (self.peek_ids(tab, rect.r1, rect.c1).expect("tab"), self.peek_ids(tab, r2, c2).expect("tab"));
        Ok((IdRect { r1: a.0, c1: a.1, r2: b.0, c2: b.1 }, self.extend_for(tab, r2 + 1, c2 + 1)))
    }

    pub fn prepare_style(&self, tab: TabId, rect: Rect, patch: StylePatch) -> Result<Op, OpError> {
        patch.validate().map_err(OpError::Invalid)?;
        let (area, ext) = self.area_ids(tab, rect)?;
        let op = Op::StyleArea { tab, area, patch };
        Ok(match ext {
            Some(e) => Op::Batch { ops: vec![e, op] },
            None => op,
        })
    }

    pub fn prepare_clear(&self, tab: TabId, rect: Rect, what: ClearWhat) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        if rect.r1 >= t.rows.len() || rect.c1 >= t.cols.len() {
            return Ok(Op::Batch { ops: vec![] });
        }
        let (area, _) = self.area_ids(tab, rect)?;
        Ok(Op::ClearArea { tab, area, what })
    }

    pub fn prepare_merge(&self, tab: TabId, rect: Rect, merge: bool) -> Result<Op, OpError> {
        let (area, ext) = self.area_ids(tab, rect)?;
        let op = if merge { Op::Merge { tab, area } } else { Op::Unmerge { tab, area } };
        Ok(match ext {
            Some(e) => Op::Batch { ops: vec![e, op] },
            None => op,
        })
    }

    /// Inserts `n` rows (or columns) before position `at`.
    pub fn prepare_insert(&self, tab: TabId, rows: bool, at: u32, n: u32) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        if n == 0 || n > 1_000_000 {
            return Err(OpError::Invalid("Insert between 1 and 1,000,000 rows or columns at a time.".into()));
        }
        let axis = if rows { &t.rows } else { &t.cols };
        let limit = if rows { MAX_ROWS } else { MAX_COLS };
        if axis.len() + n > limit {
            return Err(OpError::Invalid(format!("A tab can hold at most {limit} {}.", if rows { "rows" } else { "columns" })));
        }
        if at >= axis.len() {
            // Inserting past the end only gives the empty grid ids.
            return Ok(if rows {
                self.with_extend(tab, at + n, 0, Op::Batch { ops: vec![] })
            } else {
                self.with_extend(tab, 0, at + n, Op::Batch { ops: vec![] })
            });
        }
        let before = axis.id_at(at);
        let start = axis.next_id();
        let ids: Vec<u32> = (start..start + n).collect();
        Ok(if rows { Op::InsertRows { tab, before, ids } } else { Op::InsertCols { tab, before, ids } })
    }

    /// Deletes positions `at..at+n`.
    pub fn prepare_delete(&self, tab: TabId, rows: bool, at: u32, n: u32) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        let axis = if rows { &t.rows } else { &t.cols };
        let ids: Vec<u32> = (at..at.saturating_add(n)).filter_map(|p| axis.id_at(p)).collect();
        Ok(if rows { Op::DeleteRows { tab, ids } } else { Op::DeleteCols { tab, ids } })
    }

    /// Sorts the rows of `rect` by the given columns (positions, ascending flags). The first row can be a header.
    pub fn prepare_sort(&self, tab: TabId, rect: Rect, keys: &[(u32, bool)], header: bool) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        let r1 = rect.r1 + header as u32;
        let r2 = rect.r2.min(t.rows.len().saturating_sub(1));
        if r1 > r2 {
            return Ok(Op::Batch { ops: vec![] });
        }
        let mut order: Vec<u32> = (r1..=r2).collect();
        order.sort_by(|&a, &b| {
            for &(c, asc) in keys {
                let (va, vb) = (t.value_at(a, c), t.value_at(b, c));
                // Blanks always last, whatever the direction.
                let o = match (va.is_empty(), vb.is_empty()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => return std::cmp::Ordering::Greater,
                    (false, true) => return std::cmp::Ordering::Less,
                    _ => va.sort_cmp(&vb),
                };
                let o = if asc { o } else { o.reverse() };
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        });
        let ids = order.into_iter().map(|p| t.rows.id_at(p).expect("in range")).collect();
        Ok(Op::Reorder { tab, rows: true, ids })
    }

    /// Moves `n` rows (or columns) from `from` to before position `to` (positions before the move).
    pub fn prepare_move(&self, tab: TabId, rows: bool, from: u32, n: u32, to: u32) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        let axis = if rows { &t.rows } else { &t.cols };
        let len = axis.len();
        if from + n > len || to > len {
            return Err(OpError::Invalid("Those rows or columns are outside the tab.".into()));
        }
        let (lo, hi) = (from.min(to), (from + n).max(to));
        let mut seq: Vec<u32> = (lo..hi).map(|p| axis.id_at(p).expect("in range")).collect();
        let moving: Vec<u32> = (from..from + n).map(|p| axis.id_at(p).expect("in range")).collect();
        seq.retain(|id| !moving.contains(id));
        let insert_at = if to <= from { (to - lo) as usize } else { (to - lo) as usize - n as usize };
        for (i, id) in moving.into_iter().enumerate() {
            seq.insert(insert_at + i, id);
        }
        Ok(Op::Reorder { tab, rows, ids: seq })
    }

    pub fn prepare_add_tab(&self, name: &str, index: Option<usize>) -> Op {
        Op::AddTab { tab: self.next_tab_id, name: name.to_owned(), index }
    }

    /// A new tab holding `content` (imports, code results).
    pub fn prepare_new_tab_with(&self, name: &str, index: Option<usize>, content: TabContent) -> Op {
        let tab = self.next_tab_id;
        Op::Batch { ops: vec![Op::AddTab { tab, name: name.to_owned(), index }, Op::ReplaceTab { tab, content: Box::new(content) }] }
    }

    /// Writes `content` into an existing tab at (`row`, `col`), cell by cell (code results into a range).
    pub fn prepare_write_block(&self, tab: TabId, row: u32, col: u32, rows: &[Vec<Value>]) -> Result<Op, OpError> {
        let mut writes = Vec::new();
        for (r, line) in rows.iter().enumerate() {
            for (c, v) in line.iter().enumerate() {
                writes.push((row + r as u32, col + c as u32, CellWrite { clear: true, value: Some(v.clone()), ..Default::default() }));
            }
        }
        self.prepare_writes(tab, writes)
    }

    /// Fills `dst` from `src` like dragging the fill handle or pasting: formulas keep their relative references,
    /// values and styles repeat.
    pub fn prepare_fill(&self, tab: TabId, src: Rect, dst: Rect) -> Result<Op, OpError> {
        let t = self.tab_or(tab)?;
        let mut writes = Vec::new();
        let (h, w) = (src.rows(), src.cols());
        for r in dst.r1..=dst.r2 {
            for c in dst.c1..=dst.c2 {
                let sr = src.r1 + (r - dst.r1) % h;
                let sc = src.c1 + (c - dst.c1) % w;
                let cell = t.cell_at(CellPos::new(sr, sc));
                let style = Some(self.styles.get(cell.style).clone());
                let write = match cell.formula.and_then(|g| self.groups.get(g)) {
                    Some(g) => CellWrite {
                        clear: true,
                        formula: Some(FormulaAt { text: to_a1(&g.expr, r, c, &self.names()), row: r, col: c }),
                        style,
                        ..Default::default()
                    },
                    None => CellWrite {
                        clear: true,
                        value: Some(if cell.spilled { Value::Empty } else { cell.value }),
                        style,
                        ..Default::default()
                    },
                };
                writes.push((r, c, write));
            }
        }
        self.prepare_writes(tab, writes)
    }

    /// Whether `account` may apply `op`: the first protection it would break, if any.
    /// The owner may always change everything.
    pub fn protection_blocking(&self, op: &Op, account: &str, owner: &str) -> Option<&Protection> {
        if account == owner || self.protections.is_empty() {
            return None;
        }
        let blocks = |p: &Protection, tab: TabId, rect: Option<Rect>| -> bool {
            if p.tab != tab || p.editors.iter().any(|e| e == account) {
                return false;
            }
            match (&p.area, rect) {
                (None, _) => true,
                (Some(a), Some(r)) => self.tab(tab).and_then(|t| t.pos_rect(a)).is_some_and(|pr| pr.intersects(&r)),
                (Some(_), None) => true,
            }
        };
        let cell_rect = |tab: TabId, r: RowId, c: ColId| -> Option<Rect> {
            let (rp, cp) = self.pos_of_ids(tab, r, c)?;
            Some(Rect::new(rp, cp, rp, cp))
        };
        let check = |tab: TabId, rect: Option<Rect>| self.protections.iter().find(|p| blocks(p, tab, rect));
        match op {
            Op::SetCells { tab, cells } => cells.iter().find_map(|w| cell_rect(*tab, w.row, w.col).and_then(|r| check(*tab, Some(r)))),
            Op::StyleArea { tab, area, .. } | Op::ClearArea { tab, area, .. } | Op::Merge { tab, area } | Op::Unmerge { tab, area } => {
                check(*tab, self.tab(*tab).and_then(|t| t.pos_rect(area)))
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
            Op::PutProtection { .. } | Op::DeleteProtection { .. } => self.protections.first(),
            Op::Batch { ops } => ops.iter().find_map(|o| self.protection_blocking(o, account, owner)),
            _ => None,
        }
    }
}

/// Structural edits can change addresses, context, helper definitions or tab lookup.
fn resets_javascript(op: &Op) -> bool {
    match op {
        Op::Batch { ops } => ops.iter().any(resets_javascript),
        Op::InsertRows { .. } | Op::InsertCols { .. } | Op::DeleteRows { .. } | Op::DeleteCols { .. }
        | Op::RestoreRows { .. } | Op::RestoreCols { .. } | Op::Reorder { .. } | Op::AddTab { .. }
        | Op::DeleteTab { .. } | Op::RestoreTab { .. } | Op::RenameTab { .. } | Op::MoveTab { .. }
        | Op::ReplaceTab { .. } | Op::SetFunctions { .. } | Op::SetNames { .. } | Op::Settings { .. } => true,
        _ => false,
    }
}

fn changes_javascript_inputs(op: &Op) -> bool {
    match op {
        Op::Batch { ops } => ops.iter().any(changes_javascript_inputs),
        Op::SetCells { cells, .. } => cells.iter().any(|c| c.clear || c.input.is_some() || c.formula.is_some() || c.value.is_some() || c.style_patch.as_ref().is_some_and(|s| s.control.is_some())),
        Op::ClearArea { what, .. } => *what != ClearWhat::Formats,
        Op::StyleArea { patch, .. } => patch.control.is_some(),
        _ => resets_javascript(op),
    }
}

fn clamp_rect(rect: Rect, t: &Tab) -> Rect {
    let r2 = rect.r2.min(t.rows.len().saturating_sub(1));
    let c2 = rect.c2.min(t.cols.len().saturating_sub(1));
    Rect { r1: rect.r1.min(r2), c1: rect.c1.min(c2), r2, c2 }
}

fn bounding(changes: &[(TabId, Rect)]) -> Vec<(TabId, Rect)> {
    let mut by_tab: ahash::HashMap<TabId, Rect> = Default::default();
    for (t, r) in changes {
        by_tab
            .entry(*t)
            .and_modify(|b| *b = Rect::new(b.r1.min(r.r1), b.c1.min(r.c1), b.r2.max(r.r2), b.c2.max(r.c2)))
            .or_insert(*r);
    }
    let mut v: Vec<(TabId, Rect)> = by_tab.into_iter().collect();
    v.sort_by_key(|(t, _)| *t);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Actor<'static> = Actor { account: "00000000-0000-0000-0000-00000000000a", at: 1_800_000_000 };

    fn type_in(wb: &mut Workbook, tab: TabId, a1: &str, input: &str) -> Applied {
        let p = CellPos::parse(a1).expect("address");
        let op = wb
            .prepare_writes(tab, vec![(p.row, p.col, CellWrite { input: Some(input.to_owned()), clear: true, ..Default::default() })])
            .expect("prepare");
        wb.apply(&op, &A).expect("apply")
    }

    fn get(wb: &Workbook, tab: TabId, a1: &str) -> Value {
        let p = CellPos::parse(a1).expect("address");
        wb.tab(tab).expect("tab").value_at(p.row, p.col)
    }

    fn formula(wb: &Workbook, tab: TabId, a1: &str) -> Option<String> {
        let p = CellPos::parse(a1).expect("address");
        wb.formula_text(tab, p.row, p.col)
    }

    #[test]
    fn typing_and_undo() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        type_in(&mut wb, t, "A1", "5");
        type_in(&mut wb, t, "A2", "=A1*2");
        assert_eq!(get(&wb, t, "A2"), Value::Number(10.0));
        let applied = type_in(&mut wb, t, "A1", "7");
        assert_eq!(get(&wb, t, "A2"), Value::Number(14.0));
        wb.apply(applied.inverse.as_ref().expect("inverse"), &A).expect("undo");
        assert_eq!(get(&wb, t, "A1"), Value::Number(5.0));
        assert_eq!(get(&wb, t, "A2"), Value::Number(10.0));
    }

    #[test]
    fn percent_gets_a_format() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        type_in(&mut wb, t, "B2", "12%");
        let cell = wb.cell(t, 1, 1).expect("cell");
        assert_eq!(cell.value, Value::Number(0.12));
        assert_eq!(wb.style_of(cell.style).num_fmt.as_deref(), Some("0%"));
    }

    #[test]
    fn insert_rows_moves_references() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        for (a, v) in [("A1", "1"), ("A2", "2"), ("A3", "3")] {
            type_in(&mut wb, t, a, v);
        }
        type_in(&mut wb, t, "B1", "=SUM(A1:A3)");
        type_in(&mut wb, t, "C1", "=A3*10");
        let op = wb.prepare_insert(t, true, 1, 2).expect("prepare");
        let applied = wb.apply(&op, &A).expect("insert");
        assert_eq!(formula(&wb, t, "B1").as_deref(), Some("SUM(A1:A5)"));
        assert_eq!(formula(&wb, t, "C1").as_deref(), Some("A5*10"));
        assert_eq!(get(&wb, t, "A5"), Value::Number(3.0));
        assert_eq!(get(&wb, t, "C1"), Value::Number(30.0));
        wb.apply(applied.inverse.as_ref().expect("inverse"), &A).expect("undo");
        assert_eq!(formula(&wb, t, "B1").as_deref(), Some("SUM(A1:A3)"));
        assert_eq!(get(&wb, t, "A3"), Value::Number(3.0));
    }

    #[test]
    fn delete_rows_and_undo_restores_refs() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        for (a, v) in [("A1", "1"), ("A2", "2"), ("A3", "3"), ("A4", "4")] {
            type_in(&mut wb, t, a, v);
        }
        type_in(&mut wb, t, "B1", "=A2+A4");
        type_in(&mut wb, t, "C1", "=SUM(A1:A4)");
        let op = wb.prepare_delete(t, true, 1, 1).expect("prepare");
        let applied = wb.apply(&op, &A).expect("delete");
        assert_eq!(formula(&wb, t, "B1").as_deref(), Some("#REF!+A3"));
        assert_eq!(formula(&wb, t, "C1").as_deref(), Some("SUM(A1:A3)"));
        assert_eq!(get(&wb, t, "C1"), Value::Number(8.0));
        wb.apply(applied.inverse.as_ref().expect("inverse"), &A).expect("undo");
        assert_eq!(formula(&wb, t, "B1").as_deref(), Some("A2+A4"));
        assert_eq!(get(&wb, t, "B1"), Value::Number(6.0));
        assert_eq!(get(&wb, t, "A2"), Value::Number(2.0));
    }

    #[test]
    fn sort_keeps_row_formulas() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        for (r, v) in [(1, "3"), (2, "1"), (3, "2")] {
            type_in(&mut wb, t, &format!("A{r}"), v);
            type_in(&mut wb, t, &format!("B{r}"), &format!("=A{r}*10"));
        }
        let op = wb.prepare_sort(t, Rect::new(0, 0, 2, 1), &[(0, true)], false).expect("sort");
        let applied = wb.apply(&op, &A).expect("apply");
        assert_eq!(get(&wb, t, "A1"), Value::Number(1.0));
        assert_eq!(get(&wb, t, "B1"), Value::Number(10.0));
        assert_eq!(get(&wb, t, "B3"), Value::Number(30.0));
        assert_eq!(formula(&wb, t, "B1").as_deref(), Some("A1*10"));
        wb.apply(applied.inverse.as_ref().expect("inverse"), &A).expect("undo");
        assert_eq!(get(&wb, t, "A1"), Value::Number(3.0));
    }

    #[test]
    fn tabs_and_references() {
        let mut wb = Workbook::new();
        let t1 = wb.tabs[0].id;
        let op = wb.prepare_add_tab("Sales", None);
        wb.apply(&op, &A).expect("add");
        let t2 = wb.tab_by_name("Sales").expect("tab").id;
        type_in(&mut wb, t2, "A1", "42");
        type_in(&mut wb, t1, "A1", "=Sales!A1+1");
        assert_eq!(get(&wb, t1, "A1"), Value::Number(43.0));
        let applied = wb.apply(&Op::RenameTab { tab: t2, name: "Revenue".into() }, &A).expect("rename");
        assert_eq!(formula(&wb, t1, "A1").as_deref(), Some("Revenue!A1+1"));
        wb.apply(applied.inverse.as_ref().expect("inv"), &A).expect("undo rename");
        let applied = wb.apply(&Op::DeleteTab { tab: t2 }, &A).expect("delete");
        assert_eq!(get(&wb, t1, "A1"), Value::Error(crate::value::ErrorKind::Ref));
        wb.apply(applied.inverse.as_ref().expect("inv"), &A).expect("restore");
        assert_eq!(get(&wb, t1, "A1"), Value::Number(43.0));
        assert_eq!(formula(&wb, t1, "A1").as_deref(), Some("Sales!A1+1"));
    }

    #[test]
    fn replace_tab_and_fill() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        let content = TabContent::from_rows(vec![
            vec![Value::from("n"), Value::from("sq")],
            vec![Value::Number(1.0)],
            vec![Value::Number(2.0)],
            vec![Value::Number(3.0)],
        ]);
        wb.apply(&Op::ReplaceTab { tab: t, content: Box::new(content) }, &A).expect("replace");
        type_in(&mut wb, t, "B2", "=A2*A2");
        let op = wb.prepare_fill(t, Rect::new(1, 1, 1, 1), Rect::new(2, 1, 3, 1)).expect("fill");
        wb.apply(&op, &A).expect("fill");
        assert_eq!(get(&wb, t, "B4"), Value::Number(9.0));
        assert_eq!(formula(&wb, t, "B4").as_deref(), Some("A4*A4"));
        assert_eq!(wb.groups.ids().count(), 1);
    }

    #[test]
    fn protections_block_others() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        type_in(&mut wb, t, "A1", "1");
        let area = {
            let tab = wb.tab_mut(t).expect("tab");
            tab.id_rect(&Rect::new(0, 0, 9, 0))
        };
        wb.apply(&Op::PutProtection { protection: Protection { id: "p1".into(), tab: t, area: Some(area), editors: vec![], description: String::new() } }, &A).expect("protect");
        let op = wb.prepare_writes(t, vec![(0, 0, CellWrite { input: Some("2".into()), ..Default::default() })]).expect("prep");
        assert!(wb.protection_blocking(&op, "someone-else", A.account).is_some());
        assert!(wb.protection_blocking(&op, A.account, A.account).is_none());
        let op2 = wb.prepare_writes(t, vec![(0, 3, CellWrite { input: Some("2".into()), ..Default::default() })]).expect("prep");
        assert!(wb.protection_blocking(&op2, "someone-else", A.account).is_none());
    }
}
