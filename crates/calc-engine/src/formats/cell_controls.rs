//! Preserve Calc's custom input values and clipping in XLSX, alongside native
//! Excel checkboxes and validation lists. Other readers can ignore this XML part.
use std::io::{Cursor, Write};
use serde::{Deserialize, Serialize};
use crate::{ops::TabContent, style::CellControl};
use super::{FormatError, xml::{push_text, reader}};

pub const PART: &str = "xl/silicon-calc-controls.xml";

#[derive(Serialize, Deserialize)]
pub struct CellExtra { pub row: u32, pub col: u32, pub clip: bool, pub control: Option<CellControl> }
pub type Extras = Vec<(String, Vec<CellExtra>)>;

pub fn append(bytes: Vec<u8>, extras: &Extras) -> Result<Vec<u8>, FormatError> {
    if extras.is_empty() { return Ok(bytes); }
    let err = |e: String| FormatError::write("Excel", e);
    let json = serde_json::to_string(extras).map_err(|e| err(e.to_string()))?;
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><calc-controls version=\"1\">{}</calc-controls>", json.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"));
    let mut zip = zip::ZipWriter::new_append(Cursor::new(bytes)).map_err(|e| err(e.to_string()))?;
    zip.start_file(PART, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).map_err(|e| err(e.to_string()))?;
    zip.write_all(xml.as_bytes()).map_err(|e| err(e.to_string()))?;
    Ok(zip.finish().map_err(|e| err(e.to_string()))?.into_inner())
}

pub fn restore(xml: &[u8], tabs: &mut [(String, TabContent)]) {
    if xml.len() > 32 * 1024 * 1024 { return; }
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut json = String::new();
    loop {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            Ok(event) => push_text(&mut json, &event),
        }
    }
    let Ok(extras) = serde_json::from_str::<Extras>(&json) else { return };
    let mut budget = 1_000_000usize;
    for (name, cells) in extras {
        let Some((_, tab)) = tabs.iter_mut().find(|(n, _)| *n == name) else { continue };
        for extra in cells {
            if budget == 0 { return; }
            budget -= 1;
            if extra.row >= 1_048_576 || extra.col >= 16_384 || extra.control.as_ref().is_some_and(|c| c.validate().is_err()) { continue; }
            // Excel omits blank cells with no native formatting. Calc controls on
            // those cells still need to survive a download/import round trip.
            tab.rows = tab.rows.max(extra.row + 1);
            tab.cols = tab.cols.max(extra.col + 1);
            tab.columns.resize_with(tab.cols as usize, Default::default);
            let Some(col) = tab.columns.get_mut(extra.col as usize) else { continue };
            let at = col.styles.binary_search_by_key(&extra.row, |(r, _)| *r);
            let mut style = at.ok().and_then(|i| tab.styles.get(col.styles[i].1 as usize)).cloned().unwrap_or_default();
            style.control = extra.control;
            style.clip = extra.clip;
            let id = tab.styles.iter().position(|s| *s == style).unwrap_or_else(|| { tab.styles.push(style); tab.styles.len() - 1 }) as u32;
            match at { Ok(i) => col.styles[i].1 = id, Err(i) => col.styles.insert(i, (extra.row, id)) }
        }
    }
}
