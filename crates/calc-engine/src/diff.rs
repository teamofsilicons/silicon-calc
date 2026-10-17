//! Comparing two states of the same sheet (versions, or a copy and the sheet it came from), and merging a copy
//! back with a three-way merge at the point it was made.
//!
//! States of one sheet share tab, row and column ids, so cells are matched by id, never by position: a row
//! inserted above a cell doesn't make it look changed.

use std::collections::BTreeMap;

use ahash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use serde::{Deserialize, Serialize};

use crate::{
    addr::{CellPos, Rect},
    formula::{ast::TabId, parse::to_a1},
    model::{ColId, RowId, Tab},
    ops::{Actor, CellWrite, FormulaAt, Op, OpError},
    style::Style,
    value::Value,
    workbook::Workbook,
};

/// A cell as a person sees it: the value (or formula) and the look.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CellSummary {
    #[serde(default, skip_serializing_if = "Value::is_empty")]
    pub value: Value,
    /// `=…` written at the cell's position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    #[serde(default, skip_serializing_if = "Style::is_default")]
    pub style: Style,
    /// The R1C1 key, for comparing formulas wherever the cells are.
    #[serde(skip)]
    key: Option<String>,
}

impl CellSummary {
    fn same_content(&self, other: &Self) -> bool {
        match (&self.key, &other.key) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.value == other.value,
            _ => false,
        }
    }

    fn same(&self, other: &Self) -> bool {
        self.same_content(other) && self.style == other.style
    }

    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none() && self.style.is_default()
    }
}

fn summary(wb: &Workbook, tab: &Tab, row: RowId, col: ColId) -> CellSummary {
    let cell = tab.cell_by_id(row, col);
    let pos = tab.rows.pos_of(row).zip(tab.cols.pos_of(col));
    let (formula, key) = match (cell.formula.and_then(|g| wb.group(g)), pos) {
        (Some(g), Some((r, c))) => (Some(format!("={}", to_a1(&g.expr, r, c, &wb.names()))), Some(g.key.clone())),
        _ => (None, None),
    };
    CellSummary {
        value: if formula.is_some() || cell.spilled { Value::Empty } else { cell.value },
        formula,
        style: wb.style_of(cell.style).clone(),
        key,
    }
}

/// How one cell differs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellDiff {
    pub tab: TabId,
    pub row: RowId,
    pub col: ColId,
    /// Where the cell is in the newer state (or the older one when it was removed).
    pub address: String,
    pub before: CellSummary,
    pub after: CellSummary,
}

/// How one tab differs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TabDiff {
    pub tab: TabId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub added: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
    pub rows_added: u32,
    pub rows_removed: u32,
    pub cols_added: u32,
    pub cols_removed: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reordered: bool,
    pub cells_changed: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkbookDiff {
    pub tabs: Vec<TabDiff>,
    /// Changed cells, up to the limit asked for.
    pub cells: Vec<CellDiff>,
    pub cells_changed: u64,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub names_changed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub charts_changed: bool,
}

impl WorkbookDiff {
    pub fn is_empty(&self) -> bool {
        self.cells_changed == 0
            && !self.names_changed
            && !self.charts_changed
            && self.tabs.iter().all(|t| {
                !t.added
                    && !t.removed
                    && t.renamed_from.is_none()
                    && t.rows_added == 0
                    && t.rows_removed == 0
                    && t.cols_added == 0
                    && t.cols_removed == 0
                    && !t.reordered
            })
    }
}

fn occupied_set(t: &Tab) -> HashSet<(RowId, ColId)> {
    t.occupied().into_iter().filter(|&(r, c)| t.rows.pos_of(r).is_some() && t.cols.pos_of(c).is_some()).collect()
}

fn order_changed(base: &[u32], other: &[u32]) -> bool {
    let set: HashSet<u32> = other.iter().copied().collect();
    let base_common: Vec<u32> = base.iter().copied().filter(|i| set.contains(i)).collect();
    let bset: HashSet<u32> = base.iter().copied().collect();
    let other_common: Vec<u32> = other.iter().copied().filter(|i| bset.contains(i)).collect();
    base_common != other_common
}

