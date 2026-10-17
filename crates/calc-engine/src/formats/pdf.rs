use crate::{formats::{ExportOptions, FormatError}, workbook::Workbook};
pub(crate) fn export(_w: &Workbook, _o: &ExportOptions) -> Result<Vec<u8>, FormatError> { Err(FormatError::Unsupported("PDF")) }
