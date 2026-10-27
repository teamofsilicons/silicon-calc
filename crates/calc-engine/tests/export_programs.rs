use calc_engine::{
    ErrorKind, Op, Value, Workbook,
    formats::{self, ExportOptions, Format, ImportOptions},
    javascript::{Answer, Function},
    model::CodeSource,
    ops::{Actor, CellWrite, TabContent},
};
use std::io::{Cursor, Read};
const ACTOR: Actor<'static> = Actor {
    account: "c:exports",
    at: 1_800_000_000,
};

fn set(w: &mut Workbook, tab: u32, r: u32, c: u32, input: &str) {
    let op = w
        .prepare_writes(
            tab,
            vec![(
                r,
                c,
                CellWrite {
                    input: Some(input.into()),
                    ..Default::default()
                },
            )],
        )
        .unwrap();
    w.apply(&op, &ACTOR).unwrap();
}
fn complete(w: &mut Workbook) {
    for _ in 0..8 {
        let js = w.javascript.lock().unwrap().clone();
        let results = js
            .calls
            .iter()
            .filter(|c| c.result.is_none())
            .map(|c| {
                let rows = if c.name == "JS" {
                    vec![
                        vec![Value::Number(1.), Value::Number(2.)],
                        vec![Value::Number(3.), Value::Number(4.)],
                    ]
                } else {
                    vec![vec![Value::Number(c.args[0].as_f64().unwrap() * 2.)]]
                };
                (
                    c.id,
                    Answer {
                        rows,
                        error: None,
                        dependencies: None,
                    },
                )
            })
            .collect::<Vec<_>>();
        if results.is_empty() {
            return;
        }
        w.apply(
            &Op::JavascriptResults {
                epoch: js.epoch,
                results,
            },
            &ACTOR,
        )
        .unwrap();
    }
    panic!("calculation did not finish");
}
fn fixture() -> Workbook {
    let mut w = Workbook::empty();
    w.apply(
        &Op::AddTab {
            tab: 4,
            name: "Sales and input data for fiscal 2026".into(),
            index: None,
        },
        &ACTOR,
    )
    .unwrap();
    w.apply(
        &Op::SetFunctions {
            functions: vec![Function {
                name: "DOUBLE_VALUE".into(),
                parameters: vec!["value".into()],
                code: "return value * 2;".into(),
                account: None,
            }],
        },
        &ACTOR,
    )
    .unwrap();
    for (r, c, v) in [
        (0, 0, "5"),
        (0, 1, "=DOUBLE_VALUE(A1)"),
        (0, 2, "=B1+1"),
        (0, 3, "=JS(\"return [[1,2],[3,4]];\")"),
        (0, 5, "=SUM(D1#)"),
        (2, 0, "<script>alert('x')</script>"),
    ] {
        set(&mut w, 4, r, c, v);
    }
    w.apply(
        &Op::AddTab {
            tab: 12,
            name: "Summary".into(),
            index: None,
        },
        &ACTOR,
    )
    .unwrap();
    let mut content =
        TabContent::from_rows(vec![vec![Value::from("Total")], vec![Value::Number(10.)]]);
    content.code = Some(CodeSource {
        language: "javascript".into(),
        code: "return await calc.tab(4);".into(),
        inputs: vec![4],
        run_by: "c:exports".into(),
        run_at: 1_800_000_000,
        keep_updated: false,
    });
    w.apply(
        &Op::ReplaceTab {
            tab: 12,
            content: Box::new(content),
        },
        &ACTOR,
    )
    .unwrap();
    complete(&mut w);
    w
}
fn part(bytes: &[u8], name: &str) -> String {
    let mut z = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}
