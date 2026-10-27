//! Portable values for other spreadsheet engines; inert source metadata for Calc reimport.
use super::{
    ExportOptions, FormatError, Imported, IndexNames, export_tabs, rename_content,
    rename_in_formula,
    xml::{push_text, reader},
};
use crate::{
    CellPos, ErrorKind, Value, Workbook,
    formula::{
        ast::{Coord, Expr, RangeRef},
        parse::{parse_formula, to_a1},
    },
    javascript::Function,
    model::{CodeSource, DefinedName},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{Cursor, Read, Write},
};

pub const PART: &str = "silicon-calc/programs.xml";
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Programs {
    version: u32,
    functions: Vec<Function>,
    names: Vec<DefinedName>,
    tabs: Vec<TabPrograms>,
}
#[derive(Serialize, Deserialize)]
struct TabPrograms {
    id: u32,
    name: String,
    original_name: String,
    code: Option<CodeSource>,
    formulas: Vec<Formula>,
}
#[derive(Serialize, Deserialize)]
struct Formula {
    row: u32,
    col: u32,
    text: String,
    /// The exported footprint, used to avoid restoring a formula over changed output.
    values: Vec<Vec<Value>>,
    /// A dependent formula whose JS spill reference was changed to an ordinary range.
    expected_formula: Option<String>,
}

pub fn ensure_calculated(
    wb: &Workbook,
    format: super::Format,
    opts: &ExportOptions,
) -> Result<(), FormatError> {
    let selected = if format.holds_many_tabs() {
        opts.tab
    } else {
        super::export_tab(wb, opts)
    };
    for tab in wb
        .tabs
        .iter()
        .filter(|t| selected.is_none_or(|id| id == t.id))
    {
        if tab
            .occupied()
            .iter()
            .any(|&(r, c)| tab.cell_by_id(r, c).value == Value::Error(ErrorKind::Busy))
        {
            return Err(FormatError::write(
                "spreadsheet",
                "JavaScript is still calculating. Open the sheet with edit access, wait for calculation to finish, then download again.",
            ));
        }
    }
    Ok(())
}

pub(crate) fn uses_javascript(wb: &Workbook, expr: &Expr) -> bool {
    fn check(wb: &Workbook, expr: &Expr, seen: &mut HashSet<String>) -> bool {
        let mut found = false;
        expr.walk(&mut |e| match e {
            Expr::Call(n, _) if n.eq_ignore_ascii_case("JS") || wb.function(n).is_some() => {
                found = true
            }
            Expr::Name(n) if seen.insert(n.to_ascii_uppercase()) => {
                for name in wb
                    .names
                    .iter()
                    .filter(|name| name.name.eq_ignore_ascii_case(n))
                {
                    if let Ok(e) = parse_formula(&name.text, 0, 0, &wb.names()) {
                        found |= check(wb, &e, seen);
                    }
                }
            }
            _ => {}
        });
        found
    }
    check(wb, expr, &mut HashSet::new())
}

