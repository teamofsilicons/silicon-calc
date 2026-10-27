//! The workbook in memory: tabs, their rows and columns (with stable ids), and cell storage.
//!
//! Cells are stored by `(row id, column id)`, never by position, so inserting, deleting or sorting rows only
//! changes the position → id arrays and never moves cell data. Columns are split into chunks of 1,024 row ids.

use std::{collections::BTreeMap, sync::Arc};

use ahash::HashMap;
use serde::{Deserialize, Serialize};

use crate::{
    addr::{CellPos, MAX_COLS, MAX_ROWS, Rect},
    formula::ast::TabId,
    value::{ErrorKind, Value},
};

pub type RowId = u32;
pub type ColId = u32;
/// A formula group's id (all cells holding the same R1C1 formula in one tab share it).
pub type GroupId = u32;

pub const CHUNK: usize = 1024;
const NONE: u32 = u32::MAX;

pub(crate) const TAG_EMPTY: u8 = 0;
pub(crate) const TAG_NUMBER: u8 = 1;
pub(crate) const TAG_TEXT: u8 = 2;
pub(crate) const TAG_BOOL: u8 = 3;
pub(crate) const TAG_ERROR: u8 = 4;
/// The value was written by a spilling formula elsewhere.
pub(crate) const FLAG_SPILLED: u8 = 0x80;
const TAG_MASK: u8 = 0x0f;

/// Seconds since 2020-01-01T00:00:00Z, the unit cell times are kept in.
pub const TIME_EPOCH_UNIX: i64 = 1_577_836_800;

/// Who last changed a cell and when: `(author index << 32) | seconds since TIME_EPOCH_UNIX`. 0 means unknown.
pub type Meta = u64;

pub fn pack_meta(author: u32, unix_secs: i64) -> Meta {
    let t = (unix_secs - TIME_EPOCH_UNIX).clamp(0, u32::MAX as i64) as u64;
    ((author as u64 + 1) << 32) | t
}

pub fn unpack_meta(m: Meta) -> Option<(u32, i64)> {
    if m == 0 {
        return None;
    }
    Some((((m >> 32) as u32).wrapping_sub(1), (m & 0xffff_ffff) as i64 + TIME_EPOCH_UNIX))
}

/// One axis of a tab: the order of row (or column) ids.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Axis {
    order: Vec<u32>,
    #[serde(skip)]
    pos_of: Vec<u32>,
    next: u32,
}

impl Axis {
    pub(crate) fn remapped(&self, ids: &std::collections::BTreeMap<u32, u32>) -> Self {
        let order: Vec<_> = self.order.iter().map(|id| ids.get(id).copied().unwrap_or(*id)).collect();
        let next = ids.values().chain(order.iter()).copied().max().map_or(self.next, |id| self.next.max(id + 1));
        let mut axis = Self { order, next, pos_of: Vec::new() };
        axis.rebuild();
        axis
    }

    pub fn rebuild(&mut self) {
        self.pos_of = vec![NONE; self.next as usize];
        for (p, &id) in self.order.iter().enumerate() {
            self.pos_of[id as usize] = p as u32;
        }
    }

