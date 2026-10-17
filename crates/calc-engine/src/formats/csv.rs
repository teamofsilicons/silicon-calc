//! CSV and TSV.
//!
//! Reading accepts what spreadsheet apps and scripts really write: UTF-8 with or without a byte order mark,
//! UTF-16 (Excel's "Unicode text"), Windows-1252, any of `,` `;` tab `|` as the delimiter (detected from how
//! consistently each splits the first records, or given by Excel's `sep=;` first line), quoted fields with
//! delimiters and line breaks inside, and ragged rows. Values are read the way typing them would be (numbers,
//! dates, percentages, currency, logicals, errors), except that numbers which would lose something stay text:
//! leading zeros (`007`, ZIP codes) and integers longer than 15 digits (card numbers, ids).

use std::sync::Arc;

use ahash::{HashMap, HashSet};

use crate::{
    formats::{ExportOptions, FormatError, ImportOptions, Imported, export_tab, number_text, text::decode_text},
    input::{Entry, parse_entry},
    numfmt,
    ops::{ColumnContent, TabContent},
    style::Style,
    value::{ErrorKind, Value},
    workbook::Workbook,
};

const CANDIDATES: [u8; 4] = [b',', b'\t', b';', b'|'];

/// Picks the delimiter that splits the first records most consistently (and into the most fields).
fn sniff_delimiter(bytes: &[u8]) -> u8 {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    let truncated = sample.len() < bytes.len();
    let mut best: (u8, u32, usize) = (b',', 0, 0);
    for d in CANDIDATES {
        let mut reader = ::csv::ReaderBuilder::new().delimiter(d).has_headers(false).flexible(true).from_reader(sample);
        let mut counts: Vec<usize> = Vec::new();
        let mut record = ::csv::ByteRecord::new();
        while counts.len() < 64 {
            match reader.read_byte_record(&mut record) {
                Ok(true) => counts.push(record.len()),
                _ => break,
            }
        }
        if truncated && counts.len() > 1 {
            counts.pop();
        }
        let mut freq: HashMap<usize, usize> = HashMap::default();
        for &c in &counts {
            *freq.entry(c).or_default() += 1;
        }
        let Some((&mode, &n)) = freq.iter().max_by_key(|(c, n)| (**n, **c)) else { continue };
        if mode <= 1 {
            continue;
        }
        // Consistency in percent, then the number of fields.
        let consistency = (n * 100 / counts.len()) as u32;
        if (consistency, mode) > (best.1, best.2) {
            best = (d, consistency, mode);
        }
    }
    best.0
}

/// What a field holds, read as typing it would be read.
enum Parsed {
    Text,
    Number(f64, Option<String>),
    Bool(bool),
    Error(ErrorKind),
}

/// A plain decimal number (`12`, `-3.25`, `0.5`) read without the general parser, or None.
fn plain_number(b: &[u8]) -> Option<f64> {
    let digits = b.strip_prefix(b"-").unwrap_or(b);
    if digits.is_empty() || digits.len() > 40 {
        return None;
    }
    let mut seen_dot = false;
    for &c in digits {
        match c {
            b'0'..=b'9' => {}
            b'.' if !seen_dot => seen_dot = true,
            _ => return None,
        }
    }
    if digits[0] == b'.' || digits[digits.len() - 1] == b'.' {
        return None;
    }
    std::str::from_utf8(b).ok()?.parse().ok()
}

/// Numbers whose spelling would be lost: leading zeros (`007`) or integers of more than 15 digits.
fn keeps_as_text(s: &str) -> bool {
    let t = s.trim();
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let b = t.as_bytes();
    if b.len() >= 2 && b[0] == b'0' && b[1].is_ascii_digit() && b.iter().all(|c| c.is_ascii_digit() || *c == b'.') {
        return true;
    }
    b.len() > 15 && b.iter().all(u8::is_ascii_digit)
}

const MONTH_PREFIXES: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