fn without_programs(bytes: &[u8]) -> Vec<u8> {
    let mut input = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..input.len() {
        let f = input.by_index(i).unwrap();
        if f.name() != formats::programs::PART {
            out.raw_copy_file(f).unwrap();
        }
    }
    out.finish().unwrap().into_inner()
}
#[test]
fn excel_has_portable_values_and_native_formulas_and_calc_restores_the_source() {
    let w = fixture();
    let bytes = formats::export(&w, Format::Xlsx, &ExportOptions::default()).unwrap();
    let xml = part(&bytes, "xl/worksheets/sheet1.xml");
    assert!(!xml.contains("DOUBLE_VALUE") && !xml.contains("JS("));
    assert!(!xml.contains("ANCHORARRAY"));
    assert!(
        xml.contains("<f>B1+1</f>") && xml.contains("<f>SUM($D$1:$E$2)</f>"),
        "{xml}"
    );
    let portable = formats::import(
        Format::Xlsx,
        &without_programs(&bytes),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut other = Workbook::empty();
    other
        .apply(&portable.plan(&other, None).op, &ACTOR)
        .unwrap();
    assert_eq!(other.tabs[0].value_at(0, 1), Value::Number(10.));
    assert_eq!(other.tabs[0].value_at(0, 5), Value::Number(10.));
    assert_eq!(other.tabs[0].value_at(0, 2), Value::Number(11.));
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let mut back = Workbook::empty();
    back.apply(&imported.plan(&back, None).op, &ACTOR).unwrap();
    assert_eq!(back.functions, w.functions);
    assert_eq!(back.tabs[0].id, 4);
    assert_eq!(back.tabs[0].name, w.tabs[0].name);
    assert_eq!(back.tab(12).unwrap().code, w.tab(12).unwrap().code);
    assert_eq!(
        back.formula_text(4, 0, 1).as_deref(),
        Some("DOUBLE_VALUE(A1)")
    );
    assert_eq!(back.formula_text(4, 0, 5).as_deref(), Some("SUM(D1#)"));
    complete(&mut back);
    assert_eq!(back.tab(4).unwrap().value_at(1, 4), Value::Number(4.));
    set(&mut back, 4, 0, 0, "7");
    complete(&mut back);
    assert_eq!(back.tab(4).unwrap().value_at(0, 2), Value::Number(15.));
    if let Ok(dir) = std::env::var("CALC_EXPORT_FIXTURE_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            std::path::Path::new(&dir).join("calc-compatibility.xlsx"),
            bytes,
        )
        .unwrap();
    }
}
#[test]
fn values_formats_do_not_export_formulas_and_html_escapes_content() {
    let w = fixture();
    for format in [Format::Csv, Format::Tsv] {
        let text =
            String::from_utf8(formats::export(&w, format, &ExportOptions::default()).unwrap())
                .unwrap();
        assert!(text.contains("10") && text.contains("11"));
        assert!(!text.contains("DOUBLE_VALUE") && !text.contains("=SUM"));
    }
    let html =
        String::from_utf8(formats::export(&w, Format::Html, &ExportOptions::default()).unwrap())
            .unwrap();
    assert!(html.contains("&lt;script&gt;") && !html.contains("<script>"));
    assert!(html.contains("Summary") && html.contains("Sales and input data for fiscal 2026"));
    assert!(!html.contains("DOUBLE_VALUE"));
}
#[test]
fn pending_calculations_are_not_exported_as_finished_values() {
    let mut w = fixture();
    set(&mut w, 4, 0, 0, "7");
    assert_eq!(
        w.tab(4).unwrap().value_at(0, 1),
        Value::Error(ErrorKind::Busy)
    );
    for format in [
        Format::Xlsx,
        Format::Csv,
        Format::Tsv,
        Format::Html,
        Format::Json,
    ] {
        assert!(
            formats::export(&w, format, &ExportOptions::default())
                .unwrap_err()
                .to_string()
                .contains("still calculating")
        );
    }
}
#[test]
fn importing_into_existing_tabs_keeps_literal_script_references_bound() {
    let bytes = formats::export(&fixture(), Format::Xlsx, &ExportOptions::default()).unwrap();
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    let mut w = Workbook::new();
    w.apply(&imported.plan(&w, None).op, &ACTOR).unwrap();
    let code = w.tabs.last().unwrap().code.as_ref().unwrap();
    assert_eq!(code.inputs, vec![2]);
    assert!(code.code.contains("const bindings = [[4,"));
    assert!(w.functions[0].code.contains("return value * 2;"));
    let op = Op::SetFunctions {
        functions: vec![Function {
            name: "DOUBLE_VALUE".into(),
            parameters: vec!["v".into()],
            code: "return v * 3;".into(),
            account: None,
        }],
    };
    w.apply(&op, &ACTOR).unwrap();
    assert!(
        w.apply(&imported.plan(&w, None).op, &ACTOR)
            .unwrap_err()
            .to_string()
            .contains("different definition")
    );
}