pub(crate) fn portable_expr(wb: &Workbook, expr: &Expr, tab: u32, row: u32, col: u32) -> Expr {
    match expr {
        Expr::Spill(c) => {
            let Some(t) = wb.tab(c.tab.unwrap_or(tab)) else {
                return expr.clone();
            };
            let (Some(r), Some(column)) = (c.row.resolve(row), c.col.resolve(col)) else {
                return expr.clone();
            };
            let cell = t.cell_at(CellPos::new(r, column));
            if !cell
                .formula
                .and_then(|g| wb.group(g))
                .is_some_and(|g| uses_javascript(wb, &g.expr))
            {
                return expr.clone();
            }
            let spill = t
                .rows
                .id_at(r)
                .zip(t.cols.id_at(column))
                .and_then(|(rid, cid)| wb.spills.get(&(t.id, rid, cid)))
                .filter(|s| !s.blocked);
            match spill {
                Some(s) => Expr::Range(RangeRef {
                    tab: c.tab,
                    rows: Some((Coord::Abs(r), Coord::Abs(r + s.rows - 1))),
                    cols: Some((Coord::Abs(column), Coord::Abs(column + s.cols - 1))),
                }),
                // A blocked or scalar JS result has no spill range. Match Calc's #REF!
                // instead of summing a text error cell as zero in another spreadsheet.
                None => Expr::Error(ErrorKind::Ref),
            }
        }
        Expr::Call(name, args) => Expr::Call(
            name.clone(),
            args.iter()
                .map(|e| portable_expr(wb, e, tab, row, col))
                .collect(),
        ),
        Expr::Unary(op, e) => Expr::Unary(*op, Box::new(portable_expr(wb, e, tab, row, col))),
        Expr::Binary(op, a, b) => Expr::Binary(
            *op,
            Box::new(portable_expr(wb, a, tab, row, col)),
            Box::new(portable_expr(wb, b, tab, row, col)),
        ),
        Expr::Paren(e) => Expr::Paren(Box::new(portable_expr(wb, e, tab, row, col))),
        Expr::Array(rows) => Expr::Array(
            rows.iter()
                .map(|r| {
                    r.iter()
                        .map(|e| portable_expr(wb, e, tab, row, col))
                        .collect()
                })
                .collect(),
        ),
        Expr::Invoke(e, args) => Expr::Invoke(
            Box::new(portable_expr(wb, e, tab, row, col)),
            args.iter()
                .map(|e| portable_expr(wb, e, tab, row, col))
                .collect(),
        ),
        _ => expr.clone(),
    }
}

/// The sheet names must be the names actually written in the destination format.
pub(crate) fn encode(
    wb: &Workbook,
    opts: &ExportOptions,
    names: &[(u32, String)],
) -> Result<Option<Vec<u8>>, FormatError> {
    if opts.values_only {
        return Ok(None);
    }
    let ids = export_tabs(wb, opts, "spreadsheet")?;
    let mut tabs = Vec::new();
    for id in &ids {
        let tab = wb.tab(*id).expect("export tab");
        let mut formulas = Vec::new();
        for (rid, cid) in tab.occupied() {
            let cell = tab.cell_by_id(rid, cid);
            if cell.spilled {
                continue;
            }
            let Some(g) = cell.formula.and_then(|id| wb.group(id)) else {
                continue;
            };
            let (Some(row), Some(col)) = (tab.rows.pos_of(rid), tab.cols.pos_of(cid)) else {
                continue;
            };
            let js = uses_javascript(wb, &g.expr);
            let portable = portable_expr(wb, &g.expr, *id, row, col);
            if !js && portable == *g.expr {
                continue;
            }
            let spill = wb.spills.get(&(*id, rid, cid)).filter(|s| !s.blocked);
            let (rows, cols) = spill.map(|s| (s.rows, s.cols)).unwrap_or((1, 1));
            let values = (row..row + rows)
                .map(|r| {
                    (col..col + cols)
                        .map(|c| tab.cell_at(CellPos::new(r, c)).value)
                        .collect()
                })
                .collect();
            formulas.push(Formula {
                row,
                col,
                text: wb.formula_text(*id, row, col).expect("formula"),
                values,
                expected_formula: (!js).then(|| to_a1(&portable, row, col, &wb.names())),
            });
        }
        tabs.push(TabPrograms {
            id: *id,
            name: names
                .iter()
                .find(|n| n.0 == *id)
                .map(|n| n.1.clone())
                .unwrap_or_else(|| tab.name.clone()),
            original_name: tab.name.clone(),
            code: tab.code.clone(),
            formulas,
        });
    }
    let names: Vec<_> = wb
        .names
        .iter()
        .filter(|n| {
            parse_formula(&n.text, 0, 0, &wb.names()).is_ok_and(|e| {
                uses_javascript(wb, &e) || portable_expr(wb, &e, n.tab.unwrap_or(ids[0]), 0, 0) != e
            })
        })
        .filter_map(|n| {
            let mut n = n.clone();
            if let Some(t) = n.tab {
                n.tab = Some(ids.iter().position(|id| *id == t)? as u32);
            }
            Some(n)
        })
        .collect();
    if wb.functions.is_empty()
        && names.is_empty()
        && tabs
            .iter()
            .all(|t| t.formulas.is_empty() && t.code.is_none())
    {
        return Ok(None);
    }
    // Exported definitions are standalone copies, independent of an account library.
    let functions = wb
        .functions
        .iter()
        .filter(|f| wb.function(&f.name) == Some(*f))
        .map(|f| Function {
            account: None,
            ..f.clone()
        })
        .collect();
    let json = serde_json::to_string(&Programs {
        version: 1,
        functions,
        names,
        tabs,
    })
    .map_err(|e| FormatError::write("spreadsheet", e.to_string()))?;
    if json.len() > MAX_BYTES {
        return Err(FormatError::write(
            "spreadsheet",
            "The saved JavaScript exceeds 32 MB. Download fewer tabs at a time.",
        ));
    }
    Ok(Some(
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><calc-programs>{}</calc-programs>",
            super::html::escape(&json)
        )
        .into_bytes(),
    ))
}

