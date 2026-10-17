//! Reading Excel workbooks (`.xlsx`, `.xlsm`): our own reader over the zip package and its XML parts, so styles,
//! formulas (shared, array and dynamic), sizes, merges, frozen panes, tab colours, defined names, charts and notes
//! all survive. Worksheets are streamed straight from the zip, so a 1,048,576-row sheet never sits in memory as
//! XML.

mod comments;
mod drawing;
mod formula;
mod sheet;
mod styles;

use std::{
    io::{BufRead, BufReader, Cursor, Read},
    sync::Arc,
};

use ahash::HashMap;
use quick_xml::events::Event;
use zip::ZipArchive;

use crate::{
    formats::{
        FormatError, ImportOptions, Imported, IndexNames, Note, TabInfo, Warnings,
        colors::Palette,
        plural,
        xml::{attr, local, name_of, read_text, reader, skip_element},
    },
    model::DefinedName,
};

pub(crate) use formula::TableDef;

const FORMAT: &str = "Excel";
/// The most a single part may expand to (a guard against zip bombs).
const PART_LIMIT: u64 = 4 << 30;

/// The zip package, with case-insensitive part lookup (some producers differ in case or use backslashes).
pub(crate) struct Package<'a> {
    zip: ZipArchive<Cursor<&'a [u8]>>,
    index: HashMap<String, String>,
}

/// One relationship of a part.
#[derive(Clone, Debug)]
pub(crate) struct Rel {
    pub id: String,
    /// The last segment of the relationship type (`worksheet`, `sharedStrings`, `chart`, …).
    pub kind: String,
    /// The target part's path inside the package (resolved), or the external target.
    pub target: String,
    pub external: bool,
}

