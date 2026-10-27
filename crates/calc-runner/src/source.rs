//! Workbook reads used by trusted file conversions.

use std::sync::Arc;

use calc_engine::{
    CellPos, Rect, TabId, Value, Workbook,
    addr::{MAX_COLS, MAX_ROWS, split_tab},
};

use crate::table::{self, Table};

/// A cell read for conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct CellRead {
    pub tab: TabId,
    pub value: Value,
    /// The cell's number format, when it has one (used for dates during conversion).
    pub format: Option<String>,
}

/// A workbook read interface for conversions.
pub trait SheetSource: Send + Sync {
    /// The tab names, in order.
    fn tab_names(&self) -> Vec<String>;

    /// The table `name` stands for: a tab (any case), a range of one (`Sales!A1:C9`, `'Q3 plan'!B:D`) or a
    /// defined name; None when nothing in the sheet has that name.
    fn table(&self, name: &str) -> Option<Arc<Table>>;

    /// One cell: `Sales!B2`, `'Q3 plan'!B2`, `B2` (on the first tab) or a defined name (its first cell).
    fn cell(&self, reference: &str) -> Option<CellRead>;
}

impl SheetSource for Workbook {
    fn tab_names(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.name.clone()).collect()
    }

    fn table(&self, name: &str) -> Option<Arc<Table>> {
        let source = table::resolve(self, name)?;
        Some(Arc::new(table::build(self, name, &source)))
    }

    fn cell(&self, reference: &str) -> Option<CellRead> {
        read_cell(self, reference)
    }
}

impl<T: SheetSource + ?Sized> SheetSource for Arc<T> {
    fn tab_names(&self) -> Vec<String> {
        (**self).tab_names()
    }

    fn table(&self, name: &str) -> Option<Arc<Table>> {
        (**self).table(name)
    }

    fn cell(&self, reference: &str) -> Option<CellRead> {
        (**self).cell(reference)
    }
}

/// The cell a reference names in `wb`.
pub fn read_cell(wb: &Workbook, reference: &str) -> Option<CellRead> {
    let r = reference.trim();
    let (tab, pos) = match split_tab(r) {
        (Some(name), rest) => (wb.tab_by_name(&name)?.id, top_left(rest)?),
        (None, rest) => match wb.names.iter().find(|n| n.name.eq_ignore_ascii_case(rest)) {
            // A defined name wins over a column that happens to be spelled the same ("Tax").
            Some(def) => {
                let home = def.tab.or_else(|| wb.tabs.first().map(|t| t.id))?;
                let area = wb.resolve_area(def.text.trim_start_matches('='), home)?;
                (area.tab, CellPos::new(area.rect.r1, area.rect.c1))
            }
            None => (wb.tabs.first()?.id, top_left(rest)?),
        },
    };
    let t = wb.tab(tab)?;
    let cell = t.cell_at(pos);
    Some(CellRead {
        tab,
        value: cell.value,
        format: wb.style_of(cell.style).num_fmt.clone(),
    })
}

/// The top-left cell of `B2` or `B2:C9`.
fn top_left(text: &str) -> Option<CellPos> {
    let rect = Rect::parse(text, MAX_ROWS, MAX_COLS)?;
    Some(CellPos::new(rect.r1, rect.c1))
}
