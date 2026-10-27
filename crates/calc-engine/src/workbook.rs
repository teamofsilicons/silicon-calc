//! The workbook: its tabs, formula groups, finding what depends on a change, and recalculation.
//!
//! Every cell holding a formula belongs to a **group**: all cells in one tab whose formulas have the same R1C1
//! form. A group knows the shape of its references (relative and absolute offsets), so the cells that depend on a
//! change are found by asking each group which of its members point at the changed cells, without keeping a
//! dependency entry per cell. A formula filled down a million rows is one group with one tree.

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use ahash::{HashMap, HashMapExt as _, HashSet, HashSetExt as _};
use serde::{Deserialize, Serialize};

use crate::{
    addr::{CellPos, MAX_COLS, MAX_ROWS, Rect},
    formula::{
        ast::{Coord, Expr, TabId},
        eval::{Area, Array, Ctx, Grid, Key, Res},
        parse::{ParseError, TabNames, parse_formula, r1c1_key, to_a1},
    },
    model::{
        Authors, Cell, ColId, DefinedName, GroupId, Meta, Protection, RowId, Tab, WorkbookNames, Chart,
    },
    style::{Style, Styles},
    value::{ErrorKind, Value},
};

/// The shape of one reference in a formula, as far as dependencies are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RefPattern {
    pub tab: Option<TabId>,
    pub rows: Option<(Coord, Coord)>,
    pub cols: Option<(Coord, Coord)>,
}

fn patterns_of(expr: &Expr) -> Vec<RefPattern> {
    let mut out: Vec<RefPattern> = Vec::new();
    expr.walk(&mut |e| {
        let p = match e {
            Expr::Cell(c) | Expr::Spill(c) => {
                RefPattern { tab: c.tab, rows: Some((c.row, c.row)), cols: Some((c.col, c.col)) }
            }
            Expr::Range(r) => RefPattern { tab: r.tab, rows: r.rows, cols: r.cols },
            _ => return,
        };
        if !out.contains(&p) {
            out.push(p);
        }
    });
    out
}

/// Every cell holding the same R1C1 formula in one tab.
#[derive(Clone, Debug)]
pub struct Group {
    pub tab: TabId,
    pub key: String,
    pub expr: Arc<Expr>,
    pub(crate) refs: Vec<RefPattern>,
    pub volatile: bool,
    pub(crate) members: HashSet<(RowId, ColId)>,
    /// Members by position `(col, row)`, rebuilt when the tab's rows or columns move.
    pub(crate) index: Option<BTreeSet<(u32, u32)>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Groups {
    pub list: Vec<Option<Group>>,
    free: Vec<GroupId>,
    by_key: HashMap<(TabId, String), GroupId>,
    /// Which groups have a reference into each tab.
    targets: HashMap<TabId, HashSet<GroupId>>,
}

impl Groups {
    pub fn get(&self, id: GroupId) -> Option<&Group> {
        self.list.get(id as usize).and_then(|g| g.as_ref())
    }

    fn get_mut(&mut self, id: GroupId) -> Option<&mut Group> {
        self.list.get_mut(id as usize).and_then(|g| g.as_mut())
    }

    /// The group for this formula in this tab, created if needed.
    pub fn intern(&mut self, tab: TabId, expr: Expr) -> GroupId {
        let key = r1c1_key(&expr);
        if let Some(&id) = self.by_key.get(&(tab, key.clone())) {
            return id;
        }
        let refs = patterns_of(&expr);
        let volatile = expr.is_volatile();
        let group = Group {
            tab,
            key: key.clone(),
            expr: Arc::new(expr),
            refs,
            volatile,
            members: HashSet::new(),
            index: Some(BTreeSet::new()),
        };
        let id = match self.free.pop() {
            Some(id) => {
                self.list[id as usize] = Some(group);
                id
            }
            None => {
                self.list.push(Some(group));
                (self.list.len() - 1) as GroupId
            }
        };
        self.by_key.insert((tab, key), id);
        let g = self.list[id as usize].as_ref().expect("just added");
        let targets: Vec<TabId> = g.refs.iter().map(|p| p.tab.unwrap_or(tab)).collect();
        for t in targets {
            self.targets.entry(t).or_default().insert(id);
        }
        id
    }

    fn add_member(&mut self, id: GroupId, row: RowId, col: ColId, pos: Option<(u32, u32)>) {
        if let Some(g) = self.get_mut(id) {
            g.members.insert((row, col));
            match (g.index.as_mut(), pos) {
                (Some(ix), Some((r, c))) => {
                    ix.insert((c, r));
                }
                _ => g.index = None,
            }
        }
    }

    fn remove_member(&mut self, id: GroupId, row: RowId, col: ColId, pos: Option<(u32, u32)>) {
        let Some(g) = self.get_mut(id) else { return };
        g.members.remove(&(row, col));
        match (g.index.as_mut(), pos) {
            (Some(ix), Some((r, c))) => {
                ix.remove(&(c, r));
            }
            _ => g.index = None,
        }
        if g.members.is_empty() {
            let g = self.list[id as usize].take().expect("present");
            self.by_key.remove(&(g.tab, g.key.clone()));
            for p in &g.refs {
                if let Some(set) = self.targets.get_mut(&p.tab.unwrap_or(g.tab)) {
                    set.remove(&id);
                }
            }
            self.free.push(id);
        }
    }

    /// Forgets every position index of the groups living in `tab` (after its rows or columns moved).
    fn invalidate_tab(&mut self, tab: TabId) {
        for g in self.list.iter_mut().flatten() {
            if g.tab == tab {
                g.index = None;
            }
        }
    }

    pub fn ids(&self) -> impl Iterator<Item = GroupId> + '_ {
        self.list.iter().enumerate().filter(|(_, g)| g.is_some()).map(|(i, _)| i as GroupId)
    }
}

/// Where an anchor's spill goes, by ids so it survives inserts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpillInfo {
    pub rows: u32,
    pub cols: u32,
    /// The spill could not be written because something is in the way (the anchor shows #SPILL!).
    pub blocked: bool,
}

/// Settings that change how the workbook calculates and shows things.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Minutes east of UTC, for NOW() and TODAY().
    #[serde(default)]
    pub utc_offset_minutes: i32,
    /// The locale numbers and dates are written in (only `en-US` for now).
    #[serde(default = "default_locale")]
    pub locale: String,
}

fn default_locale() -> String {
    "en-US".to_owned()
}

impl Default for Settings {
    fn default() -> Self {
        Self { utc_offset_minutes: 0, locale: default_locale() }
    }
}