impl<'a> Package<'a> {
    pub fn new(bytes: &'a [u8], format: &'static str) -> Result<Self, FormatError> {
        let zip = ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| FormatError::read(format, format!("it is not a valid zip package ({e})")))?;
        let index = zip.file_names().map(|n| (normalize(n).to_ascii_lowercase(), n.to_owned())).collect();
        Ok(Self { zip, index })
    }

    fn actual(&self, path: &str) -> Option<String> {
        self.index.get(&normalize(path).to_ascii_lowercase()).cloned()
    }

    pub fn has(&self, path: &str) -> bool {
        self.actual(path).is_some()
    }

    /// The whole part.
    pub fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        let name = self.actual(path)?;
        let file = self.zip.by_name(&name).ok()?;
        let mut out = Vec::with_capacity(file.size().min(64 << 20) as usize);
        file.take(PART_LIMIT).read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// Streams a part through `f`.
    pub fn stream<T>(&mut self, path: &str, f: impl FnOnce(&mut dyn BufRead) -> T) -> Option<T> {
        let name = self.actual(path)?;
        let file = self.zip.by_name(&name).ok()?;
        let mut r = BufReader::with_capacity(256 * 1024, file.take(PART_LIMIT));
        Some(f(&mut r))
    }

    /// The relationships of a part (`xl/workbook.xml` → `xl/_rels/workbook.xml.rels`).
    pub fn rels(&mut self, part: &str) -> Vec<Rel> {
        let (dir, file) = match part.rsplit_once('/') {
            Some((d, f)) => (d.to_owned(), f.to_owned()),
            None => (String::new(), part.to_owned()),
        };
        let rels_path = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
        let Some(bytes) = self.read(&rels_path) else { return Vec::new() };
        let mut out = Vec::new();
        let mut r = reader(bytes.as_slice());
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if name_of(&e) == b"Relationship" => {
                    let (Some(id), Some(kind), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Type"), attr(&e, b"Target")) else {
                        continue;
                    };
                    let external = attr(&e, b"TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
                    let kind = kind.rsplit('/').next().unwrap_or("").to_owned();
                    let target = if external { target } else { resolve(&dir, &target) };
                    out.push(Rel { id, kind, target, external });
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
        out
    }
}

fn normalize(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches('/').to_owned()
}

/// A relationship target resolved against the directory of the part it belongs to.
pub(crate) fn resolve(dir: &str, target: &str) -> String {
    let target = target.replace('\\', "/");
    let mut parts: Vec<&str> = if target.starts_with('/') { Vec::new() } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// A sheet listed in the workbook.
struct SheetEntry {
    name: String,
    rid: String,
    hidden: bool,
}

struct WorkbookInfo {
    sheets: Vec<SheetEntry>,
    /// `(name, local sheet index, formula text, hidden)`.
    names: Vec<(String, Option<u32>, String, bool)>,
    date1904: bool,
}

fn read_workbook(xml: &[u8]) -> WorkbookInfo {
    let mut info = WorkbookInfo { sheets: Vec::new(), names: Vec::new(), date1904: false };
    let mut r = reader(xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match name_of(e) {
                    b"workbookPr" => info.date1904 = attr(e, b"date1904").is_some_and(|v| crate::formats::xml::truthy(&v)),
                    b"sheet" => {
                        let name = attr(e, b"name").unwrap_or_default();
                        let rid = attr(e, b"id").unwrap_or_default();
                        let hidden = attr(e, b"state").is_some_and(|s| s != "visible");
                        info.sheets.push(SheetEntry { name, rid, hidden });
                    }
                    b"definedName" if !empty => {
                        let name = attr(e, b"name").unwrap_or_default();
                        let local_sheet = attr(e, b"localSheetId").and_then(|v| v.trim().parse().ok());
                        let hidden = attr(e, b"hidden").is_some_and(|v| crate::formats::xml::truthy(&v));
                        let mut b2 = Vec::new();
                        let text = read_text(&mut r, &mut b2, &[]);
                        info.names.push((name, local_sheet, text, hidden));
                    }
                    b"extLst" if !empty => {
                        let mut b2 = Vec::new();
                        skip_element(&mut r, &mut b2);
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    info
}

/// The theme's colours, in `clrScheme` order (dk1, lt1, dk2, lt2, accent1–6, hlink, folHlink).
fn read_theme(xml: &[u8]) -> Option<Vec<u32>> {
    let order: [&[u8]; 12] =
        [b"dk1", b"lt1", b"dk2", b"lt2", b"accent1", b"accent2", b"accent3", b"accent4", b"accent5", b"accent6", b"hlink", b"folHlink"];
    let mut colors: Vec<Option<u32>> = vec![None; 12];
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut current: Option<usize> = None;
    let mut in_scheme = false;
    loop {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = name_of(&e);
                if n == b"clrScheme" {
                    in_scheme = true;
                } else if in_scheme {
                    if let Some(i) = order.iter().position(|o| *o == n) {
                        current = Some(i);
                    } else if let Some(i) = current {
                        let v = match n {
                            b"srgbClr" => attr(&e, b"val"),
                            b"sysClr" => attr(&e, b"lastClr"),
                            _ => None,
                        };
                        if let Some(rgb) = v.and_then(|v| crate::formats::colors::parse_hex(&v)) {
                            colors[i].get_or_insert(rgb);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                if local(e.name().into_inner()) == b"clrScheme" {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if colors.iter().all(Option::is_none) {
        return None;
    }
    let defaults = Palette::default().theme;
    Some(colors.into_iter().enumerate().map(|(i, c)| c.unwrap_or(defaults[i])).collect())
}

/// Shared strings, with rich text flattened and phonetic runs left out.
fn read_shared_strings(r: &mut dyn BufRead) -> Vec<Arc<str>> {
    let mut out: Vec<Arc<str>> = Vec::new();
    let mut xr = reader(r);
    let mut buf = Vec::new();
    let mut b2 = Vec::new();
    loop {
        buf.clear();
        match xr.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if name_of(&e) == b"si" => {
                let text = read_text_runs(&mut xr, &mut b2);
                out.push(Arc::from(decode_escapes(&text).as_ref()));
            }
            Ok(Event::Empty(e)) if name_of(&e) == b"si" => out.push(Arc::from("")),
            Ok(Event::Start(e)) if name_of(&e) == b"sst" => {
                if let Some(n) = attr(&e, b"uniqueCount").and_then(|v| v.parse::<usize>().ok()) {
                    out.reserve(n.min(4_000_000));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// The text of `<t>` elements inside a string item (`<si>`, `<is>`, a comment's `<text>`), skipping phonetic
/// runs and properties, until the item ends.
pub(crate) fn read_text_runs<R: BufRead>(xr: &mut quick_xml::Reader<R>, buf: &mut Vec<u8>) -> String {
    let mut out = String::new();
    let mut depth = 1usize;
    let mut in_t = false;
    let mut skip = 0usize;
    loop {
        buf.clear();
        let ev = match xr.read_event_into(buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) => {
                depth += 1;
                let n = name_of(e);
                if skip > 0 || n == b"rPh" || n == b"phoneticPr" || n == b"rPr" {
                    skip += 1;
                } else if n == b"t" {
                    in_t = true;
                }
            }
            Event::End(e) => {
                depth -= 1;
                if skip > 0 {
                    skip -= 1;
                } else if local(e.name().into_inner()) == b"t" {
                    in_t = false;
                }
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            other => {
                if in_t && skip == 0 {
                    crate::formats::xml::push_text(&mut out, other);
                }
            }
        }
    }
    out
}

/// Undoes OOXML's `_xHHHH_` escapes (`_x000D_` is a carriage return; `_x005F_` escapes an underscore).
pub(crate) fn decode_escapes(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains("_x") {
        return std::borrow::Cow::Borrowed(s);
    }
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'_' && i + 6 < b.len() && b[i + 1] == b'x' && b[i + 6] == b'_' && b[i + 2..i + 6].iter().all(u8::is_ascii_hexdigit) {
            if let Some(c) = u32::from_str_radix(&s[i + 2..i + 6], 16).ok().and_then(char::from_u32) {
                out.push(c);
                i += 7;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        out.push(ch);
        i += ch.len_utf8();
    }
    std::borrow::Cow::Owned(out)
}

/// Reads an Excel workbook.
pub(crate) fn import(bytes: &[u8], _opts: &ImportOptions) -> Result<Imported, FormatError> {
    let mut pkg = Package::new(bytes, FORMAT)?;
    // The workbook part, from the package relationships.
    let wb_path = pkg
        .rels("")
        .into_iter()
        .find(|r| r.kind == "officeDocument")
        .map(|r| r.target)
        .filter(|p| pkg.has(p))
        .unwrap_or_else(|| "xl/workbook.xml".to_owned());
    let wb_xml = pkg.read(&wb_path).ok_or_else(|| {
        if pkg.has("Index/Document.iwa") {
            FormatError::read(FORMAT, "it is an Apple Numbers file")
        } else {
            FormatError::read(FORMAT, "it has no workbook part (xl/workbook.xml)")
        }
    })?;
    let info = read_workbook(&wb_xml);
    let wb_rels = pkg.rels(&wb_path);
    let by_id: HashMap<&str, &Rel> = wb_rels.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut palette = Palette::default();
    if let Some(theme) = wb_rels.iter().find(|r| r.kind == "theme").and_then(|r| pkg.read(&r.target)).and_then(|x| read_theme(&x)) {
        palette.theme = theme;
    }
    let styles = match wb_rels.iter().find(|r| r.kind == "styles").and_then(|r| pkg.read(&r.target)) {
        Some(x) => styles::read(&x, &mut palette),
        None => styles::CellStyles::default(),
    };
    let strings: Vec<Arc<str>> = wb_rels
        .iter()
        .find(|r| r.kind == "sharedStrings")
        .and_then(|r| {
            let t = r.target.clone();
            pkg.stream(&t, read_shared_strings)
        })
        .unwrap_or_default();

    let mut warnings = Warnings::default();
    // Worksheets in order (chart sheets and dialog sheets have no cells).
    struct Planned {
        name: String,
        path: String,
        hidden: bool,
    }
    let mut planned: Vec<Planned> = Vec::new();
    for s in &info.sheets {
        match by_id.get(s.rid.as_str()) {
            Some(rel) if rel.kind == "worksheet" && pkg.has(&rel.target) => {
                planned.push(Planned { name: s.name.clone(), path: rel.target.clone(), hidden: s.hidden })
            }
            Some(rel) if rel.kind == "chartsheet" => {
                warnings.say(format!("\"{}\" is a chart sheet, which a sheet can't hold, so it was left out.", s.name))
            }
            Some(rel) if rel.kind == "dialogsheet" || rel.kind == "xlMacrosheet" || rel.kind == "xlIntlMacrosheet" => {
                warnings.say(format!("\"{}\" is a macro or dialog sheet, so it was left out.", s.name))
            }
            _ => {}
        }
    }
    if planned.is_empty() {
        return Err(FormatError::read(FORMAT, "it has no worksheets"));
    }
    let tab_names: Vec<String> = planned.iter().map(|p| p.name.clone()).collect();
    // Index of each workbook sheet among the worksheets kept (local names use workbook indices).
    let sheet_to_tab: Vec<Option<usize>> = info
        .sheets
        .iter()
        .map(|s| planned.iter().position(|p| by_id.get(s.rid.as_str()).is_some_and(|r| r.target == p.path)))
        .collect();

    // Tables first: any sheet's formulas may refer to any table.
    let mut sheet_rels: Vec<Vec<Rel>> = Vec::with_capacity(planned.len());
    let mut tables: Vec<TableDef> = Vec::new();
    for (i, p) in planned.iter().enumerate() {
        let rels = pkg.rels(&p.path);
        for rel in rels.iter().filter(|r| r.kind == "table" && !r.external) {
            if let Some(x) = pkg.read(&rel.target)
                && let Some(t) = formula::read_table(&x, i)
            {
                tables.push(t);
            }
        }
        sheet_rels.push(rels);
    }
    if !tables.is_empty() {
        warnings.say(format!(
            "The file's {} became plain ranges: formulas that used them keep working, but the table styles and filter buttons aren't kept.",
            plural(tables.len(), "table", "tables")
        ));
    }

    let names = IndexNames(&tab_names);
    let ctx = sheet::Ctx {
        strings: &strings,
        styles: &styles,
        palette: &palette,
        names: &names,
        tab_names: &tab_names,
        tables: &tables,
        date1904: info.date1904,
    };
    let mut tabs = Vec::with_capacity(planned.len());
    let mut tab_info = Vec::with_capacity(planned.len());
    let mut charts = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    for (i, p) in planned.iter().enumerate() {
        let out = pkg
            .stream(&p.path, |r| sheet::read(r, i, &ctx, &mut warnings))
            .ok_or_else(|| FormatError::read(FORMAT, format!("the worksheet \"{}\" can't be read", p.name)))??;
        let rels = &sheet_rels[i];
        if let Some(rid) = &out.drawing
            && let Some(rel) = rels.iter().find(|r| &r.id == rid && !r.external)
        {
            let found = drawing::read(&mut pkg, &rel.target, i, &out.content, &tab_names, &mut warnings);
            charts.extend(found);
        }
        let threaded = rels.iter().find(|r| r.kind == "threadedComment" && !r.external);
        let legacy = rels.iter().find(|r| r.kind == "comments" && !r.external);
        let persons = wb_rels.iter().find(|r| r.kind == "person").map(|r| r.target.clone());
        if let Some(rel) = threaded {
            notes.extend(comments::read_threaded(&mut pkg, &rel.target, persons.as_deref(), i));
        } else if let Some(rel) = legacy {
            notes.extend(comments::read_legacy(&mut pkg, &rel.target, i));
        }
        if p.hidden {
            warnings.say(format!("\"{}\" was hidden in the file; sheets show every tab.", p.name));
        }
        tab_info.push(TabInfo { color: out.color, hidden: p.hidden });
        tabs.push((p.name.clone(), out.content));
    }
    for (k, c) in charts.iter_mut().enumerate() {
        c.id = format!("chart{}", k + 1);
    }

    // Defined names (Excel's own `_xlnm.` names, like print areas and filter ranges, are left out).
    let mut defined = Vec::new();
    for (name, local_sheet, text, _hidden) in &info.names {
        if name.starts_with("_xlnm.") || name.is_empty() || text.trim().is_empty() {
            continue;
        }
        let tab = match local_sheet {
            Some(s) => match sheet_to_tab.get(*s as usize).copied().flatten() {
                Some(t) => Some(t as u32),
                None => continue,
            },
            None => None,
        };
        let text = text.trim().trim_start_matches('=').to_owned();
        if text.contains('[') && formula::is_external(&text) {
            warnings.count("external_name", || name.clone());
            continue;
        }
        defined.push(DefinedName { name: name.clone(), text, tab });
    }

    let warnings = warnings.finish(describe);
    Ok(Imported { tabs, names: defined, warnings, tab_info, charts, notes })
}

fn describe(kind: &str, n: usize) -> String {
    match kind {
        "external" => format!(
            "{} refer to other files, which a sheet can't reach, so they keep the values they last had",
            plural(n, "formula", "formulas")
        ),
        "unreadable" => format!(
            "{} use something Calc can't read yet, so they keep the values they last had",
            plural(n, "formula", "formulas")
        ),
        "data_table" => format!("{} of what-if data tables keep the values they last had", plural(n, "cell", "cells")),
        "external_name" => format!("{} refer to other files and were left out", plural(n, "defined name", "defined names")),
        "hyperlink" => format!("{} were kept as their text, without the link", plural(n, "linked cell", "linked cells")),
        "conditional" => format!("{} aren't kept", plural(n, "conditional format", "conditional formats")),
        "validation" => format!("{} aren't kept", plural(n, "data validation rule", "data validation rules")),
        "picture" => format!("{} aren't kept", plural(n, "picture", "pictures")),
        "shape" => format!("{} aren't kept", plural(n, "shape or text box", "shapes and text boxes")),
        "chart_kind" => format!("{} drawn as the nearest kind a sheet has", plural(n, "chart was", "charts were")),
        "chart_data" => format!("{} use data a sheet chart can't follow and were left out", plural(n, "chart", "charts")),
        "rotation" => format!("{} lost their text rotation", plural(n, "style", "styles")),
        "filter" => format!("{} lost their filter buttons (the rows they hid stay hidden)", plural(n, "tab", "tabs")),
        other => format!("{n} × {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(resolve("xl", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve("xl/worksheets", "../drawings/drawing1.xml"), "xl/drawings/drawing1.xml");
        assert_eq!(resolve("xl", "/xl/styles.xml"), "xl/styles.xml");
        assert_eq!(resolve("", "xl/workbook.xml"), "xl/workbook.xml");
    }

    #[test]
    fn escapes() {
        assert_eq!(decode_escapes("a_x000D_b"), "a\rb");
        assert_eq!(decode_escapes("_x005F_x000D_"), "_x000D_");
        assert_eq!(decode_escapes("plain_x"), "plain_x");
    }
}
