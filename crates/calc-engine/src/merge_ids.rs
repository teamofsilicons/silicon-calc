//! Stable identity translation for a copy merged more than once. Positions and formula meaning stay the same;
//! the recorded identity map lets later merges recognize rows, columns and tabs already accepted by the parent.

use crate::{
    OpError, Workbook,
    formula::ast::{Coord, Expr},
    model::{Axis, IdRect},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeIds {
    #[serde(default)]
    pub tabs: BTreeMap<u32, u32>,
    /// Axis maps are keyed by the source tab's ID.
    #[serde(default)]
    pub rows: BTreeMap<u32, BTreeMap<u32, u32>>,
    #[serde(default)]
    pub cols: BTreeMap<u32, BTreeMap<u32, u32>>,
}

fn mapped(map: &BTreeMap<u32, u32>, id: u32) -> u32 {
    map.get(&id).copied().unwrap_or(id)
}
fn validate(values: impl Iterator<Item = u32>, map: &BTreeMap<u32, u32>) -> Result<(), OpError> {
    let mut seen = BTreeSet::new();
    for id in values {
        let to = mapped(map, id);
        if to == u32::MAX || !seen.insert(to) {
            return Err(OpError::Invalid(
                "The merge identity map contains colliding or invalid identities.".into(),
            ));
        }
    }
    if map.values().any(|id| *id == u32::MAX) {
        return Err(OpError::Invalid(
            "The merge identity map contains an invalid identity.".into(),
        ));
    }
    Ok(())
}
fn axis(axis: &Axis, map: &BTreeMap<u32, u32>) -> Result<Axis, OpError> {
    validate(axis.ids().iter().copied(), map)?;
    Ok(axis.remapped(map))
}
fn rectangle(area: &IdRect, rows: &BTreeMap<u32, u32>, cols: &BTreeMap<u32, u32>) -> IdRect {
    IdRect {
        r1: mapped(rows, area.r1),
        r2: mapped(rows, area.r2),
        c1: mapped(cols, area.c1),
        c2: mapped(cols, area.c2),
    }
}
fn expression(expr: &Expr, tabs: &BTreeMap<u32, u32>) -> Expr {
    expr.map_refs(&mut |e| match e {
        Expr::Cell(c) => {
            let mut c = *c;
            c.tab = c.tab.map(|id| mapped(tabs, id));
            Some(Expr::Cell(c))
        }
        Expr::Spill(c) => {
            let mut c = *c;
            c.tab = c.tab.map(|id| mapped(tabs, id));
            Some(Expr::Spill(c))
        }
        Expr::Range(r) => {
            let mut r = *r;
            r.tab = r.tab.map(|id| mapped(tabs, id));
            Some(Expr::Range(r))
        }
        _ => None,
    })
}

/// Translates formula reference positions through stable identities between two workbook layouts.
/// Cell coordinates are positions, while `ids` maps source tab/axis identities to destination identities.
/// Absolute references stay absolute and relative references stay relative to the destination formula cell.
/// References to tabs or axis positions absent from the destination retain their source representation: a
/// partial receipt may intentionally leave that structure pending for a later merge.
pub fn remap_expr_positions(
    source: &Workbook,
    dest: &Workbook,
    source_tab: u32,
    dest_tab: u32,
    source_cell: (u32, u32),
    dest_cell: (u32, u32),
    expr: &Expr,
    ids: &MergeIds,
) -> Expr {
    let empty = BTreeMap::new();
    let coordinate =
        |coord: Coord, from: u32, to: u32, source: &Axis, dest: &Axis, map: &BTreeMap<u32, u32>| {
            let translated = coord
                .resolve(from)
                .and_then(|position| source.id_at(position))
                .map(|id| mapped(map, id))
                .and_then(|id| dest.pos_of(id));
            match (coord, translated) {
                (Coord::Abs(_), Some(position)) => Coord::Abs(position),
                (Coord::Rel(_), Some(position)) => {
                    i32::try_from(i64::from(position) - i64::from(to))
                        .map(Coord::Rel)
                        .unwrap_or(coord)
                }
                _ => coord,
            }
        };
    expr.map_refs(&mut |reference| {
        let explicit = match reference {
            Expr::Cell(c) | Expr::Spill(c) => c.tab,
            Expr::Range(r) => r.tab,
            _ => return None,
        };
        let from_id = explicit.unwrap_or(source_tab);
        let to_id = if from_id == source_tab {
            dest_tab
        } else {
            mapped(&ids.tabs, from_id)
        };
        let (Some(from), Some(to)) = (source.tab(from_id), dest.tab(to_id)) else {
            return Some(reference.clone());
        };
        let row = |c| {
            coordinate(
                c,
                source_cell.0,
                dest_cell.0,
                &from.rows,
                &to.rows,
                ids.rows.get(&from_id).unwrap_or(&empty),
            )
        };
        let col = |c| {
            coordinate(
                c,
                source_cell.1,
                dest_cell.1,
                &from.cols,
                &to.cols,
                ids.cols.get(&from_id).unwrap_or(&empty),
            )
        };
        Some(match reference {
            Expr::Cell(c) | Expr::Spill(c) => {
                let mut c = *c;
                c.tab = explicit.map(|_| to_id);
                c.row = row(c.row);
                c.col = col(c.col);
                if matches!(reference, Expr::Spill(_)) {
                    Expr::Spill(c)
                } else {
                    Expr::Cell(c)
                }
            }
            Expr::Range(r) => {
                let mut r = *r;
                r.tab = explicit.map(|_| to_id);
                r.rows = r.rows.map(|(a, b)| (row(a), row(b)));
                r.cols = r.cols.map(|(a, b)| (col(a), col(b)));
                Expr::Range(r)
            }
            _ => unreachable!("reference"),
        })
    })
}

/// Rebuilds a workbook under a stable identity map. Missing entries retain their original identity; collisions
/// are rejected. This changes no cell content, cached result, style, attribution, position or tab name.
pub fn remap(source: &Workbook, ids: &MergeIds) -> Result<Workbook, OpError> {
    validate(source.tabs.iter().map(|t| t.id), &ids.tabs)?;
    let empty = BTreeMap::new();
    let mut out = Workbook::empty();
    out.styles = source.styles.clone();
    out.authors = source.authors.clone();
    out.settings = source.settings.clone();
    out.clock_unix = source.clock_unix;
    for tab in &source.tabs {
        let rows = ids.rows.get(&tab.id).unwrap_or(&empty);
        let cols = ids.cols.get(&tab.id).unwrap_or(&empty);
        let mut copy = tab.clone();
        copy.id = mapped(&ids.tabs, tab.id);
        copy.rows = axis(&tab.rows, rows)?;
        copy.cols = axis(&tab.cols, cols)?;
        copy.columns.clear();
        copy.col_widths = tab
            .col_widths
            .iter()
            .map(|(id, w)| (mapped(cols, *id), *w))
            .collect();
        copy.row_heights = tab
            .row_heights
            .iter()
            .map(|(id, h)| (mapped(rows, *id), *h))
            .collect();
        copy.hidden_rows = tab.hidden_rows.iter().map(|id| mapped(rows, *id)).collect();
        copy.hidden_cols = tab.hidden_cols.iter().map(|id| mapped(cols, *id)).collect();
        copy.merges = tab
            .merges
            .iter()
            .map(|r| rectangle(r, rows, cols))
            .collect();
        for rule in &mut copy.format_rules {rule.area=rectangle(&rule.area,rows,cols);}
        if let Some(code) = &mut copy.code {
            code.inputs = code
                .inputs
                .iter()
                .map(|id| mapped(&ids.tabs, *id))
                .collect();
        }
        // Cell storage is indexed by stable IDs, so copying raw chunks would attach values to the wrong rows.
        for (r, c) in tab.occupied() {
            if tab.rows.pos_of(r).is_none() || tab.cols.pos_of(c).is_none() {
                continue;
            }
            let cell = tab.cell_by_id(r, c);
            let (r, c) = (mapped(rows, r), mapped(cols, c));
            copy.set_value_by_id(r, c, &cell.value, cell.spilled, Some(cell.meta));
            copy.set_style_by_id(r, c, cell.style);
        }
        out.insert_tab(copy, None);
    }
    out.next_tab_id = out
        .next_tab_id
        .max(source.next_tab_id)
        .max(ids.tabs.values().copied().max().map_or(0, |i| i + 1));
    out.names = source
        .names
        .iter()
        .map(|n| {
            let mut n = n.clone();
            n.tab = n.tab.map(|id| mapped(&ids.tabs, id));
            n
        })
        .collect();
    out.charts = source
        .charts
        .iter()
        .map(|c| {
            let mut c = c.clone();
            c.tab = mapped(&ids.tabs, c.tab);
            c
        })
        .collect();
    out.protections = source
        .protections
        .iter()
        .map(|p| {
            let mut copy = p.clone();
            copy.tab = mapped(&ids.tabs, p.tab);
            copy.area = p.area.as_ref().map(|r| {
                rectangle(
                    r,
                    ids.rows.get(&p.tab).unwrap_or(&empty),
                    ids.cols.get(&p.tab).unwrap_or(&empty),
                )
            });
            copy
        })
        .collect();
    // Formula references encode positions (R1C1); only their explicit tab identities need translation.
    for tab in &source.tabs {
        let rows = ids.rows.get(&tab.id).unwrap_or(&empty);
        let cols = ids.cols.get(&tab.id).unwrap_or(&empty);
        for (r, c) in tab.occupied() {
            if tab.rows.pos_of(r).is_none() || tab.cols.pos_of(c).is_none() {
                continue;
            }
            if let Some(g) = tab.cell_by_id(r, c).formula.and_then(|g| source.group(g)) {
                out.put_expr(
                    mapped(&ids.tabs, tab.id),
                    mapped(rows, r),
                    mapped(cols, c),
                    expression(&g.expr, &ids.tabs),
                );
            }
        }
    }
    out.spills = source
        .spills
        .iter()
        .map(|((t, r, c), spill)| {
            (
                (
                    mapped(&ids.tabs, *t),
                    mapped(ids.rows.get(t).unwrap_or(&empty), *r),
                    mapped(ids.cols.get(t).unwrap_or(&empty), *c),
                ),
                *spill,
            )
        })
        .collect();
    Ok(out)
}

/// Records the source state considered by a merge, keeping unselected changes pending.
/// Conflict choices consume a source change even when the parent value is retained.
pub fn accept_merge(
    base: &Workbook,
    source: &Workbook,
    choice: &crate::diff::MergeChoice,
) -> Result<Workbook, OpError> {
    use crate::{
        CellPos,
        model::Tab,
        ops::{Actor, Op},
    };
    if choice.tabs.is_empty() && choice.area.is_none() {
        return Ok(source.clone());
    }
    let mut out = base.clone();
    if let Some((id, area)) = choice.area {
        let tab = source.tab(id).ok_or(OpError::Invalid(
            "The selected merge tab is missing.".into(),
        ))?;
        if out.tab(id).is_none() {
            out.insert_tab(Tab::new(id, tab.name.clone()), None);
        }
        // Include only structural scaffolding needed to address the selected range. Deleted axes remain
        // pending, as do cells outside the range. Existing IDs are never reassigned by position.
        for rows in [true, false] {
            let axis = if rows { &tab.rows } else { &tab.cols };
            let last = if rows { area.r2 } else { area.c2 };
            for pos in 0..axis.len().min(last.saturating_add(1)) {
                let id_at = axis.id_at(pos).expect("position");
                let current = out.tab(id).expect("exists");
                let dest = if rows { &current.rows } else { &current.cols };
                if dest.pos_of(id_at).is_some() {
                    continue;
                }
                let at = axis.ids()[..pos as usize]
                    .iter()
                    .rev()
                    .find_map(|id| dest.pos_of(*id))
                    .map_or(0, |p| p + 1);
                let positions = vec![(at, id_at)];
                let op = if rows {
                    Op::RestoreRows {
                        tab: id,
                        positions,
                        cells: vec![],
                        formulas: vec![],
                    }
                } else {
                    Op::RestoreCols {
                        tab: id,
                        positions,
                        cells: vec![],
                        formulas: vec![],
                    }
                };
                out.apply(
                    &op,
                    &Actor {
                        account: "merge-baseline",
                        at: 0,
                    },
                )?;
            }
        }
        let cells: BTreeSet<_> = tab
            .occupied()
            .into_iter()
            .chain(out.tab(id).expect("exists").occupied())
            .collect();
        for (r, c) in cells {
            let Some((rp, cp)) = tab.rows.pos_of(r).zip(tab.cols.pos_of(c)) else {
                continue;
            };
            if area.contains(CellPos::new(rp, cp)) {
                copy_cell(&mut out, source, id, r, c, Some(base));
            }
        }
        return Ok(out);
    }
    let selected = |id| choice.tabs.contains(&id);
    // Replace whole selected tabs without turning references from other tabs into #REF.
    let removed: Vec<_> = out
        .tabs
        .iter()
        .filter(|t| selected(t.id))
        .flat_map(|t| t.occupied().into_iter().map(move |(r, c)| (t.id, r, c)))
        .collect();
    for (t, r, c) in removed {
        out.clear_formula(t, r, c);
    }
    let old_order: Vec<_> = out.tabs.iter().map(|t| t.id).collect();
    out.tabs.retain(|t| !selected(t.id));
    out.reindex_tabs();
    for tab in source.tabs.iter().filter(|t| selected(t.id)) {
        let mut skeleton = tab.clone();
        skeleton.columns.clear();
        out.insert_tab(skeleton, None);
    }
    // All selected layouts must exist before formulas are translated, including forward references.
    for tab in source.tabs.iter().filter(|t| selected(t.id)) {
        for (r, c) in tab.occupied() {
            if tab.rows.pos_of(r).is_some() && tab.cols.pos_of(c).is_some() {
                copy_cell(&mut out, source, tab.id, r, c, Some(base));
            }
        }
    }
    let mut order: Vec<_> = source
        .tabs
        .iter()
        .map(|t| t.id)
        .filter(|id| old_order.contains(id) || selected(*id))
        .collect();
    order.extend(
        old_order
            .iter()
            .copied()
            .filter(|id| !source.tabs.iter().any(|t| t.id == *id) && !selected(*id)),
    );
    out.tabs.sort_by_key(|t| {
        order
            .iter()
            .position(|id| *id == t.id)
            .unwrap_or(usize::MAX)
    });
    out.reindex_tabs();
    // The merge planner applies workbook names and charts only during a whole-workbook merge.
    // Keep those changes pending after selecting individual tabs; protection policy stays on the parent.
    out.spills.retain(|(t, _, _), _| !selected(*t));
    out.spills.extend(
        source
            .spills
            .iter()
            .filter(|((t, _, _), _)| selected(*t))
            .map(|(key, value)| (*key, *value)),
    );
    out.next_tab_id = out.next_tab_id.max(source.next_tab_id);
    Ok(out)
}

/// A selected formula stays pending while a source dependency has not reached the receipt. Otherwise the
/// parent can contain #REF (or a fallback position) while the receipt incorrectly marks the formula accepted.
fn pending_dependencies(
    source: &Workbook,
    dest: &Workbook,
    tab: u32,
    cell: (u32, u32),
    expr: &Expr,
) -> bool {
    let mut pending = false;
    expr.walk(&mut |reference| {
        if pending {
            return;
        }
        let explicit = match reference {
            Expr::Cell(c) | Expr::Spill(c) => c.tab,
            Expr::Range(r) => r.tab,
            _ => return,
        };
        let id = explicit.unwrap_or(tab);
        let Some(from) = source.tab(id) else {
            return;
        };
        let Some(to) = dest.tab(id) else {
            pending = true;
            return;
        };
        let missing = |row: u32, col: u32| {
            let (Some(r), Some(c)) = (from.rows.id_at(row), from.cols.id_at(col)) else {
                return false;
            };
            if to.rows.pos_of(r).is_some() && to.cols.pos_of(c).is_some() {
                return false;
            }
            let value = from.cell_by_id(r, c);
            !value.value.is_empty() || value.formula.is_some()
        };
        match reference {
            Expr::Cell(c) | Expr::Spill(c) => {
                if let Some((r, c)) = c.row.resolve(cell.0).zip(c.col.resolve(cell.1)) {
                    pending = missing(r, c);
                }
            }
            Expr::Range(r) => {
                let span = |axis: Option<(Coord, Coord)>, at: u32| -> Option<(u32, u32)> {
                    match axis {
                        None => Some((0, u32::MAX)),
                        Some((a, b)) => a
                            .resolve(at)
                            .zip(b.resolve(at))
                            .map(|(a, b)| (a.min(b), a.max(b))),
                    }
                };
                let Some(((r1, r2), (c1, c2))) = span(r.rows, cell.0).zip(span(r.cols, cell.1))
                else {
                    return;
                };
                // Usually the receipt already has every referenced axis, so avoid scanning cell data.
                let rows_missing = from.rows.ids().iter().enumerate().any(|(pos, id)| {
                    pos as u32 >= r1 && pos as u32 <= r2 && to.rows.pos_of(*id).is_none()
                });
                let cols_missing = from.cols.ids().iter().enumerate().any(|(pos, id)| {
                    pos as u32 >= c1 && pos as u32 <= c2 && to.cols.pos_of(*id).is_none()
                });
                if rows_missing || cols_missing {
                    pending = from.occupied().into_iter().any(|(r, c)| {
                        from.rows
                            .pos_of(r)
                            .zip(from.cols.pos_of(c))
                            .is_some_and(|(r, c)| {
                                r >= r1 && r <= r2 && c >= c1 && c <= c2 && missing(r, c)
                            })
                    });
                }
            }
            _ => {}
        }
    });
    pending
}

fn copy_cell(
    out: &mut Workbook,
    source: &Workbook,
    tab: u32,
    row: u32,
    col: u32,
    previous: Option<&Workbook>,
) {
    let cell = source.tab(tab).expect("source tab").cell_by_id(row, col);
    let from = source.tab(tab).expect("source tab");
    let source_cell = (
        from.rows.pos_of(row).expect("source row"),
        from.cols.pos_of(col).expect("source col"),
    );
    if cell
        .formula
        .and_then(|g| source.group(g))
        .is_some_and(|g| pending_dependencies(source, out, tab, source_cell, &g.expr))
    {
        if let Some(previous) = previous {
            if previous
                .tab(tab)
                .is_some_and(|t| t.rows.pos_of(row).is_some() && t.cols.pos_of(col).is_some())
            {
                copy_cell(out, previous, tab, row, col, None);
            }
            return;
        }
    }
    let style = out.styles.intern(source.style_of(cell.style).clone());
    let meta = crate::model::unpack_meta(cell.meta)
        .and_then(|(author, at)| source.authors.get(author).map(|name| (name, at)))
        .map(|(name, at)| crate::model::pack_meta(out.author_index(name), at))
        .unwrap_or(0);
    // Translate positional references through the shared stable identities. Missing future tabs stay in
    // the AST, so accepting another tab later can make those references available without losing them.
    let expr = cell.formula.and_then(|g| source.group(g)).map(|g| {
        let to = out.tab(tab).expect("destination tab");
        remap_expr_positions(
            source,
            out,
            tab,
            tab,
            source_cell,
            (
                to.rows.pos_of(row).expect("destination row"),
                to.cols.pos_of(col).expect("destination col"),
            ),
            &g.expr,
            &MergeIds::default(),
        )
    });
    out.clear_formula(tab, row, col);
    let dest = out.tab_mut(tab).expect("destination tab");
    dest.set_value_by_id(row, col, &cell.value, cell.spilled, Some(meta));
    dest.set_style_by_id(row, col, style);
    if let Some(expr) = expr {
        out.put_expr(tab, row, col, expr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CellPos, Value,
        ops::{Actor, CellWrite},
    };
    #[test]
    fn remapping_preserves_cells_history_and_cross_tab_recalculation() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        let other = wb.add_tab("Other", None);
        let actor = Actor {
            account: "c:author",
            at: 1_800_000_000,
        };
        let op = wb
            .prepare_writes(
                t,
                vec![(
                    0,
                    0,
                    CellWrite {
                        input: Some("4".into()),
                        ..Default::default()
                    },
                )],
            )
            .unwrap();
        wb.apply(&op, &actor).unwrap();
        let name = wb.tabs[0].name.clone();
        let op = wb
            .prepare_writes(
                other,
                vec![(
                    0,
                    0,
                    CellWrite {
                        input: Some(format!("='{}'!A1*2", name)),
                        ..Default::default()
                    },
                )],
            )
            .unwrap();
        wb.apply(&op, &actor).unwrap();
        let before = wb.tab(t).unwrap().cell_at(CellPos::new(0, 0));
        let ids = MergeIds {
            tabs: BTreeMap::from([(t, 10), (other, 20)]),
            rows: BTreeMap::from([
                (t, BTreeMap::from([(0, 5)])),
                (other, BTreeMap::from([(0, 6)])),
            ]),
            cols: BTreeMap::from([(t, BTreeMap::from([(0, 3)]))]),
        };
        let mut out = remap(&wb, &ids).unwrap();
        assert_eq!(out.tab(10).unwrap().rows.id_at(0), Some(5));
        assert_eq!(out.tab(10).unwrap().cell_by_id(5, 3).meta, before.meta);
        assert_eq!(out.tab(20).unwrap().value_at(0, 0), Value::Number(8.0));
        assert_eq!(out.formula_text(20, 0, 0), wb.formula_text(other, 0, 0));
        let op = out
            .prepare_writes(
                10,
                vec![(
                    0,
                    0,
                    CellWrite {
                        input: Some("7".into()),
                        clear: true,
                        ..Default::default()
                    },
                )],
            )
            .unwrap();
        out.apply(&op, &actor).unwrap();
        assert_eq!(out.tab(20).unwrap().value_at(0, 0), Value::Number(14.0));
        let mut bad = ids;
        bad.tabs.insert(other, 10);
        assert!(remap(&wb, &bad).is_err());
        let snapshot = crate::snapshot::encode(&out);
        let back = crate::snapshot::decode(&snapshot).unwrap();
        assert_eq!(back.tab(20).unwrap().value_at(0, 0), Value::Number(14.0));
    }
}