    /// How many positions have an id (the rest of the grid is empty and gets ids when first written).
    pub fn len(&self) -> u32 {
        self.order.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn id_at(&self, pos: u32) -> Option<u32> {
        self.order.get(pos as usize).copied()
    }

    pub fn pos_of(&self, id: u32) -> Option<u32> {
        self.pos_of.get(id as usize).copied().filter(|&p| p != NONE)
    }

    pub fn ids(&self) -> &[u32] {
        &self.order
    }

    pub fn next_id(&self) -> u32 {
        self.next
    }

    /// Makes sure positions `0..len` have ids.
    pub fn ensure_len(&mut self, len: u32) {
        while (self.order.len() as u32) < len {
            let id = self.next;
            self.next += 1;
            self.pos_of.push(self.order.len() as u32);
            self.order.push(id);
        }
    }

    /// The id at `pos`, giving positions up to it ids if they have none yet.
    pub fn id_at_or_create(&mut self, pos: u32) -> u32 {
        self.ensure_len(pos + 1);
        self.order[pos as usize]
    }

    /// Inserts `n` new ids at `pos` and returns them.
    pub fn insert(&mut self, pos: u32, n: u32) -> Vec<u32> {
        self.ensure_len(pos);
        let ids: Vec<u32> = (0..n)
            .map(|_| {
                let id = self.next;
                self.next += 1;
                self.pos_of.push(NONE);
                id
            })
            .collect();
        let p = pos as usize;
        self.order.splice(p..p, ids.iter().copied());
        for (i, &id) in self.order.iter().enumerate().skip(p) {
            self.pos_of[id as usize] = i as u32;
        }
        ids
    }

    /// Inserts existing ids (bringing back deleted rows) at `pos`.
    pub fn insert_ids(&mut self, pos: u32, ids: &[u32]) {
        self.ensure_len(pos);
        for &id in ids {
            while self.pos_of.len() <= id as usize {
                self.pos_of.push(NONE);
            }
            self.next = self.next.max(id + 1);
        }
        let p = pos as usize;
        self.order.splice(p..p, ids.iter().copied());
        for (i, &id) in self.order.iter().enumerate().skip(p) {
            self.pos_of[id as usize] = i as u32;
        }
    }

    /// Removes `n` positions from `pos` and returns their ids.
    pub fn remove(&mut self, pos: u32, n: u32) -> Vec<u32> {
        let p = (pos as usize).min(self.order.len());
        let end = (p + n as usize).min(self.order.len());
        let removed: Vec<u32> = self.order.drain(p..end).collect();
        for &id in &removed {
            self.pos_of[id as usize] = NONE;
        }
        for (i, &id) in self.order.iter().enumerate().skip(p) {
            self.pos_of[id as usize] = i as u32;
        }
        removed
    }

    /// Replaces the order of positions `start..start+ids.len()` (sorting, moving).
    pub fn reorder(&mut self, start: u32, ids: &[u32]) {
        let s = start as usize;
        self.order[s..s + ids.len()].copy_from_slice(ids);
        for (i, &id) in ids.iter().enumerate() {
            self.pos_of[id as usize] = (s + i) as u32;
        }
    }
}

/// Text interned per tab.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Strings {
    list: Vec<Arc<str>>,
    #[serde(skip)]
    index: HashMap<Arc<str>, u32>,
}

impl Strings {
    pub fn rebuild(&mut self) {
        self.index = self.list.iter().enumerate().map(|(i, s)| (s.clone(), i as u32)).collect();
    }

    pub fn intern(&mut self, s: &Arc<str>) -> u32 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(s.clone());
        self.index.insert(s.clone(), i);
        i
    }

    pub fn get(&self, i: u32) -> Arc<str> {
        self.list.get(i as usize).cloned().unwrap_or_else(|| Arc::from(""))
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Chunk {
    pub tags: Vec<u8>,
    pub data: Vec<u64>,
    #[serde(default)]
    pub formula: Option<Vec<u32>>,
    #[serde(default)]
    pub style: Option<Vec<u32>>,
    #[serde(default)]
    pub meta: Option<Vec<u64>>,
    pub default_meta: u64,
}

impl Chunk {
    fn new(default_meta: u64) -> Self {
        Self { tags: vec![0; CHUNK], data: vec![0; CHUNK], formula: None, style: None, meta: None, default_meta }
    }

    fn is_blank(&self) -> bool {
        self.tags.iter().all(|&t| t == 0)
            && self.formula.as_ref().is_none_or(|f| f.iter().all(|&g| g == 0))
            && self.style.as_ref().is_none_or(|s| s.iter().all(|&g| g == 0))
    }
}

/// A column's cells, by row id.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Column {
    pub chunks: Vec<Option<Box<Chunk>>>,
}

impl Column {
    fn chunk(&self, row: RowId) -> Option<(&Chunk, usize)> {
        let ci = row as usize / CHUNK;
        self.chunks.get(ci).and_then(|c| c.as_deref()).map(|c| (c, row as usize % CHUNK))
    }

    fn chunk_mut(&mut self, row: RowId, default_meta: u64) -> (&mut Chunk, usize) {
        let ci = row as usize / CHUNK;
        if self.chunks.len() <= ci {
            self.chunks.resize_with(ci + 1, || None);
        }
        let c = self.chunks[ci].get_or_insert_with(|| Box::new(Chunk::new(default_meta)));
        (c, row as usize % CHUNK)
    }
}

/// What a cell holds, as the engine reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub value: Value,
    pub formula: Option<GroupId>,
    pub style: u32,
    pub meta: Meta,
    pub spilled: bool,
}

