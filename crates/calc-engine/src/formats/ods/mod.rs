//! OpenDocument translation helpers retained for format conformance tests. Complete document conversions
//! run in calc-runner through LibreOffice, preserving styles, formulas and chart objects.

#[allow(dead_code)]
pub(crate) mod formula;
#[cfg(feature = "formats-wip")]
#[allow(dead_code)]
pub(crate) mod datastyle;
