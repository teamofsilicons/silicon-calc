//! A whole workbook as bytes (MessagePack with field names, then zstd), and back.

use serde::{Deserialize, Serialize};

use crate::{
    formula::ast::{Expr, TabId},
    model::{Authors, Chart, DefinedName, Protection, RowId, ColId, Tab},
    style::Styles,
    workbook::{Settings, SpillInfo, Workbook},
};

const MAGIC: &[u8; 8] = b"SICALC01";

#[derive(Serialize, Deserialize)]
struct Stored {
    tabs: Vec<Tab>,
    next_tab_id: TabId,
    styles: Styles,
    authors: Authors,
    names: Vec<DefinedName>,
    charts: Vec<Chart>,
    protections: Vec<Protection>,
    settings: Settings,
    /// Formula groups by id (`None` for a free slot): the tab and the tree.
    groups: Vec<Option<(TabId, Expr)>>,
    spills: Vec<((TabId, RowId, ColId), SpillInfo)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("This is not a Silicon Calc snapshot.")]
    NotASnapshot,
    #[error("The snapshot could not be read: {0}")]
    Corrupt(String),
}

/// Encodes a workbook.
pub fn encode(wb: &Workbook) -> Vec<u8> {
    let stored = Stored {
        tabs: wb.tabs.clone(),
        next_tab_id: wb.next_tab_id,
        styles: wb.styles.clone(),
        authors: wb.authors.clone(),
        names: wb.names.clone(),
        charts: wb.charts.clone(),
        protections: wb.protections.clone(),
        settings: wb.settings.clone(),
        groups: wb.groups.list.iter().map(|g| g.as_ref().map(|g| (g.tab, (*g.expr).clone()))).collect(),
        spills: wb.spills.iter().map(|(k, v)| (*k, *v)).collect(),
    };
    let raw = rmp_serde::to_vec_named(&stored).expect("a workbook always serializes");
    let mut out = Vec::with_capacity(raw.len() / 4 + MAGIC.len());
    out.extend_from_slice(MAGIC);
    out.extend(zstd::bulk::compress(&raw, 3).expect("compression never fails on a buffer"));
    out
}

/// Decodes a workbook encoded by `encode`.
pub fn decode(bytes: &[u8]) -> Result<Workbook, SnapshotError> {
    let body = bytes.strip_prefix(MAGIC.as_slice()).ok_or(SnapshotError::NotASnapshot)?;
    let raw = zstd::stream::decode_all(body).map_err(|e| SnapshotError::Corrupt(e.to_string()))?;
    let stored: Stored = rmp_serde::from_slice(&raw).map_err(|e| SnapshotError::Corrupt(e.to_string()))?;
    let mut wb = Workbook::empty();
    wb.next_tab_id = stored.next_tab_id;
    wb.styles = stored.styles;
    wb.styles.rebuild_index();
    wb.authors = stored.authors;
    wb.authors.rebuild();
    wb.names = stored.names;
    wb.charts = stored.charts;
    wb.protections = stored.protections;
    wb.settings = stored.settings;
    for mut t in stored.tabs {
        t.rebuild_indexes();
        wb.tabs.push(t);
    }
    wb.reindex_tabs();
    wb.restore_groups(stored.groups);
    wb.spills = stored.spills.into_iter().collect();
    Ok(wb)
}

impl Workbook {
    /// Rebuilds the formula groups from their trees and the cells that point at them.
    fn restore_groups(&mut self, groups: Vec<Option<(TabId, Expr)>>) {
        // Re-intern every group so keys, patterns and indexes are rebuilt, then map old ids to new.
        let mut remap: Vec<Option<u32>> = Vec::with_capacity(groups.len());
        for g in groups {
            remap.push(g.map(|(tab, expr)| self.groups.intern(tab, expr)));
        }
        for ti in 0..self.tabs.len() {
            let tab_id = self.tabs[ti].id;
            let mut members: Vec<(RowId, ColId, u32)> = Vec::new();
            self.tabs[ti].for_each_occupied(|r, c, chunk, i| {
                if let Some(f) = chunk.formula.as_ref()
                    && f[i] != 0
                {
                    members.push((r, c, f[i] - 1));
                }
            });
            for (r, c, old) in members {
                match remap.get(old as usize).copied().flatten() {
                    Some(new) => {
                        self.tabs[ti].set_formula_by_id(r, c, Some(new));
                        let pos = {
                            let t = &self.tabs[ti];
                            t.rows.pos_of(r).zip(t.cols.pos_of(c))
                        };
                        self.groups_add_member(new, r, c, pos);
                    }
                    None => self.tabs[ti].set_formula_by_id(r, c, None),
                }
            }
            let _ = tab_id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ops::{Actor, CellWrite},
        value::Value,
    };

    #[test]
    fn round_trip() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        let actor = Actor { account: "a", at: 1_800_000_000 };
        let op = wb
            .prepare_writes(
                t,
                vec![
                    (0, 0, CellWrite { input: Some("4".into()), ..Default::default() }),
                    (0, 1, CellWrite { input: Some("=A1*A1".into()), ..Default::default() }),
                    (1, 1, CellWrite { input: Some("={1,2,3}".into()), ..Default::default() }),
                    (2, 0, CellWrite { input: Some("hello".into()), ..Default::default() }),
                ],
            )
            .expect("prepare");
        wb.apply(&op, &actor).expect("apply");
        let bytes = encode(&wb);
        let mut back = decode(&bytes).expect("decode");
        assert_eq!(back.tab(t).expect("tab").value_at(0, 1), Value::Number(16.0));
        assert_eq!(back.tab(t).expect("tab").value_at(2, 0), Value::from("hello"));
        assert_eq!(back.tab(t).expect("tab").value_at(1, 3), Value::Number(3.0));
        assert_eq!(back.formula_text(t, 0, 1).as_deref(), Some("A1*A1"));
        // Still recalculates after loading.
        let op = back.prepare_writes(t, vec![(0, 0, CellWrite { input: Some("5".into()), ..Default::default() })]).expect("prep");
        back.apply(&op, &actor).expect("apply");
        assert_eq!(back.tab(t).expect("tab").value_at(0, 1), Value::Number(25.0));
    }
}