fn parse_field(s: &str) -> Parsed {
    let b = s.as_bytes();
    if let Some(n) = plain_number(b) {
        if keeps_as_text(s) {
            return Parsed::Text;
        }
        return Parsed::Number(n, None);
    }
    let t = s.trim_start();
    let Some(&first) = t.as_bytes().first() else { return Parsed::Text };
    let worth_parsing = match first {
        b'0'..=b'9' | b'-' | b'+' | b'.' | b'$' | b'(' | b'#' => true,
        c if c.is_ascii_alphabetic() => {
            let l = t.len();
            (l == 4 || l == 5) && (t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false"))
                || l <= 40 && t.len() >= 3 && MONTH_PREFIXES.iter().any(|m| t[..3].eq_ignore_ascii_case(m))
        }
        _ => false,
    };
    if !worth_parsing || keeps_as_text(s) {
        return Parsed::Text;
    }
    match parse_entry(s) {
        Entry::Number(n, fmt) => Parsed::Number(n, fmt),
        Entry::Bool(v) => Parsed::Bool(v),
        Entry::Error(e) => Parsed::Error(e),
        // A field that looks like a formula stays text: files don't run code when opened.
        Entry::Formula(_) | Entry::Text(_) | Entry::Empty => Parsed::Text,
    }
}

/// What a field is, for telling a header row from data.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FieldKind {
    Empty,
    Text,
    /// A whole number that reads as a year (1000–2999), like the `2024` in `region,2024,2025`.
    Year,
    Date,
    Number,
    Other,
}

fn field_kind(f: &str) -> FieldKind {
    if f.trim().is_empty() {
        return FieldKind::Empty;
    }
    match parse_field(f) {
        Parsed::Text => FieldKind::Text,
        Parsed::Number(_, Some(fmt)) if numfmt::is_date_format(&fmt) => FieldKind::Date,
        Parsed::Number(n, None) if n.fract() == 0.0 && (1000.0..3000.0).contains(&n) => FieldKind::Year,
        Parsed::Number(..) => FieldKind::Number,
        Parsed::Bool(_) | Parsed::Error(_) => FieldKind::Other,
    }
}

/// Whether the first record is a header row. It is when some column has text there and typed values below
/// (`amount` over numbers), or when the row is complete, has some text, and each of its other fields is a year or
/// a date over a column that holds other kinds of values (`region,2024,2025` over counts).
fn detect_header(sample: &[Vec<String>]) -> bool {
    let Some((first, rest)) = sample.split_first() else { return false };
    if rest.is_empty() {
        return false;
    }
    let mut has_text = false;
    let mut all_header_like = true;
    for (c, head) in first.iter().enumerate() {
        let hk = field_kind(head);
        let below: Vec<FieldKind> =
            rest.iter().map(|row| row.get(c).map_or(FieldKind::Empty, |f| field_kind(f))).filter(|k| *k != FieldKind::Empty).collect();
        match hk {
            FieldKind::Empty => all_header_like = false,
            FieldKind::Text => {
                has_text = true;
                let typed = below.iter().filter(|k| **k != FieldKind::Text).count();
                if typed > 0 && typed >= below.len() - typed {
                    return true;
                }
            }
            FieldKind::Year | FieldKind::Date => {
                if below.is_empty() || below.contains(&hk) {
                    all_header_like = false;
                }
            }
            _ => all_header_like = false,
        }
    }
    has_text && all_header_like && first.len() > 1
}

/// Shares one allocation between equal texts (categories repeat a lot), up to a limit.
#[derive(Default)]
struct Interner {
    set: HashSet<Arc<str>>,
}

impl Interner {
    fn get(&mut self, s: &str) -> Value {
        if let Some(a) = self.set.get(s) {
            return Value::Text(a.clone());
        }
        let a: Arc<str> = Arc::from(s);
        if self.set.len() < 1_000_000 && s.len() <= 256 {
            self.set.insert(a.clone());
        }
        Value::Text(a)
    }
}