/// Restricts a diff to cells changed in `area` (by position in the newer state), when given.
pub fn diff(before: &Workbook, after: &Workbook, limit: usize, only: Option<(TabId, Rect)>) -> WorkbookDiff {
    let mut out = WorkbookDiff::default();
    let before_ids: HashSet<TabId> = before.tabs.iter().map(|t| t.id).collect();
    for t in &after.tabs {
        if only.is_some_and(|(ot, _)| ot != t.id) {
            continue;
        }
        let Some(b) = before.tab(t.id) else {
            let n = t.occupied().len() as u64;
            out.cells_changed += n;
            out.tabs.push(TabDiff { tab: t.id, name: t.name.clone(), added: true, cells_changed: n, ..Default::default() });
            continue;
        };
        let mut td = TabDiff { tab: t.id, name: t.name.clone(), ..Default::default() };
        if b.name != t.name {
            td.renamed_from = Some(b.name.clone());
        }
        let (brows, arows): (HashSet<u32>, HashSet<u32>) = (b.rows.ids().iter().copied().collect(), t.rows.ids().iter().copied().collect());
        let (bcols, acols): (HashSet<u32>, HashSet<u32>) = (b.cols.ids().iter().copied().collect(), t.cols.ids().iter().copied().collect());
        // Rows that only gained ids at the end of the grid (Extend) are not "added rows".
        let extended = |id: u32, other: &crate::model::Axis| id >= other.next_id();
        td.rows_added = arows.iter().filter(|i| !brows.contains(i) && !(extended(**i, &b.rows) && t.rows.pos_of(**i).is_some_and(|p| p >= b.rows.len()))).count() as u32;
        td.rows_removed = brows.iter().filter(|i| !arows.contains(i)).count() as u32;
        td.cols_added = acols.iter().filter(|i| !bcols.contains(i) && !(extended(**i, &b.cols) && t.cols.pos_of(**i).is_some_and(|p| p >= b.cols.len()))).count() as u32;
        td.cols_removed = bcols.iter().filter(|i| !acols.contains(i)).count() as u32;
        td.reordered = order_changed(b.rows.ids(), t.rows.ids()) || order_changed(b.cols.ids(), t.cols.ids());
        let mut ids: Vec<(RowId, ColId)> = occupied_set(t).union(&occupied_set(b)).copied().collect();
        ids.sort_unstable_by_key(|&(r, c)| (t.rows.pos_of(r).or(b.rows.pos_of(r)), t.cols.pos_of(c).or(b.cols.pos_of(c))));
        for (r, c) in ids {
            let (sb, sa) = (summary(before, b, r, c), summary(after, t, r, c));
            if sb.same(&sa) {
                continue;
            }
            let pos = t.rows.pos_of(r).zip(t.cols.pos_of(c)).or(b.rows.pos_of(r).zip(b.cols.pos_of(c)));
            if let (Some((_, area)), Some((pr, pc))) = (only, pos)
                && !area.contains(CellPos::new(pr, pc))
            {
                continue;
            }
            td.cells_changed += 1;
            out.cells_changed += 1;
            if out.cells.len() < limit {
                let address = pos.map(|(pr, pc)| CellPos::new(pr, pc).to_string()).unwrap_or_default();
                out.cells.push(CellDiff { tab: t.id, row: r, col: c, address, before: sb, after: sa });
            } else {
                out.truncated = true;
            }
        }
        if td.cells_changed > 0 || td.renamed_from.is_some() || td.rows_added + td.rows_removed + td.cols_added + td.cols_removed > 0 || td.reordered {
            out.tabs.push(td);
        }
    }
    for b in &before.tabs {
        if !after.tabs.iter().any(|t| t.id == b.id) && only.is_none_or(|(ot, _)| ot == b.id) {
            let n = b.occupied().len() as u64;
            out.cells_changed += n;
            out.tabs.push(TabDiff { tab: b.id, name: b.name.clone(), removed: true, cells_changed: n, ..Default::default() });
        }
    }
    let _ = before_ids;
    out.names_changed = before.names != after.names;
    out.charts_changed = before.charts != after.charts;
    out
}