/// The whole spreadsheet.
#[derive(Debug)]
pub struct Workbook {
    pub tabs: Vec<Tab>,
    pub next_tab_id: TabId,
    pub styles: Styles,
    pub authors: Authors,
    pub names: Vec<DefinedName>,
    pub charts: Vec<Chart>,
    pub protections: Vec<Protection>,
    pub settings: Settings,
    pub functions: Vec<crate::javascript::Function>,
    pub javascript: Mutex<crate::javascript::State>,
    pub(crate) format_stats: Mutex<std::collections::BTreeMap<(TabId,String),(f64,f64)>>,
    pub(crate) groups: Groups,
    pub(crate) spills: HashMap<(TabId, RowId, ColId), SpillInfo>,
    tab_index: HashMap<TabId, usize>,
    pub(crate) dirty: BTreeSet<(TabId, u32, u32)>,
    /// The time recalculation reads for NOW(), as Unix seconds.
    pub(crate) clock_unix: f64,
    rng: Mutex<rand::rngs::SmallRng>,
}

impl Clone for Workbook {
    fn clone(&self) -> Self {
        Self {
            tabs: self.tabs.clone(),
            next_tab_id: self.next_tab_id,
            styles: self.styles.clone(),
            authors: self.authors.clone(),
            names: self.names.clone(),
            charts: self.charts.clone(),
            protections: self.protections.clone(),
            settings: self.settings.clone(),
            functions: self.functions.clone(),
            javascript: Mutex::new(self.javascript.lock().unwrap().clone()),
            format_stats: Mutex::default(),
            groups: self.groups.clone(),
            spills: self.spills.clone(),
            tab_index: self.tab_index.clone(),
            dirty: BTreeSet::new(),
            clock_unix: self.clock_unix,
            rng: Mutex::new(rand::SeedableRng::from_os_rng()),
        }
    }
}

impl Default for Workbook {
    fn default() -> Self {
        Self::empty()
    }
}

/// What a recalculation changed, for telling viewers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recalc {
    /// Cells whose shown value changed, by position. Capped; `overflow` lists tabs with more.
    pub cells: Vec<(TabId, u32, u32)>,
    pub overflow: Vec<TabId>,
    pub evaluated: usize,
}

const RECALC_REPORT_CAP: usize = 20_000;