/// Reads CSV or TSV: values are read the way typing them would be (numbers, dates, percentages), and the format
/// their spelling implies is kept per cell. `delimiter` is the format's own (tab for TSV); `,` still sniffs.
pub fn import(bytes: &[u8], delimiter: Option<u8>, opts: &ImportOptions) -> Result<Imported, FormatError> {
    let format = if delimiter == Some(b'\t') { "TSV" } else { "CSV" };
    let (text, encoding) = decode_text(bytes);
    let mut body: &str = text.as_ref();
    let mut declared = None;
    // Excel's "sep=;" line names the delimiter.
    if body.len() >= 5 && body[..4].eq_ignore_ascii_case("sep=") {
        let line_end = body.find('\n').unwrap_or(body.len());
        let line = body[4..line_end].trim_end_matches('\r');
        if line.len() == 1 && line.is_ascii() {
            declared = Some(line.as_bytes()[0]);
            body = &body[(line_end + 1).min(body.len())..];
        }
    }
    let delimiter = match (opts.delimiter, declared, delimiter) {
        (Some(d), _, _) => d,
        (None, Some(d), _) => d,
        (None, None, Some(b'\t')) => b'\t',
        _ => sniff_delimiter(body.as_bytes()),
    };
    let builder = || {
        let mut b = ::csv::ReaderBuilder::new();
        b.delimiter(delimiter).has_headers(false).flexible(true);
        b
    };
    let header = match opts.header {
        Some(h) => h,
        None if opts.raw_text => false,
        None => {
            let mut sample = Vec::new();
            let mut reader = builder().from_reader(body.as_bytes());
            for record in reader.records().take(21) {
                match record {
                    Ok(r) => sample.push(r.iter().map(str::to_owned).collect::<Vec<_>>()),
                    Err(_) => break,
                }
            }
            detect_header(&sample)
        }
    };

    let mut reader = builder().from_reader(body.as_bytes());
    let mut columns: Vec<ColumnContent> = Vec::new();
    let mut styles: Vec<Style> = Vec::new();
    let mut style_of_fmt: HashMap<String, u32> = HashMap::default();
    let mut interner = Interner::default();
    let mut rows = 0u32;
    let mut record = ::csv::StringRecord::new();
    loop {
        match reader.read_record(&mut record) {
            Ok(false) => break,
            Ok(true) => {}
            Err(e) => {
                let line = e.position().map(|p| format!(" (line {})", p.line())).unwrap_or_default();
                return Err(FormatError::read(format, format!("{e}{line}")));
            }
        }
        if rows >= crate::addr::MAX_ROWS {
            return Err(FormatError::read(format, format!("it has more than {} rows", crate::addr::MAX_ROWS)));
        }
        if record.len() > columns.len() {
            if record.len() > crate::addr::MAX_COLS as usize {
                return Err(FormatError::read(format, "it has more than 16,384 columns"));
            }
            columns.resize_with(record.len(), ColumnContent::default);
        }
        let as_text = opts.raw_text || (rows == 0 && header);
        for (c, field) in record.iter().enumerate() {
            if field.is_empty() {
                continue;
            }
            let value = if as_text {
                interner.get(field)
            } else {
                match parse_field(field) {
                    Parsed::Number(n, fmt) => {
                        if let Some(f) = fmt {
                            let next = styles.len() as u32;
                            let ix = *style_of_fmt.entry(f.clone()).or_insert_with(|| {
                                styles.push(Style { num_fmt: Some(f), ..Default::default() });
                                next
                            });
                            columns[c].styles.push((rows, ix));
                        }
                        Value::Number(n)
                    }
                    Parsed::Bool(b) => Value::Bool(b),
                    Parsed::Error(e) => Value::Error(e),
                    Parsed::Text => interner.get(field),
                }
            };
            let col = &mut columns[c];
            if (col.values.len() as u32) < rows {
                col.values.resize(rows as usize, Value::Empty);
            }
            col.values.push(value);
        }
        rows += 1;
    }
    let cols = columns.len() as u32;
    let content = TabContent { rows, cols, columns, styles, ..Default::default() };
    let mut warnings = Vec::new();
    if encoding != "UTF-8" {
        warnings.push(format!("The file was read as {encoding} text."));
    }
    Ok(Imported { tabs: vec![("Tab 1".to_owned(), content)], warnings, ..Default::default() })
}

/// The text a cell is written as in CSV, TSV and JSON: dates, percentages and currency as they show, other
/// numbers in full (15 significant digits), formulas as their values.
pub(crate) fn shown_for_data(value: &Value, num_fmt: Option<&str>) -> String {
    match value {
        Value::Number(n) => match num_fmt {
            Some(f) if numfmt::is_date_format(f) || f.contains('%') || f.contains('$') => numfmt::format_value(value, Some(f)).0,
            _ => number_text(*n),
        },
        v => v.display(),
    }
}

