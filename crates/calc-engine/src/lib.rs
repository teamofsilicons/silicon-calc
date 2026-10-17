//! Silicon Calc's spreadsheet engine.
//!
//! Everything here is pure computation with no I/O: the workbook and its cells, Excel-compatible formulas and
//! recalculation, the operations history is made of (and their inverses), snapshots, comparing and merging
//! workbooks, and reading and writing every supported file format.

pub mod addr;
pub mod dates;
pub mod diff;
pub mod formats;
pub mod formula;
pub mod input;
pub mod model;
pub mod numfmt;
pub mod ops;
pub mod snapshot;
pub mod style;
pub mod value;
pub mod workbook;

pub use addr::{CellPos, Rect};
pub use formula::ast::TabId;
pub use model::{Cell, ColId, RowId, Tab};
pub use ops::{Applied, Op, OpError};
pub use style::{Style, StylePatch};
pub use value::{ErrorKind, Value};
pub use workbook::Workbook;