pub fn read_package(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let mut file = zip.by_name(PART).ok()?;
    if file.size() > (MAX_BYTES * 6) as u64 {
        return None;
    }
    let mut out = Vec::new();
    file.by_ref()
        .take((MAX_BYTES * 6 + 1) as u64)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() <= MAX_BYTES * 6).then_some(out)
}

/// ODS conversion drops foreign XLSX parts; explicitly carry the inert metadata across it.
pub fn append_package(
    bytes: Vec<u8>,
    xml: Option<&[u8]>,
    ods: bool,
) -> Result<Vec<u8>, FormatError> {
    let Some(xml) = xml else {
        return Ok(bytes);
    };
    let err = |e: String| FormatError::write("spreadsheet", e);
    let mut source = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| err(e.to_string()))?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for i in 0..source.len() {
        let mut file = source.by_index(i).map_err(|e| err(e.to_string()))?;
        if file.name() == PART {
            continue;
        }
        if ods && file.name() == "META-INF/manifest.xml" {
            let mut manifest = String::new();
            file.read_to_string(&mut manifest)
                .map_err(|e| err(e.to_string()))?;
            let entry = format!(
                "<manifest:file-entry manifest:full-path=\"{PART}\" manifest:media-type=\"text/xml\"/>"
            );
            let manifest =
                manifest.replace("</manifest:manifest>", &(entry + "</manifest:manifest>"));
            out.start_file("META-INF/manifest.xml", options)
                .map_err(|e| err(e.to_string()))?;
            out.write_all(manifest.as_bytes())
                .map_err(|e| err(e.to_string()))?;
        } else {
            out.raw_copy_file(file).map_err(|e| err(e.to_string()))?;
        }
    }
    out.start_file(PART, options)
        .map_err(|e| err(e.to_string()))?;
    out.write_all(xml).map_err(|e| err(e.to_string()))?;
    Ok(out.finish().map_err(|e| err(e.to_string()))?.into_inner())
}

fn same_value(a: &Value, b: &Value) -> bool {
    a == b
        || match (a, b) {
            // Office conversions retain 15 significant decimal digits. Do not mistake their rounding for an edit.
            (Value::Number(a), Value::Number(b)) => {
                super::number_text(*a) == super::number_text(*b)
            }
            (Value::Error(e), Value::Text(s)) => e.as_str() == s.as_ref(),
            _ => false,
        }
}