impl Workbook {
    /// A workbook with no tabs.
    pub fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            next_tab_id: 1,
            styles: Styles::new(),
            authors: Authors::default(),
            names: Vec::new(),
            charts: Vec::new(),
            protections: Vec::new(),
            settings: Settings::default(),
            functions: Vec::new(),
            javascript: Mutex::default(),
            format_stats: Mutex::default(),
            groups: Groups::default(),
            spills: HashMap::new(),
            tab_index: HashMap::new(),
            dirty: BTreeSet::new(),
            clock_unix: 0.0,
            rng: Mutex::new(rand::SeedableRng::from_os_rng()),
        }
    }

    /// A new workbook with one empty tab named "Tab 1".
    pub fn new() -> Self {
        let mut wb = Self::empty();
        wb.add_tab("Tab 1", None);
        wb
    }

    pub(crate) fn reindex_tabs(&mut self) {
        self.tab_index = self.tabs.iter().enumerate().map(|(i, t)| (t.id, i)).collect();
    }

    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tab_index.get(&id).and_then(|&i| self.tabs.get(i))
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        let i = *self.tab_index.get(&id)?;
        self.tabs.get_mut(i)
    }

    pub fn tab_by_name(&self, name: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.name.eq_ignore_ascii_case(name))
    }

    /// Adds a tab (at `index`, or last) and returns its id.
    pub fn add_tab(&mut self, name: &str, index: Option<usize>) -> TabId {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.insert_tab(Tab::new(id, name), index);
        id
    }

    pub(crate) fn insert_tab(&mut self, mut tab: Tab, index: Option<usize>) {
        self.next_tab_id = self.next_tab_id.max(tab.id + 1);
        tab.rebuild_indexes();
        let at = index.unwrap_or(self.tabs.len()).min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.reindex_tabs();
    }

    /// A name for a new tab that no tab has yet: "Tab 2", "Tab 3", …
    pub fn unused_tab_name(&self, base: &str) -> String {
        if self.tab_by_name(base).is_none() {
            return base.to_owned();
        }
        let stem = base.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end();
        let stem = if stem.is_empty() { base } else { stem };
        (2..).map(|n| format!("{stem} {n}")).find(|n| self.tab_by_name(n).is_none()).unwrap_or_default()
    }

    pub fn names(&self) -> WorkbookNames<'_> {
        WorkbookNames(&self.tabs)
    }

    pub fn author_index(&mut self, uuid: &str) -> u32 {
        self.authors.intern(uuid)
    }

    pub fn group(&self, id: GroupId) -> Option<&Group> {
        self.groups.get(id)
    }

    /// The formula in a cell as text (without `=`), written for its own position.
    pub fn formula_text(&self, tab: TabId, row: u32, col: u32) -> Option<String> {
        let t = self.tab(tab)?;
        let g = self.groups.get(t.formula_at(row, col)?)?;
        Some(to_a1(&g.expr, row, col, &self.names()))
    }

    /// The formula of a cell by ids as `(text, row, col)`: A1 text written at the cell's current position.
    pub fn formula_at_ids(&self, tab: TabId, row: RowId, col: ColId) -> Option<(String, u32, u32)> {
        let t = self.tab(tab)?;
        let (r, c) = (t.rows.pos_of(row)?, t.cols.pos_of(col)?);
        self.formula_text(tab, r, c).map(|s| (s, r, c))
    }

    pub(crate) fn groups_add_member(&mut self, g: GroupId, row: RowId, col: ColId, pos: Option<(u32, u32)>) {
        self.groups.add_member(g, row, col, pos);
    }

    /// The R1C1 key of a cell's formula (equal keys mean the same formula, wherever the cells are).
    pub fn formula_key(&self, tab: TabId, row: RowId, col: ColId) -> Option<&str> {
        let g = self.tab(tab)?.cell_by_id(row, col).formula?;
        self.groups.get(g).map(|g| g.key.as_str())
    }

    pub fn style_of(&self, id: u32) -> &Style {
        self.styles.get(id)
    }

    pub fn set_clock(&mut self, unix_secs: f64) {
        self.clock_unix = unix_secs;
    }

    // ---- cells -------------------------------------------------------------------------------------------

    /// Puts a formula (text without `=`, written at the cell's position) into the cell with these ids.
    /// The cell's value becomes stale until the next recalculation.
    pub fn put_formula(&mut self, tab: TabId, row: RowId, col: ColId, text: &str, at: (u32, u32)) -> Result<(), ParseError> {
        let expr = parse_formula(text, at.0, at.1, &self.names())?;
        self.put_expr(tab, row, col, expr);
        Ok(())
    }

    pub(crate) fn put_expr(&mut self, tab: TabId, row: RowId, col: ColId, expr: Expr) {
        self.clear_formula(tab, row, col);
        self.regroup(tab, row, col, expr);
    }

    /// Moves a formula cell into the group of `expr` without touching its value or spill.
    fn regroup(&mut self, tab: TabId, row: RowId, col: ColId, expr: Expr) {
        let pos = self.tab(tab).and_then(|t| Some((t.rows.pos_of(row)?, t.cols.pos_of(col)?)));
        if let Some(old) = self.tab(tab).map(|t| t.cell_by_id(row, col).formula).unwrap_or(None) {
            self.groups.remove_member(old, row, col, pos);
        }
        let g = self.groups.intern(tab, expr);
        self.groups.add_member(g, row, col, pos);
        if let Some(t) = self.tab_mut(tab) {
            t.set_formula_by_id(row, col, Some(g));
        }
    }

    pub(crate) fn clear_formula(&mut self, tab: TabId, row: RowId, col: ColId) {
        let Some(t) = self.tab(tab) else { return };
        let (rp, cp) = (t.rows.pos_of(row), t.cols.pos_of(col));
        let Some(g) = rp.zip(cp).and_then(|(r, c)| t.formula_at(r, c)).or_else(|| t.cell_by_id(row, col).formula) else {
            return;
        };
        self.groups.remove_member(g, row, col, rp.zip(cp));
        if let Some(t) = self.tab_mut(tab) {
            t.set_formula_by_id(row, col, None);
        }
        // A formula that spilled leaves its spill behind until it is cleared.
        if let Some(info) = self.spills.remove(&(tab, row, col)) {
            self.clear_spill_area(tab, row, col, info);
        }
    }

    fn clear_spill_area(&mut self, tab: TabId, row: RowId, col: ColId, info: SpillInfo) -> Vec<(u32, u32)> {
        let mut cleared = Vec::new();
        if info.blocked {
            return cleared;
        }
        let Some(t) = self.tab_mut(tab) else { return cleared };
        let (Some(r0), Some(c0)) = (t.rows.pos_of(row), t.cols.pos_of(col)) else { return cleared };
        for dr in 0..info.rows {
            for dc in 0..info.cols {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let (r, c) = (r0 + dr, c0 + dc);
                if t.is_spilled_at(r, c)
                    && let (Some(rid), Some(cid)) = (t.rows.id_at(r), t.cols.id_at(c))
                {
                    t.set_value_by_id(rid, cid, &Value::Empty, false, None);
                    cleared.push((r, c));
                }
            }
        }
        cleared
    }

    /// The anchors whose spill area covers this position (including blocked spills that wanted it).
    fn spill_anchors_covering(&self, tab: TabId, row: u32, col: u32) -> Vec<(u32, u32)> {
        let Some(t) = self.tab(tab) else { return Vec::new() };
        let mut out = Vec::new();
        for (&(st, rid, cid), info) in &self.spills {
            if st != tab {
                continue;
            }
            let (Some(r0), Some(c0)) = (t.rows.pos_of(rid), t.cols.pos_of(cid)) else { continue };
            if row >= r0 && row < r0 + info.rows && col >= c0 && col < c0 + info.cols && (row, col) != (r0, c0) {
                out.push((r0, c0));
            }
        }
        out
    }

    // ---- dependencies ------------------------------------------------------------------------------------

    fn ensure_index(&mut self, id: GroupId) {
        let Some(g) = self.groups.list.get(id as usize).and_then(|g| g.as_ref()) else { return };
        if g.index.is_some() {
            return;
        }
        let tab = g.tab;
        let mut ix = BTreeSet::new();
        if let Some(t) = self.tab(tab) {
            for &(r, c) in &g.members {
                if let (Some(rp), Some(cp)) = (t.rows.pos_of(r), t.cols.pos_of(c)) {
                    ix.insert((cp, rp));
                }
            }
        }
        if let Some(g) = self.groups.get_mut(id) {
            g.index = Some(ix);
        }
    }

    /// Marks dirty every formula cell that reads any cell in `changes` (and what reads those), returning them.
    pub(crate) fn mark_dependents(&mut self, changes: Vec<(TabId, Rect)>) {
        self.mark_dependents_with(changes, true);
    }

    /// `spill_owners`: also dirty spill anchors whose area a change falls in (a write there may block or unblock
    /// them). Off for the anchor's own spill writes.
    fn mark_dependents_with(&mut self, changes: Vec<(TabId, Rect)>, spill_owners: bool) {
        let mut queue = changes;
        let mut first_round = queue.len();
        let mut rounds = 0usize;
        while let Some((tab, rect)) = queue.pop() {
            rounds += 1;
            if rounds > 50_000_000 {
                break;
            }
            let Some(candidates) = self.groups.targets.get(&tab).map(|s| s.iter().copied().collect::<Vec<_>>()) else {
                continue;
            };
            for gid in candidates {
                self.ensure_index(gid);
                let Some(g) = self.groups.get(gid) else { continue };
                let mut hits: Vec<(u32, u32)> = Vec::new();
                for p in &g.refs {
                    if p.tab.unwrap_or(g.tab) != tab {
                        continue;
                    }
                    let Some((rlo, rhi)) = member_interval(p.rows, rect.r1, rect.r2) else { continue };
                    let Some((clo, chi)) = member_interval(p.cols, rect.c1, rect.c2) else { continue };
                    let ix = g.index.as_ref().expect("index built");
                    let (rlo, rhi) = (rlo.max(0) as u32, rhi.min(u32::MAX as i64) as u32);
                    let (clo, chi) = (clo.max(0) as u32, chi.min(u32::MAX as i64) as u32);
                    if clo > chi || rlo > rhi {
                        continue;
                    }
                    for &(c, r) in ix.range((clo, rlo)..=(chi, rhi)) {
                        if r >= rlo && r <= rhi {
                            hits.push((r, c));
                        }
                    }
                }
                if hits.is_empty() {
                    continue;
                }
                let gtab = g.tab;
                hits.sort_unstable_by_key(|&(r, c)| (c, r));
                hits.dedup();
                // New dirty cells, compressed into vertical runs so the next round asks once per run.
                let mut run: Option<(u32, u32, u32)> = None; // (col, start row, end row)
                for (r, c) in hits {
                    if !self.dirty.insert((gtab, c, r)) {
                        continue;
                    }
                    run = match run {
                        Some((rc, s, e)) if rc == c && e + 1 == r => Some((rc, s, r)),
                        Some((rc, s, e)) => {
                            queue.push((gtab, Rect::new(s, rc, e, rc)));
                            Some((c, r, r))
                        }
                        None => Some((c, r, r)),
                    };
                }
                if let Some((rc, s, e)) = run {
                    queue.push((gtab, Rect::new(s, rc, e, rc)));
                }
            }
            // Anything a spill covers belongs to its anchor: a change there may block or unblock it. Only the
            // cells written directly count; cells that recalculated are never written into a spill area.
            let direct = first_round > 0;
            first_round = first_round.saturating_sub(1);
            if spill_owners && direct && rect.rows() == 1 && rect.cols() == 1 && !self.spills.is_empty() {
                for (r, c) in self.spill_anchors_covering(tab, rect.r1, rect.c1) {
                    if self.dirty.insert((tab, c, r)) {
                        queue.push((tab, Rect::new(r, c, r, c)));
                    }
                }
            }
        }
    }

    /// Marks dirty every formula cell inside `rect`.
    pub(crate) fn mark_formulas_in(&mut self, tab: TabId, rect: Rect) {
        let gids: Vec<GroupId> = self.groups.ids().filter(|&g| self.groups.get(g).is_some_and(|g| g.tab == tab)).collect();
        for gid in gids {
            self.ensure_index(gid);
            let g = self.groups.get(gid).expect("present");
            let ix = g.index.as_ref().expect("built");
            let hits: Vec<(u32, u32)> =
                ix.range((rect.c1, rect.r1)..=(rect.c2, rect.r2)).filter(|&&(_, r)| r >= rect.r1 && r <= rect.r2).copied().collect();
            for (c, r) in hits {
                self.dirty.insert((tab, c, r));
            }
        }
    }

    fn mark_volatile(&mut self) {
        let vol: Vec<GroupId> = self.groups.ids().filter(|&g| self.groups.get(g).is_some_and(|g| g.volatile)).collect();
        let mut changed = Vec::new();
        for gid in vol {
            self.ensure_index(gid);
            let g = self.groups.get(gid).expect("present");
            for &(c, r) in g.index.as_ref().expect("built") {
                if self.dirty.insert((g.tab, c, r)) {
                    changed.push((g.tab, Rect::new(r, c, r, c)));
                }
            }
        }
        self.mark_dependents(changed);
    }

    /// Marks every formula in the workbook dirty (after loading a file whose values may be stale).
    pub fn mark_all_dirty(&mut self) {
        let ids: Vec<GroupId> = self.groups.ids().collect();
        for gid in ids {
            self.ensure_index(gid);
            let g = self.groups.get(gid).expect("present");
            let tab = g.tab;
            let cells: Vec<(u32, u32)> = g.index.as_ref().expect("built").iter().copied().collect();
            for (c, r) in cells {
                self.dirty.insert((tab, c, r));
            }
        }
    }

    // ---- recalculation -----------------------------------------------------------------------------------

    /// Recalculates everything that depends on `changes` (and every volatile formula), then returns what changed.
    pub fn recalc(&mut self, changes: Vec<(TabId, Rect)>) -> Recalc {
        let mut report = Recalc::default();
        let mut overflow: HashSet<TabId> = HashSet::new();
        // Formulas written directly need calculating themselves, and everything reading the written cells.
        for (tab, rect) in &changes {
            self.mark_formulas_in(*tab, *rect);
        }
        self.mark_dependents(changes);
        self.mark_volatile();
        let mut stack: Vec<Key> = self.dirty.iter().rev().map(|&(t, c, r)| (t, r, c)).collect();
        let mut waiting: HashSet<Key> = HashSet::new();
        let mut note = |report: &mut Recalc, k: Key| {
            if report.cells.len() < RECALC_REPORT_CAP {
                report.cells.push(k);
            } else {
                overflow.insert(k.0);
            }
        };
        while let Some(key) = stack.pop() {
            let (tab, row, col) = key;
            if !self.dirty.contains(&(tab, col, row)) {
                continue;
            }
            let Some(gid) = self.tab(tab).and_then(|t| t.formula_at(row, col)) else {
                self.dirty.remove(&(tab, col, row));
                continue;
            };
            let Some(expr) = self.groups.get(gid).map(|g| g.expr.clone()) else {
                self.dirty.remove(&(tab, col, row));
                continue;
            };
            let (res, missing) = {
                let view = View { wb: self };
                let mut ctx = Ctx::new(&view, tab, row, col);
                let res = ctx.eval(&expr);
                let res = finish(&mut ctx, res);
                (res, std::mem::take(&mut ctx.missing))
            };
            report.evaluated += 1;
            if !missing.is_empty() {
                let cyclic = missing.iter().any(|m| *m == key || waiting.contains(m));
                if !cyclic {
                    waiting.insert(key);
                    stack.push(key);
                    for m in missing {
                        if self.dirty.contains(&(m.0, m.2, m.1)) {
                            stack.push(m);
                        }
                    }
                    continue;
                }
                let changed = self.write_result(tab, row, col, Out::Value(Value::Error(ErrorKind::Circ)));
                self.dirty.remove(&(tab, col, row));
                waiting.remove(&key);
                for (r, c) in changed {
                    note(&mut report, (tab, r, c));
                }
                continue;
            }
            self.dirty.remove(&(tab, col, row));
            waiting.remove(&key);
            let changed = self.write_result(tab, row, col, res);
            // Spilled cells that changed are read by other formulas, which may already have run.
            let mut followups = Vec::new();
            for &(r, c) in &changed {
                note(&mut report, (tab, r, c));
                if (r, c) != (row, col) {
                    followups.push((tab, Rect::new(r, c, r, c)));
                }
            }
            if !followups.is_empty() {
                let before: HashSet<(TabId, u32, u32)> = self.dirty.iter().copied().collect();
                self.mark_dependents_with(followups, false);
                for &(t, c, r) in self.dirty.iter() {
                    if !before.contains(&(t, c, r)) {
                        stack.push((t, r, c));
                    }
                }
            }
        }
        self.dirty.clear();
        report.overflow = overflow.into_iter().collect();
        report.overflow.sort_unstable();
        report
    }

    /// Writes a formula's result into its cell (and its spill), returning the positions whose value changed.
    fn write_result(&mut self, tab: TabId, row: u32, col: u32, out: Out) -> Vec<(u32, u32)> {
        let mut changed = Vec::new();
        let Some(t) = self.tab(tab) else { return changed };
        let (Some(rid), Some(cid)) = (t.rows.id_at(row), t.cols.id_at(col)) else { return changed };
        let old_spill = self.spills.get(&(tab, rid, cid)).copied();
        let (value, spill) = match out {
            Out::Value(v) => (v, None),
            Out::Spill(arr) => (arr.data.first().cloned().unwrap_or(Value::Empty), Some(arr)),
        };
        // Clear the previous spill first so it never blocks itself, remembering what it showed.
        let mut previous: ahash::HashMap<(u32, u32), Value> = ahash::HashMap::default();
        if let Some(info) = old_spill {
            self.spills.remove(&(tab, rid, cid));
            if !info.blocked {
                let t = self.tab(tab).expect("tab");
                for dr in 0..info.rows {
                    for dc in 0..info.cols {
                        if (dr, dc) != (0, 0) && t.is_spilled_at(row + dr, col + dc) {
                            previous.insert((row + dr, col + dc), t.value_at(row + dr, col + dc));
                        }
                    }
                }
            }
            self.clear_spill_area(tab, rid, cid, info);
        }
        let mut anchor_value = value;
        if let Some(arr) = spill {
            let (h, w) = (arr.rows, arr.cols);
            let t = self.tab(tab).expect("tab");
            let fits = row as u64 + h as u64 <= MAX_ROWS as u64 && col as u64 + w as u64 <= MAX_COLS as u64;
            let mut blocked = !fits;
            if fits {
                'scan: for dr in 0..h {
                    for dc in 0..w {
                        if dr == 0 && dc == 0 {
                            continue;
                        }
                        let cell = t.cell_at(CellPos::new(row + dr, col + dc));
                        if !cell.value.is_empty() || cell.formula.is_some() {
                            blocked = true;
                            break 'scan;
                        }
                    }
                }
            }
            if blocked {
                anchor_value = Value::Error(ErrorKind::Spill);
                self.spills.insert((tab, rid, cid), SpillInfo { rows: h.min(MAX_ROWS), cols: w.min(MAX_COLS), blocked: true });
            } else {
                let t = self.tab_mut(tab).expect("tab");
                t.rows.ensure_len(row + h);
                t.cols.ensure_len(col + w);
                for dr in 0..h {
                    for dc in 0..w {
                        if dr == 0 && dc == 0 {
                            continue;
                        }
                        let v = spill_value(arr.get(dr, dc));
                        let (r2, c2) = (t.rows.id_at(row + dr).expect("ensured"), t.cols.id_at(col + dc).expect("ensured"));
                        let before = previous.remove(&(row + dr, col + dc)).unwrap_or(Value::Empty);
                        t.set_value_by_id(r2, c2, &v, true, None);
                        if before != v {
                            changed.push((row + dr, col + dc));
                        }
                    }
                }
                self.spills.insert((tab, rid, cid), SpillInfo { rows: h, cols: w, blocked: false });
            }
        }
        for (pos, v) in previous {
            if !v.is_empty() {
                changed.push(pos);
            }
        }
        let t = self.tab_mut(tab).expect("tab");
        let before = t.value_at(row, col);
        let v = spill_value(&anchor_value);
        t.set_value_by_id(rid, cid, &v, false, None);
        if before != v {
            changed.push((row, col));
        }
        changed.sort_unstable();
        changed.dedup();
        changed
    }

    // ---- moving rows and columns -------------------------------------------------------------------------

    /// After rows or columns of `tab` were inserted, deleted or moved, rewrites every formula that refers to
    /// them so it keeps pointing at the same cells (Excel's rules), and rebuilds position indexes.
    ///
    /// `map_row`/`map_col` give a reference target's new position from its old one (`None` = deleted);
    /// `old_pos_row`/`old_pos_col` give a member's old position from its id.
    pub(crate) fn rewrite_after_move(&mut self, tab: TabId, change: &AxisChange) {
        self.groups.invalidate_tab(tab);
        let gids: Vec<GroupId> = self.groups.ids().collect();
        // Sorting keeps every formula's R1C1 form (relative references move with their cells) and references
        // into the sorted area keep their positions, as in Excel: nothing to rewrite.
        if matches!(change, AxisChange::Permute { .. }) {
            for g in gids {
                self.ensure_index(g);
            }
            return;
        }
        for gid in gids {
            let Some(g) = self.groups.get(gid) else { continue };
            let refers = g.refs.iter().any(|p| p.tab.unwrap_or(g.tab) == tab);
            if !refers && g.tab != tab {
                continue;
            }
            let gtab = g.tab;
            let expr = g.expr.clone();
            let members: Vec<(RowId, ColId)> = g.members.iter().copied().collect();
            let Some(home) = self.tab(gtab) else { continue };
            // Group members by the rewrite their formula needs; most share one.
            let mut by_new: HashMap<String, (Expr, Vec<(RowId, ColId)>)> = HashMap::new();
            let mut unchanged = true;
            for (rid, cid) in members {
                let (Some(new_r), Some(new_c)) = (home.rows.pos_of(rid), home.cols.pos_of(cid)) else { continue };
                // Old position of this member.
                let (old_r, old_c) = if gtab == tab { change.old_pos(new_r, new_c, rid, cid) } else { (new_r, new_c) };
                let rewritten = rewrite_expr(&expr, gtab, tab, (old_r, old_c), (new_r, new_c), change);
                let key = r1c1_key(&rewritten);
                if key != self.groups.get(gid).map(|g| g.key.as_str()).unwrap_or("") {
                    unchanged = false;
                }
                by_new.entry(key).or_insert_with(|| (rewritten, Vec::new())).1.push((rid, cid));
            }
            if unchanged {
                continue;
            }
            for (_, (new_expr, cells)) in by_new {
                for (rid, cid) in cells {
                    self.regroup(gtab, rid, cid, new_expr.clone());
                }
            }
        }
        let gids: Vec<GroupId> = self.groups.ids().collect();
        for g in gids {
            self.ensure_index(g);
        }
    }
}