/// The code a tab was made by.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodeSource {
    /// `javascript` for browser commands; historical snapshots may retain a previous language label.
    pub language: String,
    pub code: String,
    /// The tabs it reads, by id.
    pub inputs: Vec<TabId>,
    /// The account that ran it (uuid).
    pub run_by: String,
    /// Unix seconds.
    pub run_at: i64,
    /// Rerun when an input changes.
    pub keep_updated: bool,
}

/// A merged area, by ids so it survives inserts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdRect {
    pub r1: RowId,
    pub c1: ColId,
    pub r2: RowId,
    pub c2: ColId,
}

/// A range only some accounts may change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Protection {
    pub id: String,
    pub tab: TabId,
    /// None protects the whole tab.
    pub area: Option<IdRect>,
    /// Account uuids allowed to change it (the sheet's owner always may).
    pub editors: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// A chart drawn from a range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chart {
    pub id: String,
    pub tab: TabId,
    /// `line`, `bar`, `column`, `area`, `pie`, `scatter`.
    pub kind: String,
    pub title: String,
    /// The data, as A1 text (`A1:C20` or `Sales!A1:C20`); the first row and column may be labels.
    pub range: String,
    /// Where it sits: top-left cell position and size in pixels.
    pub anchor_row: u32,
    pub anchor_col: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tab {
    pub id: TabId,
    pub name: String,
    pub rows: Axis,
    pub cols: Axis,
    pub(crate) columns: Vec<Option<Column>>,
    pub strings: Strings,
    #[serde(default)]
    pub col_widths: BTreeMap<ColId, f32>,
    #[serde(default)]
    pub row_heights: BTreeMap<RowId, f32>,
    #[serde(default)]
    pub hidden_rows: Vec<RowId>,
    #[serde(default)]
    pub hidden_cols: Vec<ColId>,
    #[serde(default)]
    pub frozen_rows: u32,
    #[serde(default)]
    pub frozen_cols: u32,
    #[serde(default)]
    pub merges: Vec<IdRect>,
    #[serde(default)]
    pub code: Option<CodeSource>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub format_rules: Vec<crate::conditional::Rule>,
}