pub fn restore(xml: &[u8], imported: &mut Imported) {
    let failure =
        "The Calc JavaScript metadata could not be restored; the file's exported values were kept.";
    let decode = || -> Option<Programs> {
        if xml.len() > MAX_BYTES * 6 {
            return None;
        }
        let mut reader = reader(xml);
        let mut json = String::new();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_event_into(&mut buf) {
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => return None,
                Ok(e) => push_text(&mut json, &e),
            }
            if json.len() > MAX_BYTES {
                return None;
            }
        }
        let p: Programs = serde_json::from_str(&json).ok()?;
        if p.version != 1
            || p.functions.len() > 256
            || p.tabs.len() != imported.tabs.len()
            || p.names.len() > 10000
        {
            return None;
        }
        let mut keys = HashSet::new();
        if p.functions
            .iter()
            .any(|f| f.validate().is_err() || !keys.insert(f.name.to_ascii_uppercase()))
        {
            return None;
        }
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        let mut exported_names = HashSet::new();
        for t in &p.tabs {
            if t.id == 0
                || t.id >= u32::MAX - 10000
                || !ids.insert(t.id)
                || !crate::ops::valid_tab_name(&t.original_name)
                || !names.insert(t.original_name.to_lowercase())
                || !exported_names.insert(&t.name)
                || !imported.tabs.iter().any(|(n, _)| n == &t.name)
            {
                return None;
            }
            if t.code.as_ref().is_some_and(|c| {
                c.language != "javascript" || c.code.len() > 1 << 20 || c.inputs.len() > 10000
            }) {
                return None;
            }
            for f in &t.formulas {
                let cols = f.values.first()?.len();
                if f.text.len() > 1 << 20
                    || cols == 0
                    || f.values.iter().any(|r| r.len() != cols)
                    || f.row as usize + f.values.len() > 1_048_576
                    || f.col as usize + cols > 16_384
                {
                    return None;
                }
            }
        }
        Some(p)
    };
    let Some(p) = decode() else {
        imported.warnings.push(failure.into());
        return;
    };
    let old: Vec<_> = imported.tabs.iter().map(|t| t.0.clone()).collect();
    let new: Vec<_> = old
        .iter()
        .map(|name| {
            p.tabs
                .iter()
                .find(|t| &t.name == name)
                .unwrap()
                .original_name
                .clone()
        })
        .collect();
    for (index, (name, content)) in imported.tabs.iter_mut().enumerate() {
        let saved = p.tabs.iter().find(|t| &t.name == name).unwrap();
        *content = rename_content(content, &IndexNames(&old), &IndexNames(&new));
        *name = new[index].clone();
        content.code = saved.code.clone();
        for f in &saved.formulas {
            let unchanged = f.values.iter().enumerate().all(|(r, row)| {
                row.iter().enumerate().all(|(c, v)| {
                    let col = content.columns.get(f.col as usize + c);
                    let actual = col
                        .and_then(|col| {
                            col.formulas
                                .iter()
                                .find(|(row, _)| *row == f.row + r as u32)
                        })
                        .map(|f| f.1.as_str());
                    let expected = if r == 0 && c == 0 {
                        f.expected_formula.as_deref()
                    } else {
                        None
                    };
                    // XLSX represents classic error values with a constant error formula.
                    (actual == expected
                        || expected.is_none()
                            && matches!(v,Value::Error(e) if actual==Some(e.as_str())))
                        && same_value(
                            v,
                            col.and_then(|col| col.values.get(f.row as usize + r))
                                .unwrap_or(&Value::Empty),
                        )
                })
            });
            if !unchanged {
                imported.warnings.push(format!("The exported result at {name}!{} changed outside Calc; those cells were kept without restoring the original formula.",CellPos::new(f.row,f.col)));
                continue;
            }
            for (r, row) in f.values.iter().enumerate() {
                for c in 0..row.len() {
                    if let Some(value) = content
                        .columns
                        .get_mut(f.col as usize + c)
                        .and_then(|col| col.values.get_mut(f.row as usize + r))
                    {
                        *value = Value::Empty;
                    }
                }
            }
            content.cols = content.cols.max(f.col + 1);
            content.rows = content.rows.max(f.row + 1);
            content
                .columns
                .resize_with(content.cols as usize, Default::default);
            content.columns[f.col as usize]
                .formulas
                .retain(|(row, _)| *row != f.row);
            content.columns[f.col as usize]
                .formulas
                .push((f.row, f.text.clone()));
            content.columns[f.col as usize]
                .formulas
                .sort_by_key(|f| f.0);
        }
        imported
            .source_tabs
            .push((saved.id, saved.original_name.clone()));
    }
    for n in &mut imported.names {
        n.text = rename_in_formula(&n.text, 0, 0, &IndexNames(&old), &IndexNames(&new));
    }
    for saved in p.names {
        if let Some(name) = imported
            .names
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(&saved.name) && n.tab == saved.tab)
        {
            *name = saved;
        } else {
            imported.names.push(saved);
        }
    }
    imported.functions = p.functions;
}