/// How a tab's rows or columns moved: enough to map old positions to new ones.
#[derive(Clone, Debug)]
pub(crate) enum AxisChange {
    /// `n` rows (or columns) inserted at position `at`.
    Insert { rows: bool, at: u32, n: u32 },
    /// The positions `at..at+n` deleted.
    Delete { rows: bool, at: u32, n: u32 },
    /// Positions permuted: `old_of_new[i]` is the old position now at `start + i`.
    Permute { rows: bool, start: u32, old_of_new: Vec<u32> },
}

impl AxisChange {
    fn is_rows(&self) -> bool {
        match self {
            Self::Insert { rows, .. } | Self::Delete { rows, .. } | Self::Permute { rows, .. } => *rows,
        }
    }

    /// New position of an old position on the changed axis (`None` = deleted).
    fn map(&self, old: u32) -> Option<u32> {
        match self {
            Self::Insert { at, n, .. } => Some(if old >= *at { old + n } else { old }),
            Self::Delete { at, n, .. } => {
                if old < *at {
                    Some(old)
                } else if old < at + n {
                    None
                } else {
                    Some(old - n)
                }
            }
            // A reference into a sorted area keeps pointing at the same position, as in Excel.
            Self::Permute { .. } => Some(old),
        }
    }

    /// For ranges: the new position of an endpoint (deleted endpoints snap inward).
    fn map_end(&self, old: u32, is_start: bool) -> Option<u32> {
        match self {
            Self::Delete { at, n, .. } if old >= *at && old < at + n => {
                if is_start {
                    Some(*at)
                } else if *at == 0 {
                    None
                } else {
                    Some(at - 1)
                }
            }
            _ => self.map(old),
        }
    }