// ---- merging a copy ------------------------------------------------------------------------------------------

/// Which side wins a cell both sides changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The sheet being merged into.
    Ours,
    /// The copy.
    #[default]
    Theirs,
}

/// What to merge.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MergeChoice {
    /// Only these tabs of the copy (by the copy's tab ids); all when empty.
    #[serde(default)]
    pub tabs: Vec<TabId>,
    /// Only this area of one tab of the copy (positions in the copy).
    #[serde(default)]
    pub area: Option<(TabId, Rect)>,
    /// How each conflict is settled, by the copy's `(tab, row, col)` ids. Unlisted conflicts follow `default`.
    #[serde(default)]
    pub resolutions: Vec<((TabId, RowId, ColId), Side)>,
    /// For conflicts not listed. The copy wins by default: merging means taking its work.
    #[serde(default = "default_side")]
    pub default: Side,
}

fn default_side() -> Side {
    Side::Theirs
}

/// A cell both sides changed differently.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    /// Ids in the copy.
    pub tab: TabId,
    pub row: RowId,
    pub col: ColId,
    pub tab_name: String,
    pub address: String,
    pub base: CellSummary,
    pub ours: CellSummary,
    pub theirs: CellSummary,
    pub resolved: Side,
}

/// The result of planning a merge.
#[derive(Clone, Debug, Default)]
pub struct MergePlan {
    /// One operation that applies the merge to the sheet being merged into.
    pub op: Option<Op>,
    pub conflicts: Vec<Conflict>,
    pub cells_taken: u64,
    pub tabs_added: Vec<String>,
    pub tabs_removed: Vec<String>,
    pub rows_added: u32,
    pub rows_removed: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("{0}")]
    Op(#[from] OpError),
}

fn write_from(theirs: &Workbook, tt: &Tab, row: RowId, col: ColId, target_row: RowId, target_col: ColId) -> CellWrite {
    let cell = tt.cell_by_id(row, col);
    let pos = tt.rows.pos_of(row).zip(tt.cols.pos_of(col));
    let formula = match (cell.formula.and_then(|g| theirs.group(g)), pos) {
        (Some(g), Some((r, c))) => Some(FormulaAt { text: to_a1(&g.expr, r, c, &theirs.names()), row: r, col: c }),
        _ => None,
    };
    CellWrite {
        row: target_row,
        col: target_col,
        clear: true,
        value: if formula.is_none() && !cell.spilled { Some(cell.value.clone()) } else { None },
        formula,
        style: Some(theirs.style_of(cell.style).clone()),
        ..Default::default()
    }
}

