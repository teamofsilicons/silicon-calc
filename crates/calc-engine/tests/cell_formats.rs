use calc_engine::{addr::{CellPos, Rect}, formats::{self, ExportOptions, Format, ImportOptions}, numfmt::format_number, ops::{Actor, CellWrite, ClearWhat}, snapshot, style::{CellControl, Style, StylePatch}, Value, Workbook};

const ACTOR: Actor<'static> = Actor { account: "c:formats", at: 1_800_000_000 };
fn set(w: &mut Workbook, address: &str, input: &str) {
    let p = CellPos::parse(address).unwrap();
    let op = w.prepare_writes(1, vec![(p.row, p.col, CellWrite { input: Some(input.into()), clear: true, ..Default::default() })]).unwrap();
    w.apply(&op, &ACTOR).unwrap();
}
fn style(w: &Workbook, address: &str) -> Style {
    w.style_of(w.tab(1).unwrap().cell_at(CellPos::parse(address).unwrap()).style).clone()
}
fn range(address: &str) -> Rect { Rect::parse(address, 1000, 26).unwrap() }

#[test]
fn checkbox_values_undo_snapshots_clear_and_formulas() {
    let mut w = Workbook::new();
    set(&mut w, "B1", "=IF(A1,10,0)");
    let op = w.prepare_style(1, range("A1:A3"), StylePatch { control: Some(Some(CellControl::Checkbox { checked: None, unchecked: None })), ..Default::default() }).unwrap();
    let inverse = w.apply(&op, &ACTOR).unwrap().inverse.unwrap();
    assert_eq!(w.tab(1).unwrap().value_at(0, 0), Value::Bool(false));
    set(&mut w, "A1", "TRUE");
    assert_eq!(w.tab(1).unwrap().value_at(0, 1), Value::Number(10.0));
    let mut back = snapshot::decode(&snapshot::encode(&w)).unwrap();
    assert_eq!(style(&back, "A1").control, style(&w, "A1").control);
    let clear = back.prepare_clear(1, range("A1"), ClearWhat::Formats).unwrap();
    back.apply(&clear, &ACTOR).unwrap();
    assert!(style(&back, "A1").control.is_some());
    back.apply(&inverse, &ACTOR).unwrap();
    assert_eq!(back.tab(1).unwrap().value_at(0, 0), Value::Empty);
    assert!(style(&back, "A1").control.is_none());
}

#[test]
fn custom_values_and_plain_text_keep_their_literal_type() {
    let mut w = Workbook::new();
    let control = CellControl::Checkbox { checked: Some("Approved".into()), unchecked: Some("Waiting".into()) };
    let op = w.prepare_style(1, range("A1"), StylePatch { control: Some(Some(control)), ..Default::default() }).unwrap();
    w.apply(&op, &ACTOR).unwrap();
    assert_eq!(w.tab(1).unwrap().value_at(0,0), Value::from("Waiting"));
    let op = w.prepare_style(1, range("B1:B3"), StylePatch { num_fmt: Some(Some("@".into())), ..Default::default() }).unwrap();
    w.apply(&op, &ACTOR).unwrap();
    for (a, v) in [("B1", "00123"), ("B2", "=1+2"), ("B3", "TRUE")] {
        set(&mut w, a, v);
        let cell = w.tab(1).unwrap().cell_at(CellPos::parse(a).unwrap());
        assert_eq!(cell.value, Value::from(v));
        assert!(cell.formula.is_none());
    }
}