    fn old_pos(&self, new_r: u32, new_c: u32, _rid: RowId, _cid: ColId) -> (u32, u32) {
        let unmap = |p: u32| -> u32 {
            match self {
                Self::Insert { at, n, .. } => {
                    if p >= at + n {
                        p - n
                    } else {
                        p
                    }
                }
                Self::Delete { at, n, .. } => {
                    if p >= *at {
                        p + n
                    } else {
                        p
                    }
                }
                Self::Permute { start, old_of_new, .. } => {
                    if p >= *start && ((p - start) as usize) < old_of_new.len() {
                        old_of_new[(p - start) as usize]
                    } else {
                        p
                    }
                }
            }
        };
        if self.is_rows() { (unmap(new_r), new_c) } else { (new_r, unmap(new_c)) }
    }
}

/// Rewrites one formula of a cell that moved from `old` to `new` (positions), after `change` happened to `tab`.
fn rewrite_expr(expr: &Expr, home_tab: TabId, tab: TabId, old: (u32, u32), new: (u32, u32), change: &AxisChange) -> Expr {
    let rows_axis = change.is_rows();
    expr.map_refs(&mut |e| {
        let target_tab = match e {
            Expr::Cell(c) | Expr::Spill(c) => c.tab.unwrap_or(home_tab),
            Expr::Range(r) => r.tab.unwrap_or(home_tab),
            _ => return None,
        };
        let affected = target_tab == tab;
        // Rewrites one coordinate: the old target is resolved from the old position, mapped, and written relative
        // to the new position.
        let fix = |c: Coord, old_at: u32, new_at: u32, axis_changed: bool, end: Option<bool>| -> Option<Coord> {
            let target = c.resolve(old_at)?;
            let mapped = if axis_changed {
                match end {
                    Some(is_start) => change.map_end(target, is_start)?,
                    None => change.map(target)?,
                }
            } else {
                target
            };
            Some(match c {
                Coord::Abs(_) => Coord::Abs(mapped),
                Coord::Rel(_) => Coord::Rel(mapped as i64 as i32 - new_at as i32),
            })
        };
        let (row_changed, col_changed) = (affected && rows_axis, affected && !rows_axis);
        match e {
            Expr::Cell(c) | Expr::Spill(c) => {
                let row = fix(c.row, old.0, new.0, row_changed, None);
                let col = fix(c.col, old.1, new.1, col_changed, None);
                match (row, col) {
                    (Some(row), Some(col)) => {
                        let nc = crate::formula::ast::CellRef { tab: c.tab, row, col };
                        Some(if matches!(e, Expr::Spill(_)) { Expr::Spill(nc) } else { Expr::Cell(nc) })
                    }
                    _ => Some(Expr::Error(ErrorKind::Ref)),
                }
            }
            Expr::Range(r) => {
                let rows = match r.rows {
                    None => Some(None),
                    Some((a, b)) => {
                        // Order the endpoints so deletions snap the right way.
                        let (ta, tb) = (a.resolve(old.0), b.resolve(old.0));
                        let swap = matches!((ta, tb), (Some(x), Some(y)) if x > y);
                        let (a, b) = if swap { (b, a) } else { (a, b) };
                        match (fix(a, old.0, new.0, row_changed, Some(true)), fix(b, old.0, new.0, row_changed, Some(false))) {
                            (Some(x), Some(y)) => {
                                let valid = match (x.resolve(new.0), y.resolve(new.0)) {
                                    (Some(p), Some(q)) => p <= q,
                                    _ => false,
                                };
                                if valid { Some(Some((x, y))) } else { None }
                            }
                            _ => None,
                        }
                    }
                };
                let cols = match r.cols {
                    None => Some(None),
                    Some((a, b)) => {
                        let (ta, tb) = (a.resolve(old.1), b.resolve(old.1));
                        let swap = matches!((ta, tb), (Some(x), Some(y)) if x > y);
                        let (a, b) = if swap { (b, a) } else { (a, b) };
                        match (fix(a, old.1, new.1, col_changed, Some(true)), fix(b, old.1, new.1, col_changed, Some(false))) {
                            (Some(x), Some(y)) => {
                                let valid = match (x.resolve(new.1), y.resolve(new.1)) {
                                    (Some(p), Some(q)) => p <= q,
                                    _ => false,
                                };
                                if valid { Some(Some((x, y))) } else { None }
                            }
                            _ => None,
                        }
                    }
                };
                match (rows, cols) {
                    (Some(rows), Some(cols)) => Some(Expr::Range(crate::formula::ast::RangeRef { tab: r.tab, rows, cols })),
                    _ => Some(Expr::Error(ErrorKind::Ref)),
                }
            }
            _ => None,
        }
    })
}

