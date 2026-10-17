//! Reading and writing files. Every format a sheet can be opened from or downloaded as goes through here, except
//! Parquet and Apple Numbers, which the service converts with DuckDB and the code runner (they arrive here as
//! rows of values).

pub mod csv;

use serde::{Deserialize, Serialize};

use crate::{
    formula::ast::TabId,
    model::DefinedName,
    ops::TabContent,
    workbook::Workbook,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Csv,
    Tsv,
    Xlsx,
    Xls,
    Ods,
    Numbers,
    Json,
    Parquet,
    Pdf,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
            Self::Ods => "ods",
            Self::Numbers => "numbers",
            Self::Json => "json",
            Self::Parquet => "parquet",
            Self::Pdf => "pdf",
        }
    }

    pub fn media_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv; charset=utf-8",
            Self::Tsv => "text/tab-separated-values; charset=utf-8",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Numbers => "application/vnd.apple.numbers",
            Self::Json => "application/json",
            Self::Parquet => "application/vnd.apache.parquet",
            Self::Pdf => "application/pdf",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "csv" => Self::Csv,
            "tsv" | "tab" => Self::Tsv,
            "xlsx" | "xlsm" => Self::Xlsx,
            "xls" => Self::Xls,
            "ods" => Self::Ods,
            "numbers" => Self::Numbers,
            "json" => Self::Json,
            "parquet" => Self::Parquet,
            "pdf" => Self::Pdf,
            _ => return None,
        })
    }

    /// From a file name, falling back to the first bytes.
    pub fn detect(filename: &str, bytes: &[u8]) -> Option<Self> {
        if let Some(f) = filename.rsplit_once('.').and_then(|(_, e)| Self::from_extension(e)) {
            return Some(f);
        }
        if bytes.starts_with(b"PAR1") {
            return Some(Self::Parquet);
        }
        if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            return Some(Self::Xls);
        }
        if bytes.starts_with(b"PK") {
            return Some(Self::Xlsx);
        }
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
        let t = head.trim_start();
        if t.starts_with('[') || t.starts_with('{') {
            return Some(Self::Json);
        }
        if head.contains('\t') && !head.contains(',') {
            return Some(Self::Tsv);
        }
        Some(Self::Csv)
    }

    /// Formats a whole sheet can be downloaded as (the rest download one tab).
    pub fn holds_many_tabs(self) -> bool {
        matches!(self, Self::Xlsx | Self::Xls | Self::Ods | Self::Numbers | Self::Json | Self::Pdf)
    }
}

/// What reading a file gave.
#[derive(Clone, Debug, Default)]
pub struct Imported {
    /// Tabs in order, with their names.
    pub tabs: Vec<(String, TabContent)>,
    pub names: Vec<DefinedName>,
    /// Things the file had that could not be kept, in words for the person who opened it.
    pub warnings: Vec<String>,
}

/// Options for reading.
#[derive(Clone, Debug, Default)]
pub struct ImportOptions {
    /// CSV: the first row is a header row (always kept as text). Detected when None.
    pub header: Option<bool>,
    /// CSV: the delimiter; detected when None.
    pub delimiter: Option<u8>,
    /// Keep every value as typed text (no numbers or dates).
    pub raw_text: bool,
}

/// Options for writing.
#[derive(Clone, Debug, Default)]
pub struct ExportOptions {
    /// One tab only (formats that hold one tab use the first when None).
    pub tab: Option<TabId>,
    /// Write values instead of formulas.
    pub values_only: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("This file can't be read as {format}: {message}")]
    Read { format: &'static str, message: String },
    #[error("The sheet can't be written as {format}: {message}")]
    Write { format: &'static str, message: String },
    #[error("{0} files are converted by the service, not the engine.")]
    External(&'static str),
    #[error("{0} is not supported yet.")]
    Unsupported(&'static str),
}

/// Reads a file.
pub fn import(format: Format, bytes: &[u8], opts: &ImportOptions) -> Result<Imported, FormatError> {
    match format {
        Format::Csv => csv::import(bytes, opts.delimiter.or(Some(b',')), opts),
        Format::Tsv => csv::import(bytes, Some(b'\t'), opts),
        Format::Parquet => Err(FormatError::External("Parquet")),
        Format::Numbers => Err(FormatError::External("Numbers")),
        Format::Xlsx => Err(FormatError::Unsupported("Excel")),
        Format::Xls => Err(FormatError::Unsupported("Excel 97-2003")),
        Format::Ods => Err(FormatError::Unsupported("OpenDocument")),
        Format::Json => Err(FormatError::Unsupported("JSON")),
        Format::Pdf => Err(FormatError::Unsupported("Opening PDF")),
    }
}

/// Writes a sheet (or one tab of it).
pub fn export(wb: &Workbook, format: Format, opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    match format {
        Format::Csv => csv::export(wb, opts, b','),
        Format::Tsv => csv::export(wb, opts, b'\t'),
        Format::Parquet => Err(FormatError::External("Parquet")),
        Format::Numbers => Err(FormatError::External("Numbers")),
        Format::Xlsx => Err(FormatError::Unsupported("Excel")),
        Format::Xls => Err(FormatError::Unsupported("Excel 97-2003")),
        Format::Ods => Err(FormatError::Unsupported("OpenDocument")),
        Format::Json => Err(FormatError::Unsupported("JSON")),
        Format::Pdf => Err(FormatError::Unsupported("PDF")),
    }
}

/// The tab an export of one tab uses.
pub(crate) fn export_tab(wb: &Workbook, opts: &ExportOptions) -> Option<TabId> {
    opts.tab.or_else(|| wb.tabs.first().map(|t| t.id))
}