impl Tab {
    pub fn new(id: TabId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            rows: Axis::default(),
            cols: Axis::default(),
            columns: Vec::new(),
            strings: Strings::default(),
            col_widths: BTreeMap::new(),
            row_heights: BTreeMap::new(),
            hidden_rows: Vec::new(),
            hidden_cols: Vec::new(),
            frozen_rows: 0,
            frozen_cols: 0,
            merges: Vec::new(),
            code: None,
            color: None,
            format_rules: Vec::new(),
        }
    }

    pub(crate) fn rebuild_indexes(&mut self) {
        self.rows.rebuild();
        self.cols.rebuild();
        self.strings.rebuild();
    }

    fn column(&self, col: ColId) -> Option<&Column> {
        self.columns.get(col as usize).and_then(|c| c.as_ref())
    }

    fn column_mut(&mut self, col: ColId) -> &mut Column {
        if self.columns.len() <= col as usize {
            self.columns.resize_with(col as usize + 1, || None);
        }
        self.columns[col as usize].get_or_insert_with(Column::default)
    }

    /// The cell with these ids.
    pub fn cell_by_id(&self, row: RowId, col: ColId) -> Cell {
        let Some((chunk, i)) = self.column(col).and_then(|c| c.chunk(row)) else {
            return Cell::default();
        };
        Cell {
            value: self.decode(chunk.tags[i], chunk.data[i]),
            formula: chunk.formula.as_ref().map(|f| f[i]).filter(|&g| g != 0).map(|g| g - 1),
            style: chunk.style.as_ref().map(|s| s[i]).unwrap_or(0),
            meta: chunk.meta.as_ref().map(|m| m[i]).filter(|&m| m != 0).unwrap_or(chunk.default_meta),
            spilled: chunk.tags[i] & FLAG_SPILLED != 0,
        }
    }

    /// The value at a position (fast path for formulas).
    pub fn value_at(&self, row: u32, col: u32) -> Value {
        let (Some(r), Some(c)) = (self.rows.id_at(row), self.cols.id_at(col)) else {
            return Value::Empty;
        };
        match self.column(c).and_then(|cc| cc.chunk(r)) {
            Some((chunk, i)) => self.decode(chunk.tags[i], chunk.data[i]),
            None => Value::Empty,
        }
    }

    /// The number at a position, if it holds one (fast path for SUM over big ranges).
    pub(crate) fn raw_at(&self, row: u32, col: u32) -> (u8, u64) {
        let (Some(r), Some(c)) = (self.rows.id_at(row), self.cols.id_at(col)) else {
            return (0, 0);
        };
        match self.column(c).and_then(|cc| cc.chunk(r)) {
            Some((chunk, i)) => (chunk.tags[i] & TAG_MASK, chunk.data[i]),
            None => (0, 0),
        }
    }

    pub fn cell_at(&self, pos: CellPos) -> Cell {
        match (self.rows.id_at(pos.row), self.cols.id_at(pos.col)) {
            (Some(r), Some(c)) => self.cell_by_id(r, c),
            _ => Cell::default(),
        }
    }

    pub fn formula_at(&self, row: u32, col: u32) -> Option<GroupId> {
        let (r, c) = (self.rows.id_at(row)?, self.cols.id_at(col)?);
        let (chunk, i) = self.column(c)?.chunk(r)?;
        chunk.formula.as_ref().map(|f| f[i]).filter(|&g| g != 0).map(|g| g - 1)
    }

    pub(crate) fn is_spilled_at(&self, row: u32, col: u32) -> bool {
        let (Some(r), Some(c)) = (self.rows.id_at(row), self.cols.id_at(col)) else {
            return false;
        };
        self.column(c).and_then(|cc| cc.chunk(r)).is_some_and(|(chunk, i)| chunk.tags[i] & FLAG_SPILLED != 0)
    }

    pub(crate) fn decode(&self, tag: u8, data: u64) -> Value {
        match tag & TAG_MASK {
            TAG_NUMBER => Value::Number(f64::from_bits(data)),
            TAG_TEXT => Value::Text(self.strings.get(data as u32)),
            TAG_BOOL => Value::Bool(data != 0),
            TAG_ERROR => Value::Error(ErrorKind::from_code(data as u8)),
            _ => Value::Empty,
        }
    }

    fn encode(&mut self, v: &Value) -> (u8, u64) {
        match v {
            Value::Empty => (TAG_EMPTY, 0),
            Value::Number(n) => (TAG_NUMBER, n.to_bits()),
            Value::Text(s) => (TAG_TEXT, self.strings.intern(s) as u64),
            Value::Bool(b) => (TAG_BOOL, *b as u64),
            Value::Error(e) => (TAG_ERROR, *e as u64),
        }
    }

    /// Stores a value (keeping the cell's formula, style and author unless `meta` is given).
    pub(crate) fn set_value_by_id(&mut self, row: RowId, col: ColId, v: &Value, spilled: bool, meta: Option<Meta>) {
        let (tag, data) = self.encode(v);
        let column = self.column_mut(col);
        let (chunk, i) = column.chunk_mut(row, meta.unwrap_or(0));
        chunk.tags[i] = tag | if spilled { FLAG_SPILLED } else { 0 };
        chunk.data[i] = data;
        if let Some(m) = meta {
            if chunk.meta.is_none() && m != chunk.default_meta {
                chunk.meta = Some(vec![0; CHUNK]);
            }
            if let Some(mm) = chunk.meta.as_mut() {
                mm[i] = m;
            }
        }
    }

    pub(crate) fn set_formula_by_id(&mut self, row: RowId, col: ColId, group: Option<GroupId>) {
        let column = self.column_mut(col);
        let (chunk, i) = column.chunk_mut(row, 0);
        match group {
            Some(g) => chunk.formula.get_or_insert_with(|| vec![0; CHUNK])[i] = g + 1,
            None => {
                if let Some(f) = chunk.formula.as_mut() {
                    f[i] = 0;
                }
            }
        }
    }

    pub(crate) fn set_style_by_id(&mut self, row: RowId, col: ColId, style: u32) {
        let column = self.column_mut(col);
        let (chunk, i) = column.chunk_mut(row, 0);
        if style == 0 && chunk.style.is_none() {
            return;
        }
        chunk.style.get_or_insert_with(|| vec![0; CHUNK])[i] = style;
    }

    pub(crate) fn set_meta_by_id(&mut self, row: RowId, col: ColId, meta: Meta) {
        let column = self.column_mut(col);
        let (chunk, i) = column.chunk_mut(row, meta);
        if chunk.meta.is_none() && meta != chunk.default_meta {
            chunk.meta = Some(vec![0; CHUNK]);
        }
        if let Some(mm) = chunk.meta.as_mut() {
            mm[i] = meta;
        }
    }

    /// Clears a cell entirely (value, formula, style).
    pub(crate) fn clear_by_id(&mut self, row: RowId, col: ColId) {
        let Some(column) = self.columns.get_mut(col as usize).and_then(|c| c.as_mut()) else {
            return;
        };
        let ci = row as usize / CHUNK;
        if let Some(Some(chunk)) = column.chunks.get_mut(ci) {
            let i = row as usize % CHUNK;
            chunk.tags[i] = 0;
            chunk.data[i] = 0;
            if let Some(f) = chunk.formula.as_mut() {
                f[i] = 0;
            }
            if let Some(s) = chunk.style.as_mut() {
                s[i] = 0;
            }
            if let Some(m) = chunk.meta.as_mut() {
                m[i] = 0;
            }
            if chunk.is_blank() {
                column.chunks[ci] = None;
            }
        }
    }

    /// Drops every cell in the given rows (by id), after they were deleted from the axis.
    pub(crate) fn drop_rows(&mut self, rows: &[RowId]) {
        let cols: Vec<ColId> = (0..self.columns.len() as u32).collect();
        for &r in rows {
            for &c in &cols {
                self.clear_by_id(r, c);
            }
        }
    }

    pub(crate) fn drop_cols(&mut self, cols: &[ColId]) {
        for &c in cols {
            if let Some(slot) = self.columns.get_mut(c as usize) {
                *slot = None;
            }
        }
    }

    /// Every non-empty cell (value, formula or style) as `(row id, col id)`, column by column.
    pub fn occupied(&self) -> Vec<(RowId, ColId)> {
        let mut out = Vec::new();
        for (c, column) in self.columns.iter().enumerate() {
            let Some(column) = column else { continue };
            for (ci, chunk) in column.chunks.iter().enumerate() {
                let Some(chunk) = chunk else { continue };
                for i in 0..CHUNK {
                    let used = chunk.tags[i] != 0
                        || chunk.formula.as_ref().is_some_and(|f| f[i] != 0)
                        || chunk.style.as_ref().is_some_and(|s| s[i] != 0);
                    if used {
                        out.push(((ci * CHUNK + i) as RowId, c as ColId));
                    }
                }
            }
        }
        out
    }

    /// Visits every cell holding a value, formula or style, with its id pair.
    pub(crate) fn for_each_occupied(&self, mut f: impl FnMut(RowId, ColId, &Chunk, usize)) {
        for (c, column) in self.columns.iter().enumerate() {
            let Some(column) = column else { continue };
            for (ci, chunk) in column.chunks.iter().enumerate() {
                let Some(chunk) = chunk else { continue };
                for i in 0..CHUNK {
                    if chunk.tags[i] != 0
                        || chunk.formula.as_ref().is_some_and(|f| f[i] != 0)
                        || chunk.style.as_ref().is_some_and(|s| s[i] != 0)
                    {
                        f((ci * CHUNK + i) as RowId, c as ColId, chunk, i);
                    }
                }
            }
        }
    }

    /// The smallest rectangle (by position) holding every value and formula, or None when the tab is empty.
    pub fn used_rect(&self) -> Option<Rect> {
        let mut max_row: Option<u32> = None;
        let mut max_col: Option<u32> = None;
        for (c, column) in self.columns.iter().enumerate() {
            let Some(column) = column else { continue };
            let Some(cpos) = self.cols.pos_of(c as ColId) else { continue };
            let mut any = false;
            for (ci, chunk) in column.chunks.iter().enumerate() {
                let Some(chunk) = chunk else { continue };
                for i in 0..CHUNK {
                    if chunk.tags[i] != 0 || chunk.formula.as_ref().is_some_and(|f| f[i] != 0) {
                        let rid = (ci * CHUNK + i) as RowId;
                        if let Some(rp) = self.rows.pos_of(rid) {
                            any = true;
                            max_row = Some(max_row.map_or(rp, |m: u32| m.max(rp)));
                        }
                    }
                }
            }
            if any {
                max_col = Some(max_col.map_or(cpos, |m: u32| m.max(cpos)));
            }
        }
        Some(Rect::new(0, 0, max_row?, max_col?))
    }

    /// Bounds of what a reader may address.
    pub fn max_rows(&self) -> u32 {
        MAX_ROWS
    }

    pub fn max_cols(&self) -> u32 {
        MAX_COLS
    }

    /// Converts a position rectangle to ids, creating ids for positions that have none.
    pub fn id_rect(&mut self, r: &Rect) -> IdRect {
        IdRect {
            r1: self.rows.id_at_or_create(r.r1),
            c1: self.cols.id_at_or_create(r.c1),
            r2: self.rows.id_at_or_create(r.r2),
            c2: self.cols.id_at_or_create(r.c2),
        }
    }

    /// The position rectangle of an id rectangle, if its corners still exist.
    pub fn pos_rect(&self, r: &IdRect) -> Option<Rect> {
        Some(Rect::new(
            self.rows.pos_of(r.r1)?,
            self.cols.pos_of(r.c1)?,
            self.rows.pos_of(r.r2)?,
            self.cols.pos_of(r.c2)?,
        ))
    }
}