/// The member positions whose pattern (on one axis) covers some position in `p1..=p2`.
fn member_interval(pat: Option<(Coord, Coord)>, p1: u32, p2: u32) -> Option<(i64, i64)> {
    const ALL: (i64, i64) = (i64::MIN, i64::MAX);
    let Some((a, b)) = pat else { return Some(ALL) };
    let (p1, p2) = (p1 as i64, p2 as i64);
    match (a, b) {
        (Coord::Abs(x), Coord::Abs(y)) => {
            let (lo, hi) = (x.min(y) as i64, x.max(y) as i64);
            (p1 <= hi && lo <= p2).then_some(ALL)
        }
        (Coord::Rel(d1), Coord::Rel(d2)) => {
            let (lo, hi) = (d1.min(d2) as i64, d1.max(d2) as i64);
            Some((p1 - hi, p2 - lo))
        }
        (Coord::Abs(x), Coord::Rel(d)) | (Coord::Rel(d), Coord::Abs(x)) => {
            let (x, d) = (x as i64, d as i64);
            if p1 <= x && x <= p2 {
                Some(ALL)
            } else if p1 > x {
                Some((p1 - d, i64::MAX))
            } else {
                Some((i64::MIN, p2 - d))
            }
        }
    }
}

/// A formula's final answer.
pub(crate) enum Out {
    Value(Value),
    Spill(Arc<Array>),
}

fn spill_value(v: &Value) -> Value {
    match v {
        Value::Empty => Value::Number(0.0),
        other => other.clone(),
    }
}

/// Turns what the top of a formula evaluated to into what the cell shows.
fn finish(ctx: &mut Ctx<'_>, res: Res) -> Out {
    match res {
        Res::V(v) => Out::Value(v),
        Res::R(area) => {
            if area.is_cell() {
                Out::Value(ctx.cell(area.tab, area.rect.r1, area.rect.c1))
            } else {
                let arr = ctx.to_array(&Res::R(area));
                if arr.rows == 0 || arr.cols == 0 {
                    Out::Value(Value::Number(0.0))
                } else if arr.rows == 1 && arr.cols == 1 {
                    Out::Value(arr.data[0].clone())
                } else {
                    Out::Spill(arr)
                }
            }
        }
        Res::A(arr) => {
            if arr.rows == 0 || arr.cols == 0 {
                Out::Value(Value::Error(ErrorKind::Calc))
            } else if arr.rows == 1 && arr.cols == 1 {
                Out::Value(arr.data[0].clone())
            } else {
                Out::Spill(arr)
            }
        }
        // A LAMBDA left uncalled: a cell can't show it (Excel shows #CALC!).
        other => Out::Value(other.shown_error()),
    }
}

/// The workbook as formulas see it.
pub(crate) struct View<'a> {
    pub wb: &'a Workbook,
}

