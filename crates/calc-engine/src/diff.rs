//! Comparing two states of the same sheet (versions, or a copy and the sheet it came from), and merging a copy
//! back with a three-way merge at the point it was made.
//!
//! States of one sheet share tab, row and column ids, so cells are matched by id, never by position: a row
//! inserted above a cell doesn't make it look changed.

use std::collections::BTreeMap;

use ahash::{HashMap, HashMapExt as _, HashSet};
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
        (Some(g), Some((r, c))) => (Some(format!("={}", to_a1(&g.expr, r, c, &wb.names()))), Some(crate::formula::identity::stable_key(wb, tab.id, r, c, &g.expr))),
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
    #[serde(default)]
    pub properties_changed: bool,
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
                    && !t.properties_changed
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
        td.properties_changed = b.col_widths != t.col_widths || b.row_heights != t.row_heights || b.hidden_rows != t.hidden_rows || b.hidden_cols != t.hidden_cols || b.frozen_rows != t.frozen_rows || b.frozen_cols != t.frozen_cols || b.merges != t.merges || b.color != t.color || b.code != t.code;
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
        if td.cells_changed > 0 || td.renamed_from.is_some() || td.rows_added + td.rows_removed + td.cols_added + td.cols_removed > 0 || td.reordered || td.properties_changed {
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
    /// Source identities and their destination identities, for durable repeated merges.
    pub ids: crate::merge_ids::MergeIds,
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
    let mut formula_writes: Vec<(TabId, RowId, ColId, TabId, RowId, ColId)> = Vec::new();
    let resolutions: HashMap<(TabId, RowId, ColId), Side> = choice.resolutions.iter().copied().collect();
    let wanted = |tab: TabId| choice.tabs.is_empty() || choice.tabs.contains(&tab);
    let mut tab_map: HashMap<TabId, TabId> = HashMap::new();

    // Reserve every destination tab before writing formulas, including forward references.
    for tt in &theirs.tabs {
        if let (Some(bt), Some(ot)) = (base.tab(tt.id), sim.tab(tt.id)) {
            tab_map.insert(tt.id, tt.id);
            if wanted(tt.id) && choice.area.is_none() && tt.name != bt.name && ot.name == bt.name {
                let name = sim.unused_tab_name(&tt.name);
                let op = Op::RenameTab { tab: tt.id, name };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
    }
    let mut added = Vec::new();
    for tt in &theirs.tabs {
        if !wanted(tt.id) || choice.area.is_some_and(|(t, _)| t != tt.id) { continue; }
        let needs_tab = base.tab(tt.id).is_none() || (sim.tab(tt.id).is_none()
            && (base.capture_tab(tt.id) != theirs.capture_tab(tt.id) || base.tab(tt.id).is_some_and(|t| t.name != tt.name || t.color != tt.color)));
        if needs_tab {
            let name = sim.unused_tab_name(&tt.name);
            let op = sim.prepare_add_tab(&name, None);
            let id = match &op { Op::AddTab { tab, .. } => *tab, _ => unreachable!("add tab") };
            sim.apply(&op, &actor)?;
            ops.push(op);
            tab_map.insert(tt.id, id);
            added.push((tt.id, id));
            plan.tabs_added.push(name);
        }
    }
    // Formula groups retain source IDs; rendering them with the destination labels remaps
    // references without replacing text inside strings or function names.
    let mut referenced = theirs.clone();
    for tab in &mut referenced.tabs {
        tab.name = tab_map.get(&tab.id).and_then(|id| sim.tab(*id)).map(|t| t.name.clone())
            .unwrap_or_else(|| format!("Unmerged copy tab {}", tab.id));
    }
    for &(source, destination) in &added {
        let tt = theirs.tab(source).expect("source tab");
        if let Some((_, area)) = choice.area {
            let op = Op::Extend { tab: destination, rows: (area.r2 + 1).min(tt.rows.len()), cols: (area.c2 + 1).min(tt.cols.len()) };
            sim.apply(&op, &actor)?;
            ops.push(op);
            let dt = sim.tab(destination).expect("destination");
            let cells = occupied_set(tt).into_iter().filter_map(|(r,c)| {
                let (pr,pc) = tt.rows.pos_of(r).zip(tt.cols.pos_of(c))?;
                if !area.contains(CellPos::new(pr,pc)) { return None; }
                Some(write_from(&referenced, tt, r, c, dt.rows.id_at(pr)?, dt.cols.id_at(pc)?))
            }).collect::<Vec<_>>();
            plan.cells_taken += cells.len() as u64;
            if !cells.is_empty() {
                let op = Op::SetCells { tab: destination, cells };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        } else {
            let mut content = referenced.capture_tab(source).unwrap_or_default();
            if let Some(code) = &mut content.code {
                code.inputs = code.inputs.iter().filter_map(|id| tab_map.get(id).copied()).collect();
            }
            let op = Op::ReplaceTab { tab: destination, content: Box::new(content) };
            sim.apply(&op, &actor)?;
            ops.push(op);
            if let Some(color) = &tt.color {
                let op = Op::TabProps { tab: destination, props: crate::ops::TabProps { color: Some(Some(color.clone())), ..Default::default() } };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
        let dt = sim.tab(destination).expect("destination");
        plan.ids.rows.insert(source, tt.rows.ids().iter().zip(dt.rows.ids()).map(|(&a,&b)|(a,b)).collect());
        plan.ids.cols.insert(source, tt.cols.ids().iter().zip(dt.cols.ids()).map(|(&a,&b)|(a,b)).collect());
        for (row,col) in tt.occupied() {
            let Some((r,c)) = tt.rows.pos_of(row).zip(tt.cols.pos_of(col)) else { continue };
            if choice.area.is_some_and(|(_,area)| !area.contains(CellPos::new(r,c))) { continue; }
            if tt.cell_by_id(row,col).formula.is_some() {
                if let Some((dr,dc)) = dt.rows.id_at(r).zip(dt.cols.id_at(c)) {
                    formula_writes.push((source,row,col,destination,dr,dc));
                }
            }
        }
    }
    for tt in &theirs.tabs {
        if !wanted(tt.id) || choice.area.is_some_and(|(t, _)| t != tt.id) || added.iter().any(|(id,_)| *id == tt.id) { continue; }
        let Some(bt) = base.tab(tt.id) else { continue };
        if sim.tab(tt.id).is_none() { continue; }
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
        // A range still needs unambiguous row/column identities. Introduce the selected
        // scaffold in source order, retaining shared rows instead of aliasing two source IDs
        // onto one parent row when the copy inserted a row in the middle.
        if let Some((_, area)) = choice.area {
            for rows in [true,false] {
                let (axis,base_axis,last) = if rows { (&tt.rows,&bt.rows,area.r2) } else { (&tt.cols,&bt.cols,area.c2) };
                let map = if rows { &mut row_map } else { &mut col_map };
                for pos in 0..axis.len().min(last.saturating_add(1)) {
                    let id=axis.id_at(pos).expect("position");
                    let parent=sim.tab(tt.id).expect("exists");
                    let parent_axis=if rows { &parent.rows } else { &parent.cols };
                    if parent_axis.pos_of(id).is_some() && base_axis.pos_of(id).is_some() { continue; }
                    let previous=axis.ids()[..pos as usize].iter().rev().find_map(|id| parent_axis.pos_of(*map.get(id).unwrap_or(id)));
                    let at=previous.map_or(0,|p|p+1);
                    let tail = axis.ids()[pos as usize+1..].iter().all(|id|base_axis.pos_of(*id).is_none());
                    if tail && parent_axis.id_at(at).is_some_and(|target| base_axis.pos_of(target).is_none()) {
                        map.insert(id,parent_axis.id_at(at).expect("position"));
                        continue;
                    }
                    let op=sim.prepare_insert(tt.id,rows,at,1)?;
                    sim.apply(&op,&actor)?;
                    ops.push(op);
                    let parent=sim.tab(tt.id).expect("exists");
                    let parent_axis=if rows { &parent.rows } else { &parent.cols };
                    map.insert(id,parent_axis.id_at(at).expect("inserted"));
                    if rows { plan.rows_added+=1; }
                }
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
                    if tt.cell_by_id(r,c).formula.is_some() { formula_writes.push((tt.id,r,c,tt.id,orow,ocol)); }
                    writes.push(write_from(&referenced, tt, r, c, orow, ocol));
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
                if tt.cell_by_id(r,c).formula.is_some() { formula_writes.push((tt.id,r,c,tt.id,orow,ocol)); }
                writes.push(write_from(&referenced, tt, r, c, orow, ocol));
                plan.cells_taken += 1;
            }
        }
        if !writes.is_empty() {
            let op = Op::SetCells { tab: tt.id, cells: writes };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        let destination = sim.tab(tt.id).expect("exists");
        plan.ids.rows.insert(tt.id, tt.rows.ids().iter().filter_map(|&id| {
            let mapped = *row_map.get(&id).unwrap_or(&id);
            destination.rows.pos_of(mapped).map(|_| (id, mapped))
        }).collect());
        plan.ids.cols.insert(tt.id, tt.cols.ids().iter().filter_map(|&id| {
            let mapped = *col_map.get(&id).unwrap_or(&id);
            destination.cols.pos_of(mapped).map(|_| (id, mapped))
        }).collect());
        // Merge each layout property independently, retaining unrelated changes on the parent.
        if choice.area.is_none() {
            let original = ours.tab(tt.id).unwrap_or(&ot);
            let mut merged = sim.tab(tt.id).expect("exists").clone();
            macro_rules! property {
                ($field:ident) => {
                    if tt.$field != bt.$field && (original.$field == bt.$field || choice.default == Side::Theirs) {
                        merged.$field = tt.$field.clone();
                    }
                };
            }
            property!(frozen_rows); property!(frozen_cols); property!(color);
            if tt.code != bt.code && (original.code == bt.code || choice.default == Side::Theirs) {
                merged.code = tt.code.clone();
                if let Some(code) = &mut merged.code {
                    code.inputs = code.inputs.iter().filter_map(|id|tab_map.get(id).copied()).collect();
                }
            }
            for id in bt.col_widths.keys().chain(tt.col_widths.keys()).copied().collect::<HashSet<_>>() {
                let to = *col_map.get(&id).unwrap_or(&id);
                if tt.col_widths.get(&id) != bt.col_widths.get(&id)
                    && (original.col_widths.get(&to) == bt.col_widths.get(&id) || choice.default == Side::Theirs) {
                    match tt.col_widths.get(&id) { Some(w) => { merged.col_widths.insert(to, *w); }, None => { merged.col_widths.remove(&to); } }
                }
            }
            for id in bt.row_heights.keys().chain(tt.row_heights.keys()).copied().collect::<HashSet<_>>() {
                let to = *row_map.get(&id).unwrap_or(&id);
                if tt.row_heights.get(&id) != bt.row_heights.get(&id)
                    && (original.row_heights.get(&to) == bt.row_heights.get(&id) || choice.default == Side::Theirs) {
                    match tt.row_heights.get(&id) { Some(h) => { merged.row_heights.insert(to, *h); }, None => { merged.row_heights.remove(&to); } }
                }
            }
            for (rows, b, t) in [(true, &bt.hidden_rows, &tt.hidden_rows), (false, &bt.hidden_cols, &tt.hidden_cols)] {
                let map = if rows { &row_map } else { &col_map };
                let hidden = if rows { &mut merged.hidden_rows } else { &mut merged.hidden_cols };
                for id in b.iter().chain(t).copied().collect::<HashSet<_>>() {
                    if b.contains(&id) != t.contains(&id) {
                        let to = *map.get(&id).unwrap_or(&id);
                        hidden.retain(|i| *i != to);
                        if t.contains(&id) { hidden.push(to); }
                    }
                }
            }
            if tt.merges != bt.merges && (original.merges == bt.merges || choice.default == Side::Theirs) {
                merged.merges = tt.merges.iter().map(|m| crate::model::IdRect {
                    r1: *row_map.get(&m.r1).unwrap_or(&m.r1), r2: *row_map.get(&m.r2).unwrap_or(&m.r2),
                    c1: *col_map.get(&m.c1).unwrap_or(&m.c1), c2: *col_map.get(&m.c2).unwrap_or(&m.c2),
                }).collect();
            }
            if tt.format_rules != bt.format_rules && (original.format_rules == bt.format_rules || choice.default == Side::Theirs) {
                merged.format_rules = tt.format_rules.iter().cloned().map(|mut r| {
                    r.area = crate::model::IdRect { r1:*row_map.get(&r.area.r1).unwrap_or(&r.area.r1),r2:*row_map.get(&r.area.r2).unwrap_or(&r.area.r2),c1:*col_map.get(&r.area.c1).unwrap_or(&r.area.c1),c2:*col_map.get(&r.area.c2).unwrap_or(&r.area.c2) };r
                }).collect();
            }
            restore_tab_metadata(&mut sim, &mut ops, &merged)?;
        }
    }
    plan.ids.tabs = tab_map.iter().map(|(&a,&b)|(a,b)).collect();
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
        // Functions follow the same three-way metadata policy as names.
        if theirs.functions != base.functions && (sim.functions == base.functions || choice.default == Side::Theirs) {
            let op = Op::SetFunctions { functions: theirs.functions.clone() };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        // Names and charts.
        if theirs.names != base.names && same_names(&sim, base) {
            let mut names = theirs.names.clone();
            for name in &mut names {
                let source_tab = name.tab.or_else(||theirs.tabs.first().map(|t|t.id));
                if let Some(source_tab) = source_tab {
                    if let Some(&destination) = tab_map.get(&source_tab) {
                        name.text = mapped_reference_text(theirs,&sim,source_tab,destination,&name.text,&plan.ids);
                    }
                }
                name.tab = name.tab.and_then(|id| tab_map.get(&id).copied());
            }
            let op = Op::SetNames { names };
            sim.apply(&op, &actor)?;
            ops.push(op);
        }
        for chart in &base.charts {
            if !theirs.charts.iter().any(|c| c.id == chart.id)
                && (sim.charts.iter().any(|c| same_chart(&sim,c,base,chart)) || choice.default == Side::Theirs) {
                let op = Op::DeleteChart { id: chart.id.clone() };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
        for chart in &theirs.charts {
            if !base.charts.contains(chart) && tab_map.contains_key(&chart.tab) {
                let mut chart = chart.clone();
                let destination=tab_map[&chart.tab];
                chart.range=mapped_reference_text(theirs,&sim,chart.tab,destination,&chart.range,&plan.ids);
                chart.tab = destination;
                let op = Op::PutChart { chart };
                sim.apply(&op, &actor)?;
                ops.push(op);
            }
        }
    }
    plan.ids.tabs = tab_map.into_iter().collect();
    // Every destination axis now exists, including referenced tabs that followed the formula's
    // own tab. Translate reference targets by stable identity, preserving absolute/mixed refs.
    let mut rewritten: BTreeMap<TabId,Vec<CellWrite>> = BTreeMap::new();
    for (source,row,col,destination,drow,dcol) in formula_writes {
        let tt=theirs.tab(source).expect("source formula tab");
        let dt=sim.tab(destination).expect("destination formula tab");
        let Some(source_pos)=tt.rows.pos_of(row).zip(tt.cols.pos_of(col)) else { continue };
        let Some(dest_pos)=dt.rows.pos_of(drow).zip(dt.cols.pos_of(dcol)) else { continue };
        let Some(group)=tt.cell_by_id(row,col).formula.and_then(|id|theirs.group(id)) else { continue };
        let expr=crate::merge_ids::remap_expr_positions(theirs,&sim,source,destination,source_pos,dest_pos,&group.expr,&plan.ids);
        rewritten.entry(destination).or_default().push(CellWrite {
            row:drow,col:dcol,clear:true,formula:Some(FormulaAt { text:to_a1(&expr,dest_pos.0,dest_pos.1,&sim.names()),row:dest_pos.0,col:dest_pos.1 }),..Default::default()
        });
    }
    for (tab,cells) in rewritten {
        let op=Op::SetCells { tab,cells };
        sim.apply(&op,&actor)?;
        ops.push(op);
    }
    plan.op = (!ops.is_empty()).then_some(Op::Batch { ops });
    Ok(plan)
}

fn mapped_reference_text(source:&Workbook,dest:&Workbook,source_tab:TabId,dest_tab:TabId,text:&str,ids:&crate::merge_ids::MergeIds)->String {
    let Ok(expr)=crate::formula::parse::parse_formula(text,0,0,&source.names()) else { return text.into() };
    let expr=crate::merge_ids::remap_expr_positions(source,dest,source_tab,dest_tab,(0,0),(0,0),&expr,ids);
    to_a1(&expr,0,0,&dest.names())
}
fn reference_key(wb:&Workbook,tab:Option<TabId>,text:&str)->String {
    let Some(tab)=tab.or_else(||wb.tabs.first().map(|t|t.id)) else { return text.into() };
    crate::formula::parse::parse_formula(text,0,0,&wb.names())
        .map(|expr|crate::formula::identity::stable_key(wb,tab,0,0,&expr)).unwrap_or_else(|_|text.into())
}
fn same_names(a:&Workbook,b:&Workbook)->bool {
    a.names.len()==b.names.len() && a.names.iter().zip(&b.names).all(|(a_name,b_name)|
        a_name.name==b_name.name && a_name.tab==b_name.tab && reference_key(a,a_name.tab,&a_name.text)==reference_key(b,b_name.tab,&b_name.text))
}
fn same_chart(a:&Workbook,a_chart:&crate::model::Chart,b:&Workbook,b_chart:&crate::model::Chart)->bool {
    let mut left=a_chart.clone();let mut right=b_chart.clone();
    left.range=reference_key(a,Some(left.tab),&left.range);right.range=reference_key(b,Some(right.tab),&right.range);
    left==right
}

/// Restores a historical state as ordinary, invertible edits. Existing row and column identities survive,
/// including identities removed after the selected version. A range restores cells at its current addresses.
pub fn plan_restore(current: &Workbook, target: &Workbook, only: Option<(TabId, Option<Rect>)>) -> Result<Option<Op>, OpError> {
    let mut sim = current.clone();
    let mut ops = Vec::new();
    if let Some((id, Some(area))) = only {
        let source = target.tab(id).ok_or(OpError::NoSuchTab(id))?;
        let mut writes = Vec::new();
        for row in area.r1..=area.r2 {
            for col in area.c1..=area.c2 {
                let w = match source.rows.id_at(row).zip(source.cols.id_at(col)) {
                    Some((r, c)) => write_from(target, source, r, c, 0, 0),
                    None => CellWrite { clear: true, ..Default::default() },
                };
                writes.push((row, col, w));
            }
        }
        return sim.prepare_writes(id, writes).map(Some);
    }
    if target.tabs.is_empty() { return Err(OpError::LastTab); }
    let selected: Vec<_> = target.tabs.iter().filter(|t| only.is_none_or(|(id, _)| id == t.id)).collect();
    if let Some((id, _)) = only {
        if selected.is_empty() { return Err(OpError::NoSuchTab(id)); }
    }
    // Temporary names allow renamed tabs to exchange names without collisions.
    for t in &selected {
        if sim.tab(t.id).is_some_and(|s| s.name != t.name) {
            let name = sim.unused_tab_name(&format!("Restoring {}", t.id));
            append_restore(&mut sim, &mut ops, Op::RenameTab { tab: t.id, name })?;
        }
    }
    for t in &selected {
        if sim.tab(t.id).is_none() {
            let name = sim.unused_tab_name(&format!("Restoring {}", t.id));
            append_restore(&mut sim, &mut ops, Op::AddTab { tab: t.id, name, index: None })?;
        }
    }
    if only.is_none() {
        for t in &current.tabs {
            if target.tab(t.id).is_none() { append_restore(&mut sim, &mut ops, Op::DeleteTab { tab: t.id })?; }
        }
    }
    for t in &selected {
        if sim.tab(t.id).expect("exists").name != t.name {
            append_restore(&mut sim, &mut ops, Op::RenameTab { tab: t.id, name: t.name.clone() })?;
        }
        // Reintroduce the exact historical IDs instead of allocating replacements.
        for rows in [true, false] {
            let desired = if rows { &t.rows } else { &t.cols };
            let s = sim.tab(t.id).expect("exists");
            let axis = if rows { &s.rows } else { &s.cols };
            let gone: Vec<_> = axis.ids().iter().copied().filter(|i| desired.pos_of(*i).is_none()).collect();
            if !gone.is_empty() {
                let op = if rows { Op::DeleteRows { tab: t.id, ids: gone } } else { Op::DeleteCols { tab: t.id, ids: gone } };
                append_restore(&mut sim, &mut ops, op)?;
            }
            let s = sim.tab(t.id).expect("exists");
            let axis = if rows { &s.rows } else { &s.cols };
            let positions: Vec<_> = desired.ids().iter().enumerate().filter(|(_, id)| axis.pos_of(**id).is_none()).map(|(p, id)| (p as u32, *id)).collect();
            if !positions.is_empty() {
                let op = if rows { Op::RestoreRows { tab: t.id, positions, cells: vec![], formulas: vec![] } }
                    else { Op::RestoreCols { tab: t.id, positions, cells: vec![], formulas: vec![] } };
                append_restore(&mut sim, &mut ops, op)?;
            }
            let s = sim.tab(t.id).expect("exists");
            let axis = if rows { &s.rows } else { &s.cols };
            if axis.ids() != desired.ids() {
                append_restore(&mut sim, &mut ops, Op::Reorder { tab: t.id, rows, ids: desired.ids().to_vec() })?;
            }
        }
    }
    // All tabs and axes exist before formulas referring to them are restored.
    for t in &selected {
        let s = sim.tab(t.id).expect("exists");
        let ids: HashSet<_> = occupied_set(s).union(&occupied_set(t)).copied().collect();
        let mut writes = Vec::new();
        for (r, c) in ids {
            if t.rows.pos_of(r).is_some() && t.cols.pos_of(c).is_some()
                && !summary(&sim, s, r, c).same(&summary(target, t, r, c)) {
                writes.push(write_from(target, t, r, c, r, c));
            }
        }
        if !writes.is_empty() { append_restore(&mut sim, &mut ops, Op::SetCells { tab: t.id, cells: writes })?; }
        restore_tab_metadata(&mut sim, &mut ops, t)?;
    }
    let wanted = |id| only.is_none_or(|(t, _)| t == id);
    for c in current.charts.iter().filter(|c| wanted(c.tab)) {
        if !target.charts.iter().any(|x| x.id == c.id) { append_restore(&mut sim, &mut ops, Op::DeleteChart { id: c.id.clone() })?; }
    }
    for c in target.charts.iter().filter(|c| wanted(c.tab)) {
        if !sim.charts.contains(c) { append_restore(&mut sim, &mut ops, Op::PutChart { chart: c.clone() })?; }
    }
    for p in current.protections.iter().filter(|p| wanted(p.tab)) {
        if !target.protections.iter().any(|x| x.id == p.id) { append_restore(&mut sim, &mut ops, Op::DeleteProtection { id: p.id.clone() })?; }
    }
    for p in target.protections.iter().filter(|p| wanted(p.tab)) {
        if !sim.protections.contains(p) { append_restore(&mut sim, &mut ops, Op::PutProtection { protection: p.clone() })?; }
    }
    if only.is_none() {
        for (index, t) in target.tabs.iter().enumerate() {
            if sim.tabs[index].id != t.id { append_restore(&mut sim, &mut ops, Op::MoveTab { tab: t.id, index })?; }
        }
        if sim.functions != target.functions { append_restore(&mut sim, &mut ops, Op::SetFunctions { functions: target.functions.clone() })?; }
        if sim.names != target.names { append_restore(&mut sim, &mut ops, Op::SetNames { names: target.names.clone() })?; }
        if sim.settings != target.settings { append_restore(&mut sim, &mut ops, Op::Settings { settings: target.settings.clone() })?; }
    }
    Ok((!ops.is_empty()).then_some(Op::Batch { ops }))
}

fn append_restore(sim: &mut Workbook, ops: &mut Vec<Op>, op: Op) -> Result<(), OpError> {
    sim.apply(&op, &Actor { account: "restore", at: 0 })?;
    ops.push(op);
    Ok(())
}

fn restore_tab_metadata(sim: &mut Workbook, ops: &mut Vec<Op>, target: &Tab) -> Result<(), OpError> {
    let s = sim.tab(target.id).ok_or(OpError::NoSuchTab(target.id))?.clone();
    let props = crate::ops::TabProps {
        frozen_rows: (s.frozen_rows != target.frozen_rows).then_some(target.frozen_rows),
        frozen_cols: (s.frozen_cols != target.frozen_cols).then_some(target.frozen_cols),
        color: (s.color != target.color).then(|| target.color.clone()),
        code: (s.code != target.code).then(|| target.code.clone()),
        format_rules: (s.format_rules != target.format_rules).then(|| target.format_rules.clone()),
    };
    if props != Default::default() { append_restore(sim, ops, Op::TabProps { tab: target.id, props })?; }
    let widths = s.col_widths.keys().chain(target.col_widths.keys()).copied().collect::<HashSet<_>>()
        .into_iter().filter(|id| s.col_widths.get(id) != target.col_widths.get(id)).map(|id| (id, target.col_widths.get(&id).copied())).collect::<Vec<_>>();
    if !widths.is_empty() { append_restore(sim, ops, Op::ColWidths { tab: target.id, widths })?; }
    let heights = s.row_heights.keys().chain(target.row_heights.keys()).copied().collect::<HashSet<_>>()
        .into_iter().filter(|id| s.row_heights.get(id) != target.row_heights.get(id)).map(|id| (id, target.row_heights.get(&id).copied())).collect::<Vec<_>>();
    if !heights.is_empty() { append_restore(sim, ops, Op::RowHeights { tab: target.id, heights })?; }
    for (rows, old, new) in [(true, &s.hidden_rows, &target.hidden_rows), (false, &s.hidden_cols, &target.hidden_cols)] {
        for hidden in [true, false] {
            let (a, b) = if hidden { (new, old) } else { (old, new) };
            let ids = a.iter().filter(|i| !b.contains(i)).copied().collect::<Vec<_>>();
            if !ids.is_empty() { append_restore(sim, ops, Op::Hide { tab: target.id, rows, ids, hidden })?; }
        }
    }
    for area in &s.merges {
        if !target.merges.contains(area) { append_restore(sim, ops, Op::Unmerge { tab: target.id, area: area.clone() })?; }
    }
    for area in &target.merges {
        if !s.merges.contains(area) { append_restore(sim, ops, Op::Merge { tab: target.id, area: area.clone() })?; }
    }
    Ok(())
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
