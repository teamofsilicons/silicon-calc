//! CSV and TSV.

use crate::{
    formats::{ExportOptions, FormatError, ImportOptions, Imported, export_tab},
    input::{Entry, parse_entry},
    numfmt,
    ops::{ColumnContent, TabContent},
    style::Style,
    value::Value,
    workbook::Workbook,
};

fn sniff_delimiter(bytes: &[u8]) -> u8 {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(16_384)]);
    let lines: Vec<&str> = head.lines().take(20).collect();
    let mut best = (b',', 0usize);
    for d in [b',', b'\t', b';', b'|'] {
        let counts: Vec<usize> = lines.iter().map(|l| l.bytes().filter(|&b| b == d).count()).collect();
        let consistent = counts.iter().filter(|&&c| c > 0 && c == counts[0]).count();
        if counts.first().copied().unwrap_or(0) > 0 && consistent > best.1 {
            best = (d, consistent);
        }
    }
    best.0
}

/// Reads CSV or TSV: values are read the way typing them would be (numbers, dates, percentages), and the format
/// their spelling implies is kept per column.
pub fn import(bytes: &[u8], delimiter: Option<u8>, opts: &ImportOptions) -> Result<Imported, FormatError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()).unwrap_or(bytes);
    let delimiter = match (opts.delimiter, delimiter) {
        (Some(d), _) => d,
        (None, Some(b'\t')) => b'\t',
        _ => sniff_delimiter(bytes),
    };
    let mut reader = ::csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut columns: Vec<ColumnContent> = Vec::new();
    let mut styles: Vec<Style> = Vec::new();
    let mut style_of_fmt: ahash::HashMap<String, u32> = Default::default();
    let mut rows = 0u32;
    let mut record = ::csv::ByteRecord::new();
    loop {
        match reader.read_byte_record(&mut record) {
            Ok(false) => break,
            Ok(true) => {}
            Err(e) => return Err(FormatError::Read { format: "CSV", message: e.to_string() }),
        }
        if rows >= crate::addr::MAX_ROWS {
            return Err(FormatError::Read { format: "CSV", message: format!("it has more than {} rows", crate::addr::MAX_ROWS) });
        }
        if record.len() > columns.len() {
            if record.len() > crate::addr::MAX_COLS as usize {
                return Err(FormatError::Read { format: "CSV", message: "it has more than 16,384 columns".into() });
            }
            columns.resize_with(record.len(), ColumnContent::default);
        }
        for (c, field) in record.iter().enumerate() {
            let text = String::from_utf8_lossy(field);
            let col = &mut columns[c];
            if text.is_empty() {
                continue;
            }
            let value = if opts.raw_text || (rows == 0 && opts.header == Some(true)) {
                Value::from(text.as_ref())
            } else {
                match parse_entry(&text) {
                    Entry::Number(n, fmt) => {
                        if let Some(f) = fmt {
                            let next = styles.len() as u32;
                            let ix = *style_of_fmt.entry(f.clone()).or_insert_with(|| {
                                styles.push(Style { num_fmt: Some(f), ..Default::default() });
                                next
                            });
                            col.styles.push((rows, ix));
                        }
                        Value::Number(n)
                    }
                    Entry::Bool(b) => Value::Bool(b),
                    // A CSV field that looks like a formula stays text: files don't run code when opened.
                    Entry::Formula(_) | Entry::Text(_) | Entry::Error(_) | Entry::Empty => Value::from(text.as_ref()),
                }
            };
            while (col.values.len() as u32) < rows {
                col.values.push(Value::Empty);
            }
            col.values.push(value);
        }
        rows += 1;
    }
    let cols = columns.len() as u32;
    let content = TabContent { rows, cols, columns, styles, ..Default::default() };
    Ok(Imported { tabs: vec![("Tab 1".to_owned(), content)], ..Default::default() })
}

/// Writes one tab as CSV/TSV: what each cell shows (formatted numbers keep their format: "12%"),
/// formulas as their values.
pub fn export(wb: &Workbook, opts: &ExportOptions, delimiter: u8) -> Result<Vec<u8>, FormatError> {
    let tab_id = export_tab(wb, opts).ok_or(FormatError::Write { format: "CSV", message: "it has no tabs".into() })?;
    let tab = wb.tab(tab_id).ok_or(FormatError::Write { format: "CSV", message: "that tab doesn't exist".into() })?;
    let mut w = ::csv::WriterBuilder::new().delimiter(delimiter).from_writer(Vec::new());
    if let Some(used) = tab.used_rect() {
        let mut line: Vec<String> = Vec::with_capacity(used.cols() as usize);
        for r in 0..=used.r2 {
            line.clear();
            for c in 0..=used.c2 {
                let cell = tab.cell_at(crate::addr::CellPos::new(r, c));
                let style = wb.style_of(cell.style);
                let shown = match &cell.value {
                    Value::Number(_) if style.num_fmt.is_some() && !numfmt::is_date_format(style.num_fmt.as_deref().unwrap_or("")) => {
                        // Plain numbers stay machine-readable unless the format is a percentage or currency.
                        let f = style.num_fmt.as_deref().unwrap_or("");
                        if f.contains('%') || f.contains('$') {
                            numfmt::format_value(&cell.value, Some(f)).0
                        } else {
                            cell.value.display()
                        }
                    }
                    Value::Number(_) if style.num_fmt.is_some() => numfmt::format_value(&cell.value, style.num_fmt.as_deref()).0,
                    v => v.display(),
                };
                line.push(shown);
            }
            w.write_record(&line).map_err(|e| FormatError::Write { format: "CSV", message: e.to_string() })?;
        }
    }
    w.into_inner().map_err(|e| FormatError::Write { format: "CSV", message: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{Actor, Op};

    #[test]
    fn round_trip() {
        let csv = "name,amount,share,when\nada,1200,50%,2026-10-17\n\"b, c\",3.5,,\n";
        let imported = import(csv.as_bytes(), Some(b','), &ImportOptions::default()).expect("import");
        let (_, content) = &imported.tabs[0];
        assert_eq!(content.rows, 3);
        assert_eq!(content.columns[1].values[1], Value::Number(1200.0));
        assert_eq!(content.columns[2].values[1], Value::Number(0.5));
        let mut wb = Workbook::new();
        let t = wb.tabs[0].id;
        wb.apply(&Op::ReplaceTab { tab: t, content: Box::new(content.clone()) }, &Actor { account: "a", at: 0 }).expect("apply");
        let out = String::from_utf8(export(&wb, &ExportOptions::default(), b',').expect("export")).expect("utf8");
        assert_eq!(out, "name,amount,share,when\nada,1200,50%,2026-10-17\n\"b, c\",3.5,,\n");
    }

    #[test]
    fn sniffs_semicolons() {
        let imported = import(b"a;b\n1;2\n", None, &ImportOptions::default()).expect("import");
        assert_eq!(imported.tabs[0].1.cols, 2);
    }
}