impl Grid for View<'_> {
    fn has_javascript(&self, name: &str) -> bool { self.wb.function(name).is_some() }
    fn javascript(&self, name: &str, tab: u32, row: u32, col: u32, args: Vec<serde_json::Value>) -> Res {
        self.wb.javascript_call(name, tab, row, col, args)
    }

    fn value(&self, tab: TabId, row: u32, col: u32) -> Value {
        self.wb.tab(tab).map(|t| t.value_at(row, col)).unwrap_or_default()
    }

    fn extent(&self, tab: TabId) -> Option<(u32, u32)> {
        self.wb.tab(tab).map(|t| (t.rows.len(), t.cols.len()))
    }

    fn pending(&self, tab: TabId, rect: &Rect, out: &mut Vec<Key>) {
        let dirty = &self.wb.dirty;
        if dirty.is_empty() {
            return;
        }
        if rect.cols() <= 64 {
            for c in rect.c1..=rect.c2 {
                for &(t, cc, r) in dirty.range((tab, c, rect.r1)..=(tab, c, rect.r2)) {
                    out.push((t, r, cc));
                }
            }
        } else {
            for &(t, c, r) in dirty.range((tab, rect.c1, 0)..=(tab, rect.c2, u32::MAX)) {
                if r >= rect.r1 && r <= rect.r2 {
                    out.push((t, r, c));
                }
            }
        }
    }

    fn tab_id(&self, name: &str) -> Option<TabId> {
        self.wb.names().tab_id(name)
    }

    fn tab_name(&self, tab: TabId) -> Option<String> {
        self.wb.tab(tab).map(|t| t.name.clone())
    }

    fn tab_position(&self, tab: TabId) -> Option<usize> {
        self.wb.tab_index.get(&tab).copied()
    }

    fn tab_count(&self) -> usize {
        self.wb.tabs.len()
    }

    fn defined_name(&self, name: &str, tab: TabId) -> Option<Expr> {
        let def = self
            .wb
            .names
            .iter()
            .find(|n| n.name.eq_ignore_ascii_case(name) && n.tab == Some(tab))
            .or_else(|| self.wb.names.iter().find(|n| n.name.eq_ignore_ascii_case(name) && n.tab.is_none()))?;
        let text = def.text.strip_prefix('=').unwrap_or(&def.text);
        parse_formula(text, 0, 0, &self.wb.names()).ok()
    }

    fn spill_extent(&self, tab: TabId, row: u32, col: u32) -> Option<(u32, u32)> {
        let t = self.wb.tab(tab)?;
        let (rid, cid) = (t.rows.id_at(row)?, t.cols.id_at(col)?);
        let info = self.wb.spills.get(&(tab, rid, cid))?;
        (!info.blocked).then_some((info.rows, info.cols))
    }

    fn formula_text(&self, tab: TabId, row: u32, col: u32) -> Option<String> {
        self.wb.formula_text(tab, row, col).map(|f| format!("={f}"))
    }

    fn number_format(&self, tab: TabId, row: u32, col: u32) -> Option<String> {
        let t = self.wb.tab(tab)?;
        let cell = t.cell_at(CellPos::new(row, col));
        self.wb.styles.get(cell.style).num_fmt.clone()
    }

    fn now(&self) -> f64 {
        crate::dates::serial_from_unix(self.wb.clock_unix + self.wb.settings.utc_offset_minutes as f64 * 60.0)
    }

    fn random(&self) -> f64 {
        use rand::Rng as _;
        self.wb.rng.lock().map(|mut r| r.random::<f64>()).unwrap_or(0.5)
    }

    fn visit(&self, tab: TabId, rect: &Rect, f: &mut dyn FnMut(u32, u32, &Value) -> bool) {
        let Some(t) = self.wb.tab(tab) else { return };
        for row in rect.r1..=rect.r2 {
            for col in rect.c1..=rect.c2 {
                if !f(row, col, &t.value_at(row, col)) {
                    return;
                }
            }
        }
    }

    fn visit_kinds(&self, tab: TabId, rect: &Rect, f: &mut dyn FnMut(u32, u32, crate::formula::eval::Kind) -> bool) {
        use crate::{formula::eval::Kind, model};
        let Some(t) = self.wb.tab(tab) else { return };
        for row in rect.r1..=rect.r2 {
            for col in rect.c1..=rect.c2 {
                let (tag, data) = t.raw_at(row, col);
                let k = match tag {
                    model::TAG_NUMBER => Kind::Number(f64::from_bits(data)),
                    model::TAG_TEXT => Kind::Text,
                    model::TAG_BOOL => Kind::Bool(data != 0),
                    model::TAG_ERROR => Kind::Error(ErrorKind::from_code(data as u8)),
                    _ => Kind::Empty,
                };
                if !f(row, col, k) {
                    return;
                }
            }
        }
    }

    fn hidden_rows(&self, tab: TabId) -> Vec<u32> {
        let Some(t) = self.wb.tab(tab) else { return Vec::new() };
        let mut out: Vec<u32> = t.hidden_rows.iter().filter_map(|&id| t.rows.pos_of(id)).collect();
        out.sort_unstable();
        out
    }

    fn formula_tree(&self, tab: TabId, row: u32, col: u32) -> Option<Arc<Expr>> {
        let t = self.wb.tab(tab)?;
        self.wb.groups.get(t.formula_at(row, col)?).map(|g| g.expr.clone())
    }

    fn col_width(&self, tab: TabId, col: u32) -> Option<f64> {
        let t = self.wb.tab(tab)?;
        t.col_widths.get(&t.cols.id_at(col)?).map(|&w| w as f64)
    }

    fn cell_style(&self, tab: TabId, row: u32, col: u32) -> Option<Style> {
        let t = self.wb.tab(tab)?;
        Some(self.wb.styles.get(t.cell_at(CellPos::new(row, col)).style).clone())
    }
}

/// Evaluates a formula once, against the workbook as it is, as if it were in `tab` at (`row`, `col`).
/// Used for previews and for code that asks for a formula's value without writing it.
pub fn evaluate_detached(wb: &Workbook, tab: TabId, row: u32, col: u32, text: &str) -> Result<Value, ParseError> {
    let expr = parse_formula(text, row, col, &wb.names())?;
    let view = View { wb };
    let mut ctx = Ctx::new(&view, tab, row, col);
    let res = ctx.eval(&expr);
    Ok(match finish(&mut ctx, res) {
        Out::Value(v) => v,
        Out::Spill(a) => a.data.first().cloned().unwrap_or_default(),
    })
}

impl Workbook {
    /// Reads a cell by position with everything a viewer needs.
    pub fn cell(&self, tab: TabId, row: u32, col: u32) -> Option<Cell> {
        self.tab(tab).map(|t| t.cell_at(CellPos::new(row, col)))
    }

    /// The author uuid and Unix time of a cell's last change.
    pub fn cell_author(&self, meta: Meta) -> Option<(String, i64)> {
        let (a, t) = crate::model::unpack_meta(meta)?;
        Some((self.authors.get(a)?.to_owned(), t))
    }