#[test]
fn changed_values_and_invalid_metadata_are_not_overwritten() {
    let bytes = formats::export(&fixture(), Format::Xlsx, &ExportOptions::default()).unwrap();
    let xml = formats::programs::read_package(&bytes).unwrap();
    let mut imported = formats::import(
        Format::Xlsx,
        &without_programs(&bytes),
        &ImportOptions::default(),
    )
    .unwrap();
    imported.tabs[0].1.columns[1].values[0] = Value::Number(99.);
    formats::programs::restore(&xml, &mut imported);
    assert_eq!(imported.warnings.len(), 1);
    assert!(imported.tabs[0].1.columns[1].formulas.is_empty());
    let mut wb = Workbook::empty();
    wb.apply(&imported.plan(&wb, None).op, &ACTOR).unwrap();
    complete(&mut wb);
    assert_eq!(wb.tabs[0].value_at(0, 1), Value::Number(99.));
    assert_eq!(wb.tabs[0].value_at(0, 2), Value::Number(100.));
    let mut imported = formats::import(
        Format::Xlsx,
        &without_programs(&bytes),
        &ImportOptions::default(),
    )
    .unwrap();
    let malformed = String::from_utf8(xml)
        .unwrap()
        .replace("Summary", "Sales and input data for fiscal");
    formats::programs::restore(malformed.as_bytes(), &mut imported);
    assert_eq!(imported.warnings.len(), 1);
    assert!(imported.functions.is_empty() && imported.source_tabs.is_empty());
}

#[test]
fn explicit_values_only_download_omits_source_metadata() {
    let bytes = formats::export(
        &fixture(),
        Format::Xlsx,
        &ExportOptions {
            values_only: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(formats::programs::read_package(&bytes).is_none());
    let xml = part(&bytes, "xl/worksheets/sheet1.xml");
    assert!(!xml.contains("<f>"));
}

#[test]
fn defined_names_referencing_javascript_spills_are_portable() {
    use calc_engine::model::DefinedName;
    let mut wb = fixture();
    let names = vec![DefinedName {
        name: "Results".into(),
        text: "'Sales and input data for fiscal 2026'!$D$1#".into(),
        tab: None,
    }];
    wb.apply(
        &Op::SetNames {
            names: names.clone(),
        },
        &ACTOR,
    )
    .unwrap();
    set(&mut wb, 4, 0, 6, "=SUM(Results)");
    complete(&mut wb);
    let bytes = formats::export(&wb, Format::Xlsx, &ExportOptions::default()).unwrap();
    assert!(!part(&bytes, "xl/workbook.xml").contains("ANCHORARRAY"));
    let portable = formats::import(
        Format::Xlsx,
        &without_programs(&bytes),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut other = Workbook::empty();
    other
        .apply(&portable.plan(&other, None).op, &ACTOR)
        .unwrap();
    assert_eq!(other.tabs[0].value_at(0, 6), Value::Number(10.));
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    let mut back = Workbook::empty();
    back.apply(&imported.plan(&back, None).op, &ACTOR).unwrap();
    complete(&mut back);
    assert_eq!(back.names, names);
    assert_eq!(back.tabs[0].value_at(0, 6), Value::Number(10.));
}

#[test]
fn javascript_formulas_with_calculated_errors_keep_their_source() {
    let mut wb = fixture();
    set(&mut wb, 4, 0, 0, "0");
    set(&mut wb, 4, 0, 1, "=1/DOUBLE_VALUE(A1)");
    complete(&mut wb);
    assert_eq!(wb.tabs[0].value_at(0, 1), Value::Error(ErrorKind::Div0));
    let bytes = formats::export(&wb, Format::Xlsx, &ExportOptions::default()).unwrap();
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let mut back = Workbook::empty();
    back.apply(&imported.plan(&back, None).op, &ACTOR).unwrap();
    complete(&mut back);
    set(&mut back, 4, 0, 0, "5");
    complete(&mut back);
    assert_eq!(back.tabs[0].value_at(0, 1), Value::Number(0.1));
}

#[test]
fn blocked_javascript_spills_do_not_turn_dependent_errors_into_zero() {
    let mut wb = fixture();
    set(&mut wb, 4, 0, 4, "blocked");
    complete(&mut wb);
    assert_eq!(wb.tabs[0].value_at(0, 5), Value::Error(ErrorKind::Ref));
    let bytes = formats::export(&wb, Format::Xlsx, &ExportOptions::default()).unwrap();
    let portable = formats::import(
        Format::Xlsx,
        &without_programs(&bytes),
        &ImportOptions::default(),
    )
    .unwrap();
    let mut other = Workbook::empty();
    other
        .apply(&portable.plan(&other, None).op, &ACTOR)
        .unwrap();
    assert_eq!(other.tabs[0].value_at(0, 5), Value::Error(ErrorKind::Ref));
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let mut back = Workbook::empty();
    back.apply(&imported.plan(&back, None).op, &ACTOR).unwrap();
    set(&mut back, 4, 0, 4, "");
    complete(&mut back);
    assert_eq!(back.tabs[0].value_at(0, 5), Value::Number(10.));
}