/// Plans merging `theirs` (a copy made from `base`) into `ours`.
pub fn plan_merge(base: &Workbook, ours: &Workbook, theirs: &Workbook, choice: &MergeChoice) -> Result<MergePlan, MergeError> {
    let mut plan = MergePlan::default();
    // Work on a clone so every step sees the state the previous steps produce.
    let mut sim = ours.clone();
    let actor = Actor { account: "merge", at: 0 };
    let mut ops: Vec<Op> = Vec::new();
    let resolutions: HashMap<(TabId, RowId, ColId), Side> = choice.resolutions.iter().copied().collect();
    let wanted = |tab: TabId| choice.tabs.is_empty() || choice.tabs.contains(&tab);
    let mut tab_map: HashMap<TabId, TabId> = HashMap::new();

    for tt in &theirs.tabs {
        if !wanted(tt.id) || choice.area.is_some_and(|(t, _)| t != tt.id) {
            continue;
        }
        let in_base = base.tab(tt.id);
        // A tab the copy added.
        if in_base.is_none() {
            let name = sim.unused_tab_name(&tt.name);
            let content = theirs.capture_tab(tt.id).unwrap_or_default();
            let op = sim.prepare_new_tab_with(&name, None, content);
            sim.apply(&op, &actor)?;
            ops.push(op);
            plan.tabs_added.push(name);
            continue;
        }
        let bt = in_base.expect("checked");
        // The sheet removed the tab since the copy was made: the copy's version comes back as a new tab.
        if sim.tab(tt.id).is_none() {
            let changed = !diff_tab_cells(base, bt, theirs, tt).is_empty();
            if changed {
                let name = sim.unused_tab_name(&tt.name);
                let content = theirs.capture_tab(tt.id).unwrap_or_default();
                let op = sim.prepare_new_tab_with(&name, None, content);
                sim.apply(&op, &actor)?;
                ops.push(op);
                plan.tabs_added.push(name);
            }
            continue;
        }
        tab_map.insert(tt.id, tt.id);
        // Rename.
        let ot_name = sim.tab(tt.id).expect("exists").name.clone();
        if tt.name != bt.name && ot_name == bt.name && choice.area.is_none() {
            let name = if sim.tab_by_name(&tt.name).is_some() { sim.unused_tab_name(&tt.name) } else { tt.name.clone() };
            let op = Op::RenameTab { tab: tt.id, name };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        let mut row_map: HashMap<RowId, RowId> = HashMap::new();
        let mut col_map: HashMap<ColId, ColId> = HashMap::new();
        if choice.area.is_none() {
            // Rows and columns the copy deleted.
            for rows in [true, false] {
                let (b_axis, t_axis) = if rows { (&bt.rows, &tt.rows) } else { (&bt.cols, &tt.cols) };
                let gone: Vec<u32> = b_axis.ids().iter().copied().filter(|i| t_axis.pos_of(*i).is_none()).collect();
                let ot = sim.tab(tt.id).expect("exists");
                let o_axis = if rows { &ot.rows } else { &ot.cols };
                let gone: Vec<u32> = gone.into_iter().filter(|i| o_axis.pos_of(*i).is_some()).collect();
                if !gone.is_empty() {
                    let op = if rows { Op::DeleteRows { tab: tt.id, ids: gone.clone() } } else { Op::DeleteCols { tab: tt.id, ids: gone.clone() } };
                    sim.apply(&op, &actor)?;
                    ops.push(op);
                    if rows {
                        plan.rows_removed += gone.len() as u32;
                    }
                }
            }
            // Rows and columns the copy inserted (ids the base never had, not just grid extension).
            for rows in [true, false] {
                let (b_axis, t_axis) = if rows { (&bt.rows, &tt.rows) } else { (&bt.cols, &tt.cols) };
                let t_ids = t_axis.ids().to_vec();
                let mut i = 0;
                while i < t_ids.len() {
                    let id = t_ids[i];
                    if b_axis.pos_of(id).is_some() {
                        i += 1;
                        continue;
                    }
                    // A run of new ids, placed after the closest older row before it.
                    let start = i;
                    while i < t_ids.len() && b_axis.pos_of(t_ids[i]).is_none() {
                        i += 1;
                    }
                    let run = &t_ids[start..i];
                    let anchor_after = t_ids[..start].iter().rev().find(|x| b_axis.pos_of(**x).is_some()).copied();
                    let ot = sim.tab(tt.id).expect("exists");
                    let o_axis = if rows { &ot.rows } else { &ot.cols };
                    let at = match anchor_after.and_then(|a| o_axis.pos_of(a)) {
                        Some(p) => p + 1,
                        None if start == 0 => 0,
                        None => o_axis.len(),
                    };
                    // New ids at the very end of the copy's grid that the base had no ids for are grid
                    // extension, not inserted rows: they map onto the sheet's own positions.
                    let is_extension = i == t_ids.len()
                        && (b_axis.is_empty() || anchor_after.is_some_and(|a| b_axis.pos_of(a) == Some(b_axis.len() - 1)));
                    if is_extension {
                        // Both sides may have extended the grid with their own ids: match by position after the
                        // last row they share.
                        let need = at + run.len() as u32;
                        if need > o_axis.len() {
                            let ext = if rows { Op::Extend { tab: tt.id, rows: need, cols: 0 } } else { Op::Extend { tab: tt.id, rows: 0, cols: need } };
                            sim.apply(&ext, &actor)?;
                            ops.push(ext);
                        }
                        let ot = sim.tab(tt.id).expect("exists");
                        let o_axis = if rows { &ot.rows } else { &ot.cols };
                        for (k, &tid) in run.iter().enumerate() {
                            let mapped = o_axis.id_at(at + k as u32).expect("extended");
                            if rows { row_map.insert(tid, mapped) } else { col_map.insert(tid, mapped) };
                        }
                        continue;
                    }
                    let op = sim.prepare_insert(tt.id, rows, at, run.len() as u32)?;
                    sim.apply(&op, &actor)?;
                    let ot = sim.tab(tt.id).expect("exists");
                    let o_axis = if rows { &ot.rows } else { &ot.cols };
                    for (k, &tid) in run.iter().enumerate() {
                        if let Some(mapped) = o_axis.id_at(at + k as u32) {
                            if rows { row_map.insert(tid, mapped) } else { col_map.insert(tid, mapped) };
                        }
                    }
                    if rows {
                        plan.rows_added += run.len() as u32;
                    }
                    ops.push(op);
                }
            }
            // A reorder (sort) in the copy, when the sheet kept the original order.
            let ot = sim.tab(tt.id).expect("exists");
            if order_changed(bt.rows.ids(), tt.rows.ids()) && !order_changed(bt.rows.ids(), ot.rows.ids()) {
                let common: Vec<u32> = tt
                    .rows
                    .ids()
                    .iter()
                    .map(|i| *row_map.get(i).unwrap_or(i))
                    .filter(|i| ot.rows.pos_of(*i).is_some())
                    .collect();
                let op = Op::Reorder { tab: tt.id, rows: true, ids: common };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
        // Cells.
        let ot = sim.tab(tt.id).expect("exists").clone();
        let mut ids: Vec<(RowId, ColId)> = occupied_set(tt).union(&occupied_set(bt)).copied().collect();
        ids.sort_unstable_by_key(|&(r, c)| (tt.rows.pos_of(r), tt.cols.pos_of(c), r, c));
        let mut writes: Vec<CellWrite> = Vec::new();
        for (r, c) in ids {
            if let Some((_, area)) = choice.area {
                match tt.rows.pos_of(r).zip(tt.cols.pos_of(c)) {
                    Some((pr, pc)) if area.contains(CellPos::new(pr, pc)) => {}
                    _ => continue,
                }
            }
            let (orow, ocol) = (*row_map.get(&r).unwrap_or(&r), *col_map.get(&c).unwrap_or(&c));
            if ot.rows.pos_of(orow).is_none() || ot.cols.pos_of(ocol).is_none() {
                // The row or column is gone on our side, or never existed there.
                if tt.rows.pos_of(r).is_none() || tt.cols.pos_of(c).is_none() {
                    continue;
                }
                continue;
            }
            let sb = summary(base, bt, r, c);
            let st = summary(theirs, tt, r, c);
            if sb.same(&st) {
                continue;
            }
            let so = summary(&sim, &ot, orow, ocol);
            let ours_changed = !sb.same(&so);
            if !ours_changed || so.same(&st) {
                if !so.same(&st) {
                    writes.push(write_from(theirs, tt, r, c, orow, ocol));
                    plan.cells_taken += 1;
                }
                continue;
            }
            let side = resolutions.get(&(tt.id, r, c)).copied().unwrap_or(choice.default);
            let address = tt.rows.pos_of(r).zip(tt.cols.pos_of(c)).map(|(pr, pc)| CellPos::new(pr, pc).to_string()).unwrap_or_default();
            plan.conflicts.push(Conflict {
                tab: tt.id,
                row: r,
                col: c,
                tab_name: tt.name.clone(),
                address,
                base: sb,
                ours: so,
                theirs: st,
                resolved: side,
            });
            if side == Side::Theirs {
                writes.push(write_from(theirs, tt, r, c, orow, ocol));
                plan.cells_taken += 1;
            }
        }
        if !writes.is_empty() {
            let op = Op::SetCells { tab: tt.id, cells: writes };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        // Column widths and frozen panes the copy changed.
        if choice.area.is_none() {
            let widths: Vec<(ColId, Option<f32>)> = tt
                .col_widths
                .iter()
                .filter(|(id, w)| bt.col_widths.get(id) != Some(w))
                .map(|(id, w)| (*col_map.get(id).unwrap_or(id), Some(*w)))
                .collect();
            if !widths.is_empty() {
                let op = Op::ColWidths { tab: tt.id, widths };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
    }
    // Tabs the copy removed (only when the sheet left them alone).
    if choice.tabs.is_empty() && choice.area.is_none() {
        for bt in &base.tabs {
            if theirs.tab(bt.id).is_some() {
                continue;
            }
            let Some(ot) = sim.tab(bt.id) else { continue };
            let untouched = diff_tab_cells(base, bt, &sim, ot).is_empty();
            if untouched && sim.tabs.len() > 1 {
                let name = ot.name.clone();
                let op = Op::DeleteTab { tab: bt.id };
                sim.apply(&op, &actor)?;
                ops.push(op);
                plan.tabs_removed.push(name);
            }
        }
        // Names and charts.
        if theirs.names != base.names && sim.names == base.names {
            let op = Op::SetNames { names: theirs.names.clone() };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        for chart in &theirs.charts {
            if !base.charts.contains(chart) && sim.tab(chart.tab).is_some() {
                let op = Op::PutChart { chart: chart.clone() };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
    }
    let _ = tab_map;
    plan.op = (!ops.is_empty()).then_some(Op::Batch { ops });
    Ok(plan)
}

fn diff_tab_cells(a: &Workbook, at: &Tab, b: &Workbook, bt: &Tab) -> Vec<(RowId, ColId)> {
    let ids: Vec<(RowId, ColId)> = occupied_set(at).union(&occupied_set(bt)).copied().collect();
    ids.into_iter().filter(|&(r, c)| !summary(a, at, r, c).same(&summary(b, bt, r, c))).collect()
}

/// Who changed each cell since `base`, by replaying history elsewhere; here, the cells two states differ in,
/// grouped by tab, for "what changed" summaries.
pub fn changed_cells_by_tab(before: &Workbook, after: &Workbook) -> BTreeMap<TabId, u64> {
    let d = diff(before, after, 0, None);
    d.tabs.into_iter().map(|t| (t.tab, t.cells_changed)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::CellWrite;

    const A: Actor<'static> = Actor { account: "a", at: 1_800_000_000 };

    fn type_in(wb: &mut Workbook, tab: TabId, a1: &str, input: &str) {
        let p = CellPos::parse(a1).expect("address");
        let op = wb.prepare_writes(tab, vec![(p.row, p.col, CellWrite { input: Some(input.into()), clear: true, ..Default::default() })]).expect("prep");
        wb.apply(&op, &A).expect("apply");
    }

    fn get(wb: &Workbook, tab: TabId, a1: &str) -> Value {
        let p = CellPos::parse(a1).expect("address");
        wb.tab(tab).expect("tab").value_at(p.row, p.col)
    }

    fn sample() -> (Workbook, TabId) {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        for r in 1..=5 {
            type_in(&mut wb, t, &format!("A{r}"), &r.to_string());
        }
        (wb, t)
    }

    #[test]
    fn diff_by_id() {
        let (base, t) = sample();
        let mut after = base.clone();
        let op = after.prepare_insert(t, true, 0, 1).expect("insert");
        after.apply(&op, &A).expect("apply");
        type_in(&mut after, t, "B3", "x");
        let d = diff(&base, &after, 100, None);
        assert_eq!(d.cells_changed, 1);
        assert_eq!(d.cells[0].address, "B3");
        assert_eq!(d.tabs[0].rows_added, 1);
    }

    #[test]
    fn merge_takes_copy_changes_and_reports_conflicts() {
        let (base, t) = sample();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        type_in(&mut theirs, t, "B1", "=A1*100");
        type_in(&mut theirs, t, "A2", "20");
        type_in(&mut ours, t, "A2", "200");
        type_in(&mut ours, t, "C1", "mine");
        let plan = plan_merge(&base, &ours, &theirs, &MergeChoice { default: Side::Theirs, ..Default::default() }).expect("plan");
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].address, "A2");
        ours.apply(plan.op.as_ref().expect("op"), &A).expect("merge");
        assert_eq!(get(&ours, t, "B1"), Value::Number(100.0));
        assert_eq!(get(&ours, t, "A2"), Value::Number(20.0));
        assert_eq!(get(&ours, t, "C1"), Value::from("mine"));
    }

    #[test]
    fn merge_keeps_ours_when_asked() {
        let (base, t) = sample();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        type_in(&mut theirs, t, "A2", "20");
        type_in(&mut ours, t, "A2", "200");
        let plan = plan_merge(&base, &ours, &theirs, &MergeChoice { default: Side::Ours, ..Default::default() }).expect("plan");
        assert!(plan.op.is_none());
        assert_eq!(plan.conflicts[0].resolved, Side::Ours);
    }

    #[test]
    fn merge_inserted_rows_and_new_tabs() {
        let (base, t) = sample();
        let mut ours = base.clone();
        let mut theirs = base.clone();
        let op = theirs.prepare_insert(t, true, 2, 1).expect("insert");
        theirs.apply(&op, &A).expect("apply");
        type_in(&mut theirs, t, "A3", "2.5");
        let add = theirs.prepare_add_tab("Notes", None);
        theirs.apply(&add, &A).expect("add");
        let nt = theirs.tab_by_name("Notes").expect("tab").id;
        type_in(&mut theirs, nt, "A1", "hello");
        type_in(&mut ours, t, "B1", "=SUM(A1:A5)");
        let plan = plan_merge(&base, &ours, &theirs, &MergeChoice::default()).expect("plan");
        ours.apply(plan.op.as_ref().expect("op"), &A).expect("merge");
        assert_eq!(get(&ours, t, "A3"), Value::Number(2.5));
        assert_eq!(get(&ours, t, "A4"), Value::Number(3.0));
        assert_eq!(ours.formula_text(t, 0, 1).as_deref(), Some("SUM(A1:A6)"));
        assert_eq!(get(&ours, t, "B1"), Value::Number(17.5));
        let notes = ours.tab_by_name("Notes").expect("merged tab").id;
        assert_eq!(get(&ours, notes, "A1"), Value::from("hello"));
    }

    #[test]
    fn merge_part() {
        let (base, t) = sample();
        let ours = base.clone();
        let mut theirs = base.clone();
        type_in(&mut theirs, t, "A1", "100");
        type_in(&mut theirs, t, "A5", "500");
        let choice = MergeChoice { area: Some((t, Rect::new(0, 0, 0, 0))), ..Default::default() };
        let plan = plan_merge(&base, &ours, &theirs, &choice).expect("plan");
        let mut ours = ours;
        ours.apply(plan.op.as_ref().expect("op"), &A).expect("merge");
        assert_eq!(get(&ours, t, "A1"), Value::Number(100.0));
        assert_eq!(get(&ours, t, "A5"), Value::Number(5.0));
    }
}