/// The accounts that changed cells, by uuid, so cells can store a small index.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Authors {
    list: Vec<String>,
    #[serde(skip)]
    index: HashMap<String, u32>,
}

impl Authors {
    pub fn rebuild(&mut self) {
        self.index = self.list.iter().enumerate().map(|(i, s)| (s.clone(), i as u32)).collect();
    }

    pub fn intern(&mut self, uuid: &str) -> u32 {
        if let Some(&i) = self.index.get(uuid) {
            return i;
        }
        let i = self.list.len() as u32;
        self.list.push(uuid.to_owned());
        self.index.insert(uuid.to_owned(), i);
        i
    }

    pub fn get(&self, i: u32) -> Option<&str> {
        self.list.get(i as usize).map(String::as_str)
    }
}

/// A defined name (`Revenue` → `Sales!$B$2:$B$100`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefinedName {
    pub name: String,
    /// The formula text it stands for, without `=`, as if written in A1 of its tab.
    pub text: String,
    pub tab: Option<TabId>,
}

/// Lets formula parsing find tabs in a workbook.
pub struct WorkbookNames<'a>(pub &'a [Tab]);

impl crate::formula::parse::TabNames for WorkbookNames<'_> {
    fn tab_id(&self, name: &str) -> Option<TabId> {
        self.0.iter().find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| t.id)
    }

    fn tab_name(&self, id: TabId) -> Option<String> {
        self.0.iter().find(|t| t.id == id).map(|t| t.name.clone())
    }
}