/// Retain literal tab references when a saved analysis is imported into another workbook.
pub(crate) fn bind_source(
    code: &str,
    source: &[(u32, String)],
    ids: &[u32],
    names: &[String],
) -> String {
    if source
        .iter()
        .enumerate()
        .all(|(i, (id, name))| ids.get(i) == Some(id) && names.get(i) == Some(name))
    {
        return code.into();
    }
    let bindings: Vec<_> = source
        .iter()
        .enumerate()
        .map(|(i, (id, name))| serde_json::json!([id, name, ids.get(i), names.get(i)]))
        .collect();
    let bindings = serde_json::to_string(&bindings).expect("bindings");
    format!(
        "return await (async (calc) => {{\n{code}\n}})((base => {{\nconst bindings = {bindings};\nconst resolve = ref => {{ const b = bindings.find(b => b[0] === ref || b[1] === ref); return b ? b[2] : ref; }};\nconst values = (tab, range) => range === undefined ? base.values(tab) : base.values(resolve(tab), range);\nreturn Object.freeze({{ ...base, context: Object.freeze({{ ...base.context, tab: bindings.find(b => b[2] === base.context?.tab)?.[0] ?? base.context?.tab }}), values, range: values, cell: async (tab, address) => (await values(tab,address))[0]?.[0] ?? null, tab: tab => tab === undefined ? base.tab() : base.tab(resolve(tab)), tabs: () => bindings.flatMap(b => {{ const t=base.tabs().find(t => t.id===b[2]); return t ? [{{...t,id:b[0],name:b[1]}}] : []; }}) }});\n}})(calc));"
    )
}

pub(crate) fn bind_formula(
    text: &str,
    row: u32,
    col: u32,
    source: &[(u32, String)],
    ids: &[u32],
    names: &[String],
) -> String {
    fn visit(e: &mut Expr, source: &[(u32, String)], ids: &[u32], names: &[String]) {
        match e {
            Expr::Call(name, args) => {
                if name.eq_ignore_ascii_case("JS")
                    && let Some(Expr::Text(code)) = args.first_mut()
                {
                    *code = bind_source(code, source, ids, names).into();
                }
                for arg in args {
                    visit(arg, source, ids, names);
                }
            }
            Expr::Unary(_, e) | Expr::Paren(e) => visit(e, source, ids, names),
            Expr::Binary(_, a, b) => {
                visit(a, source, ids, names);
                visit(b, source, ids, names);
            }
            Expr::Array(rows) => {
                for row in rows {
                    for e in row {
                        visit(e, source, ids, names);
                    }
                }
            }
            Expr::Invoke(e, args) => {
                visit(e, source, ids, names);
                for a in args {
                    visit(a, source, ids, names);
                }
            }
            _ => {}
        }
    }
    let tab_names = IndexNames(names);
    let Ok(mut e) = parse_formula(text, row, col, &tab_names) else {
        return text.into();
    };
    visit(&mut e, source, ids, names);
    crate::formula::parse::to_a1(&e, row, col, &tab_names)
}