/// Writes one tab as CSV/TSV: what each cell shows (formatted numbers keep their format: "12%"),
/// formulas as their values.
pub fn export(wb: &Workbook, opts: &ExportOptions, delimiter: u8) -> Result<Vec<u8>, FormatError> {
    let format = if delimiter == b'\t' { "TSV" } else { "CSV" };
    let tab_id = export_tab(wb, opts).ok_or(FormatError::write(format, "it has no tabs"))?;
    let tab = wb.tab(tab_id).ok_or(FormatError::write(format, "that tab doesn't exist"))?;
    let mut w = ::csv::WriterBuilder::new().delimiter(delimiter).flexible(true).from_writer(Vec::new());
    if let Some(used) = tab.used_rect() {
        let mut line: Vec<String> = Vec::with_capacity(used.cols() as usize);
        for r in 0..=used.r2 {
            line.clear();
            for c in 0..=used.c2 {
                let cell = tab.cell_at(crate::addr::CellPos::new(r, c));
                line.push(shown_for_data(&cell.value, wb.style_of(cell.style).num_fmt.as_deref()));
            }
            if line.len() == 1 && line[0].is_empty() {
                // An empty line would be skipped when read back; an empty quoted field keeps the row.
                w.write_record([""].iter()).map_err(|e| FormatError::write(format, e.to_string()))?;
                continue;
            }
            w.write_record(&line).map_err(|e| FormatError::write(format, e.to_string()))?;
        }
    }
    w.into_inner().map_err(|e| FormatError::write(format, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Actor, Op};

    fn wb_of(content: &TabContent) -> Workbook {
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        wb.apply(&Op::ReplaceTab { tab: t, content: Box::new(content.clone()) }, &Actor { account: "a", at: 0 }).expect("apply");
        wb
    }

    #[test]
    fn round_trip() {
        let csv = "name,amount,share,when\nada,1200,50%,2026-10-17\n\"b, c\",3.5,,\n";
        let imported = import(csv.as_bytes(), Some(b','), &ImportOptions::default()).expect("import");
        let (_, content) = &imported.tabs[0];
        assert_eq!(content.rows, 3);
        assert_eq!(content.columns[1].values[1], Value::Number(1200.0));
        assert_eq!(content.columns[2].values[1], Value::Number(0.5));
        let wb = wb_of(content);
        let out = String::from_utf8(export(&wb, &ExportOptions::default(), b',').expect("export")).expect("utf8");
        assert_eq!(out, "name,amount,share,when\nada,1200,50%,2026-10-17\n\"b, c\",3.5,,\n");
    }

    #[test]
    fn sniffs_semicolons() {
        let imported = import(b"a;b\n1;2\n", None, &ImportOptions::default()).expect("import");
        assert_eq!(imported.tabs[0].1.cols, 2);
        // Decimal commas don't fool it.
        let imported = import(b"x;y\n1,5;2,5\n3,25;4\n", None, &ImportOptions::default()).expect("import");
        assert_eq!(imported.tabs[0].1.cols, 2);
        assert_eq!(imported.tabs[0].1.columns[0].values[1], Value::from("1,5"));
    }

    #[test]
    fn sep_line_and_quoted_newlines() {
        let imported = import(b"sep=|\na|b\n\"line one\nline two\"|2\n", None, &ImportOptions::default()).expect("import");
        let c = &imported.tabs[0].1;
        assert_eq!(c.rows, 2);
        assert_eq!(c.columns[0].values[1], Value::from("line one\nline two"));
        assert_eq!(c.columns[1].values[1], Value::Number(2.0));
    }

    #[test]
    fn keeps_spellings_that_numbers_would_lose() {
        let imported = import(b"zip,card,n\n02139,4111111111111111,7\n", None, &ImportOptions::default()).expect("import");
        let c = &imported.tabs[0].1;
        assert_eq!(c.columns[0].values[1], Value::from("02139"));
        assert_eq!(c.columns[1].values[1], Value::from("4111111111111111"));
        assert_eq!(c.columns[2].values[1], Value::Number(7.0));
    }

    #[test]
    fn header_detection_keeps_year_headers_as_text() {
        let imported = import(b"region,2024,2025\neu,1,2\nus,3,4\n", None, &ImportOptions::default()).expect("import");
        assert_eq!(imported.tabs[0].1.columns[1].values[0], Value::from("2024"));
        let imported = import(b"1,2\n3,4\n", None, &ImportOptions::default()).expect("import");
        assert_eq!(imported.tabs[0].1.columns[0].values[0], Value::Number(1.0));
    }

    #[test]
    fn errors_and_logicals() {
        let imported = import(b"a,b\n#N/A,TRUE\n", None, &ImportOptions::default()).expect("import");
        let c = &imported.tabs[0].1;
        assert_eq!(c.columns[0].values[1], Value::Error(ErrorKind::NA));
        assert_eq!(c.columns[1].values[1], Value::Bool(true));
    }
}