impl Default for Tab {
    fn default() -> Self {
        Self::new(0, "Tab 1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_insert_remove() {
        let mut a = Axis::default();
        a.ensure_len(5);
        assert_eq!(a.ids(), &[0, 1, 2, 3, 4]);
        let new = a.insert(2, 2);
        assert_eq!(new, vec![5, 6]);
        assert_eq!(a.ids(), &[0, 1, 5, 6, 2, 3, 4]);
        assert_eq!(a.pos_of(2), Some(4));
        let removed = a.remove(1, 2);
        assert_eq!(removed, vec![1, 5]);
        assert_eq!(a.ids(), &[0, 6, 2, 3, 4]);
        assert_eq!(a.pos_of(1), None);
        assert_eq!(a.pos_of(6), Some(1));
        a.insert_ids(1, &[1, 5]);
        assert_eq!(a.ids(), &[0, 1, 5, 6, 2, 3, 4]);
    }

    #[test]
    fn cells() {
        let mut t = Tab::new(1, "A");
        let r = t.rows.id_at_or_create(3);
        let c = t.cols.id_at_or_create(2);
        t.set_value_by_id(r, c, &Value::from("hi"), false, Some(pack_meta(0, TIME_EPOCH_UNIX + 5)));
        assert_eq!(t.value_at(3, 2), Value::from("hi"));
        assert_eq!(unpack_meta(t.cell_at(CellPos::new(3, 2)).meta), Some((0, TIME_EPOCH_UNIX + 5)));
        t.set_value_by_id(r, c, &Value::Number(2.5), false, None);
        assert_eq!(t.value_at(3, 2), Value::Number(2.5));
        assert_eq!(t.used_rect(), Some(Rect::new(0, 0, 3, 2)));
        t.clear_by_id(r, c);
        assert_eq!(t.value_at(3, 2), Value::Empty);
        assert_eq!(t.used_rect(), None);
    }
}