    /// The area a range text names (`A1:B9`, `Sales!A:A`), resolved against `default_tab`.
    pub fn resolve_area(&self, text: &str, default_tab: TabId) -> Option<Area> {
        let (tab_name, rest) = crate::addr::split_tab(text);
        let tab = match tab_name {
            Some(n) => self.tab_by_name(&n)?.id,
            None => default_tab,
        };
        let rect = Rect::parse(rest, MAX_ROWS, MAX_COLS)?;
        Some(Area { tab, rect })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::pack_meta;

    fn put(wb: &mut Workbook, tab: TabId, a1: &str, entry: &str) {
        let p = CellPos::parse(a1).expect("address");
        let t = wb.tab_mut(tab).expect("tab");
        let (r, c) = (t.rows.id_at_or_create(p.row), t.cols.id_at_or_create(p.col));
        if let Some(f) = entry.strip_prefix('=') {
            wb.put_formula(tab, r, c, f, (p.row, p.col)).expect("formula");
        } else {
            wb.clear_formula(tab, r, c);
            let v = match crate::input::parse_entry(entry) {
                crate::input::Entry::Number(n, _) => Value::Number(n),
                crate::input::Entry::Text(s) => Value::from(s),
                crate::input::Entry::Bool(b) => Value::Bool(b),
                _ => Value::Empty,
            };
            wb.tab_mut(tab).expect("tab").set_value_by_id(r, c, &v, false, Some(pack_meta(0, 1_700_000_000)));
        }
        wb.recalc(vec![(tab, Rect::cell(p))]);
    }

    fn get(wb: &Workbook, tab: TabId, a1: &str) -> Value {
        let p = CellPos::parse(a1).expect("address");
        wb.tab(tab).expect("tab").value_at(p.row, p.col)
    }

    #[test]
    fn chain_recalculates() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        put(&mut wb, t, "A1", "2");
        put(&mut wb, t, "B1", "=A1*10");
        put(&mut wb, t, "C1", "=B1+A1");
        assert_eq!(get(&wb, t, "C1"), Value::Number(22.0));
        put(&mut wb, t, "A1", "3");
        assert_eq!(get(&wb, t, "B1"), Value::Number(30.0));
        assert_eq!(get(&wb, t, "C1"), Value::Number(33.0));
    }

    #[test]
    fn filled_formulas_share_a_group() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        for r in 1..=100 {
            put(&mut wb, t, &format!("A{r}"), &r.to_string());
            put(&mut wb, t, &format!("B{r}"), &format!("=A{r}*2"));
        }
        assert_eq!(wb.groups.ids().count(), 1);
        assert_eq!(get(&wb, t, "B50"), Value::Number(100.0));
        put(&mut wb, t, "A50", "7");
        assert_eq!(get(&wb, t, "B50"), Value::Number(14.0));
        assert_eq!(get(&wb, t, "B51"), Value::Number(102.0));
    }

    #[test]
    fn long_chain_does_not_overflow() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        put(&mut wb, t, "A1", "1");
        {
            let tab = wb.tab_mut(t).expect("tab");
            tab.rows.ensure_len(200_000);
            tab.cols.ensure_len(1);
        }
        for r in 1..200_000u32 {
            let (rid, cid) = { let tab = wb.tab(t).expect("tab"); (tab.rows.id_at(r).expect("row"), tab.cols.id_at(0).expect("col")) };
            wb.put_formula(t, rid, cid, &format!("A{}+1", r), (r, 0)).expect("formula");
        }
        let report = wb.recalc(vec![(t, Rect::new(0, 0, 199_999, 0))]);
        assert!(report.evaluated >= 199_999);
        assert_eq!(get(&wb, t, "A200000"), Value::Number(200_000.0));
        put(&mut wb, t, "A1", "5");
        assert_eq!(get(&wb, t, "A200000"), Value::Number(200_004.0));
    }

    #[test]
    fn cycles_are_errors() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        put(&mut wb, t, "A1", "=B1+1");
        put(&mut wb, t, "B1", "=A1+1");
        assert!(matches!(get(&wb, t, "A1"), Value::Error(_)) || matches!(get(&wb, t, "B1"), Value::Error(_)));
        put(&mut wb, t, "C1", "=C1");
        assert_eq!(get(&wb, t, "C1"), Value::Error(ErrorKind::Circ));
    }

    #[test]
    fn ranges_and_absolute_refs() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        put(&mut wb, t, "A1", "10");
        put(&mut wb, t, "A2", "20");
        put(&mut wb, t, "A3", "30");
        put(&mut wb, t, "B1", "=SUM(A:A)");
        put(&mut wb, t, "C1", "=A1/$A$3");
        put(&mut wb, t, "C2", "=A2/$A$3");
        assert_eq!(get(&wb, t, "B1"), Value::Number(60.0));
        put(&mut wb, t, "A3", "40");
        assert_eq!(get(&wb, t, "B1"), Value::Number(70.0));
        assert_eq!(get(&wb, t, "C2"), Value::Number(0.5));
    }

    #[test]
    fn spills_and_blocks() {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        put(&mut wb, t, "A1", "={1;2;3}");
        assert_eq!(get(&wb, t, "A3"), Value::Number(3.0));
        put(&mut wb, t, "B1", "=SUM(A1#)");
        assert_eq!(get(&wb, t, "B1"), Value::Number(6.0));
        put(&mut wb, t, "A2", "x");
        assert_eq!(get(&wb, t, "A1"), Value::Error(ErrorKind::Spill));
        put(&mut wb, t, "A2", "");
        assert_eq!(get(&wb, t, "A1"), Value::Number(1.0));
        assert_eq!(get(&wb, t, "A2"), Value::Number(2.0));
    }

    #[test]
    fn cross_tab() {
        let mut wb = Workbook::new();
        let t1 = wb.tabs[0].id;
        let t2 = wb.add_tab("Sales", None);
        put(&mut wb, t2, "B2", "5");
        put(&mut wb, t1, "A1", "=Sales!B2*2");
        assert_eq!(get(&wb, t1, "A1"), Value::Number(10.0));
        put(&mut wb, t2, "B2", "6");
        assert_eq!(get(&wb, t1, "A1"), Value::Number(12.0));
    }
}

/// A conditional formula anchored at the range's top-left, evaluated for another cell.
pub fn evaluate_relative(wb: &Workbook, tab: TabId, row: u32, col: u32, text: &str, anchor_row: u32, anchor_col: u32) -> Result<Value, ParseError> {
    let expr = parse_formula(text.trim().trim_start_matches('='), anchor_row, anchor_col, &wb.names())?;
    let mut unsafe_call = false;
    expr.walk(&mut |e| if let Expr::Call(name,_) = e { if !crate::formula::functions::is_known(name) { unsafe_call = true; } });
    if unsafe_call { return Ok(Value::Error(ErrorKind::Name)); }
    let view = View { wb };
    let mut ctx = Ctx::new(&view, tab, row, col);
    let result = ctx.eval(&expr);
    Ok(ctx.to_scalar(result))
}