#[test]
fn standard_and_custom_formats_render_correctly() {
    for (value, code, expected) in [
        (1.5, "# ?/?", "1 1/2"), (0.125, "# ??/??", "1/8"), (-2.25, "# ?/?", "-2 1/4"),
        (0.625, "# ?/8", "5/8"), (1.5, "??/??", "3/2"), (0.0, "# ?/?", "0"),
        (1.5, "[h]:mm:ss", "36:00:00"), (1.5, "[m]", "2160"), (-0.5, "[h]:mm:ss", "-12:00:00"),
        (-12.5, "#,##0.00;(#,##0.00);\"–\"", "(12.50)"),
        (0.0, "#,##0.00;(#,##0.00);\"–\"", "–"),
        (1234.5, "\"₹\"#,##0.00", "₹1,234.50"), (1234.5, "0.00E+00", "1.23E+03"),
        (2.0, "0;0;;@", "2"), (0.0, "0;0;;@", ""), (123.4, "@", "123.4"),
        (1.2, "0.??", "1.2 "),
    ] { assert_eq!(format_number(value, code).0, expected, "{value}: {code}"); }
    assert_eq!(format_number(-1.0, "[Red][<0]0;[Blue]0").1, Some("#C00000"));
    assert_eq!(format_number(1.0, "[Red][<0]0;[Blue]0").1, Some("#1F5FB8"));
}

#[test]
fn excel_roundtrip_keeps_checkbox_dropdown_and_text_rotation() {
    let mut w = Workbook::new();
    for (a, patch) in [
        ("A1", StylePatch { control: Some(Some(CellControl::Checkbox { checked: None, unchecked: None })), ..Default::default() }),
        ("B1", StylePatch { control: Some(Some(CellControl::Dropdown { options: vec!["Not started".into(), "Done".into()] })), ..Default::default() }),
        ("D1", StylePatch { control: Some(Some(CellControl::Rating)), ..Default::default() }),
        ("E1", StylePatch { control: Some(Some(CellControl::Person)), ..Default::default() }),
        ("F1", StylePatch { control: Some(Some(CellControl::File)), ..Default::default() }),
        ("C1", StylePatch { rotation: Some(-45), ..Default::default() }),
    ] {
        let op = w.prepare_style(1, range(a), patch).unwrap();
        w.apply(&op, &ACTOR).unwrap();
    }
    set(&mut w, "A1", "TRUE"); set(&mut w, "B1", "Done"); set(&mut w, "C1", "Tilted");
    let bytes = formats::export(&w, Format::Xlsx, &ExportOptions::default()).unwrap();
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    let mut back = Workbook::empty();
    back.apply(&imported.plan(&back, None).op, &ACTOR).unwrap();
    for a in ["A1", "B1", "C1", "D1", "E1", "F1"] { assert_eq!(style(&back, a), style(&w, a), "{a}"); }
    assert_eq!(back.tab(1).unwrap().value_at(0, 0), Value::Bool(true));
    // An ordinary Excel file has only native checkbox and validation metadata.
    use std::io::{Cursor, Read, Write};
    let mut source = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
    let mut native = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for i in 0..source.len() {
        let mut entry = source.by_index(i).unwrap();
        if entry.name().contains("silicon-calc") { continue; }
        let mut data = Vec::new(); entry.read_to_end(&mut data).unwrap();
        native.start_file(entry.name(), zip::write::SimpleFileOptions::default()).unwrap();
        native.write_all(&data).unwrap();
    }
    let bytes = native.finish().unwrap().into_inner();
    let imported = formats::import(Format::Xlsx, &bytes, &ImportOptions::default()).unwrap();
    let mut native_back = Workbook::empty();
    native_back.apply(&imported.plan(&native_back, None).op, &ACTOR).unwrap();
    for a in ["A1", "B1"] { assert_eq!(style(&native_back,a).control, style(&w,a).control, "native {a}"); }
}

#[test]
fn invalid_control_rules_and_angles_are_rejected() {
    let w = Workbook::new();
    for patch in [
        StylePatch { control: Some(Some(CellControl::Dropdown { options: vec![] })), ..Default::default() },
        StylePatch { control: Some(Some(CellControl::Checkbox { checked: Some("same".into()), unchecked: Some("same".into()) })), ..Default::default() },
        StylePatch { rotation: Some(180), ..Default::default() },
    ] { assert!(w.prepare_style(1, range("A1"), patch).is_err()); }
}
