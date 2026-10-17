//! OpenDocument spreadsheets. Reading and writing are being built (the formula translation is in formula.rs);
//! until then both answer Unsupported.

#[allow(dead_code)]
pub(crate) mod formula;

use crate::{
    formats::{ExportOptions, FormatError, ImportOptions, Imported},
    workbook::Workbook,
};

pub(crate) fn import(_bytes: &[u8], _opts: &ImportOptions) -> Result<Imported, FormatError> {
    Err(FormatError::Unsupported("Opening OpenDocument"))
}

pub(crate) fn export(_wb: &Workbook, _opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    Err(FormatError::Unsupported("OpenDocument"))
}
