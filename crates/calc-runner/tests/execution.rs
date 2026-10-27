use calc_engine::{
    Value, Workbook,
    formats::{self, ExportOptions, Format, ImportOptions},
    ops::Actor,
};
use calc_runner::{Runner, RunnerConfig, SandboxMode};
use std::path::Path;
fn runner() -> Runner {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut cfg = RunnerConfig::new(root.join(".dev/runner-venv/bin/python"), SandboxMode::Auto);
    cfg.limits.threads = 2;
    Runner::new(cfg)
}
fn workbook() -> Workbook {
    let imported = formats::import(
        Format::Csv,
        b"region,amount\nEU,10\nUS,20\nEU,15\n",
        &ImportOptions::default(),
    )
    .unwrap();
    let mut wb = Workbook::empty();
    let mut imported = imported;
    imported.tabs[0].0 = "Sales".into();
    wb.apply(
        &imported.plan(&wb, None).op,
        &Actor {
            account: "test",
            at: 1,
        },
    )
    .unwrap();
    wb
}
#[test]
fn parquet_xls_and_numbers_round_trip_values() {
    let runner = runner();
    let wb = workbook();
    for format in [Format::Parquet, Format::Xls, Format::Numbers] {
        let bytes = runner
            .export_file(&wb, format, &ExportOptions::default())
            .unwrap();
        assert!(!bytes.is_empty());
        let imported = runner
            .import_file(format, &bytes, &ImportOptions::default())
            .unwrap();
        assert_eq!(
            imported.tabs[0].1.columns[1].values,
            vec![
                Value::text("amount"),
                Value::Number(10.),
                Value::Number(20.),
                Value::Number(15.)
            ],
            "{format:?}"
        );
    }
}
#[test]
fn ods_and_pdf_use_real_office_conversion() {
    let runner = runner();
    let wb = workbook();
    let ods = runner
        .export_file(&wb, Format::Ods, &ExportOptions::default())
        .unwrap();
    let opened = runner
        .import_file(Format::Ods, &ods, &ImportOptions::default())
        .unwrap();
    assert_eq!(opened.tabs[0].1.columns[1].values[1], Value::Number(10.));
    let pdf = runner
        .export_file(&wb, Format::Pdf, &ExportOptions::default())
        .unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
}

#[test]
fn spreadsheet_conversions_preserve_formulas_and_styles() {
    use calc_engine::{CellPos, ops::CellWrite, style::Style};
    let runner = runner();
    let mut wb = workbook();
    let id = wb.tabs[0].id;
    let op = wb
        .prepare_writes(
            id,
            vec![
                (
                    1,
                    2,
                    CellWrite {
                        input: Some("=SUM(B2:B4)".into()),
                        ..Default::default()
                    },
                ),
                (
                    0,
                    0,
                    CellWrite {
                        style: Some(Style {
                            bold: true,
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                ),
            ],
        )
        .unwrap();
    wb.apply(
        &op,
        &Actor {
            account: "test",
            at: 2,
        },
    )
    .unwrap();
    let targets = wb.add_tab("Targets", None);
    let op = wb
        .prepare_writes(
            targets,
            vec![(
                0,
                0,
                CellWrite {
                    input: Some("target".into()),
                    ..Default::default()
                },
            )],
        )
        .unwrap();
    wb.apply(
        &op,
        &Actor {
            account: "test",
            at: 2,
        },
    )
    .unwrap();
    let mut formats = vec![Format::Xls, Format::Ods];
    if std::env::var("CALC_RUNNER_NUMBERS_ENABLE").is_ok_and(|v| v == "1") {
        formats.push(Format::Numbers);
    }
    for format in formats {
        let bytes = runner
            .export_file(&wb, format, &ExportOptions::default())
            .unwrap();
        let imported = runner
            .import_file(format, &bytes, &ImportOptions::default())
            .unwrap();
        assert_eq!(
            imported
                .tabs
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            vec!["Sales", "Targets"],
            "{format:?}"
        );
        let mut opened = Workbook::empty();
        opened
            .apply(
                &imported.plan(&opened, None).op,
                &Actor {
                    account: "test",
                    at: 3,
                },
            )
            .unwrap();
        let tab = &opened.tabs[0];
        assert_eq!(
            tab.cell_at(CellPos::new(1, 2)).value,
            Value::Number(45.),
            "{format:?}"
        );
        assert!(opened.formula_text(tab.id, 1, 2).is_some(), "{format:?}");
        assert!(
            opened.style_of(tab.cell_at(CellPos::new(0, 0)).style).bold,
            "{format:?}"
        );
    }
}

#[test]
fn ods_preserves_portable_javascript_values_and_direct_reimport_source() {
    use calc_engine::{
        Op,
        javascript::{Answer, Function},
        ops::CellWrite,
    };
    let runner = runner();
    let actor = Actor {
        account: "c:export-test",
        at: 1,
    };
    let mut wb = workbook();
    let id = wb.tabs[0].id;
    wb.apply(
        &Op::SetFunctions {
            functions: vec![Function {
                name: "DOUBLE_VALUE".into(),
                parameters: vec!["v".into()],
                code: "return v*2;".into(),
                account: None,
            }],
        },
        &actor,
    )
    .unwrap();
    let op = wb
        .prepare_writes(
            id,
            vec![
                (
                    1,
                    2,
                    CellWrite {
                        input: Some("=DOUBLE_VALUE(B2)".into()),
                        ..Default::default()
                    },
                ),
                (
                    1,
                    3,
                    CellWrite {
                        input: Some("=C2+1".into()),
                        ..Default::default()
                    },
                ),
                (
                    1,
                    4,
                    CellWrite {
                        input: Some("=JS(\"return [[1,2],[3,4]];\")".into()),
                        ..Default::default()
                    },
                ),
                (
                    1,
                    6,
                    CellWrite {
                        input: Some("=SUM(E2#)".into()),
                        ..Default::default()
                    },
                ),
                (
                    5,
                    0,
                    CellWrite {
                        input: Some("=JS(\"return 1/3;\")".into()),
                        ..Default::default()
                    },
                ),
            ],
        )
        .unwrap();
    wb.apply(&op, &actor).unwrap();
    let js = wb.javascript.lock().unwrap().clone();
    let results = js
        .calls
        .iter()
        .filter(|c| c.result.is_none())
        .map(|c| {
            (
                c.id,
                Answer {
                    rows: if c.row == 5 {
                        vec![vec![Value::Number(1. / 3.)]]
                    } else if c.name == "JS" {
                        vec![
                            vec![Value::Number(1.), Value::Number(2.)],
                            vec![Value::Number(3.), Value::Number(4.)],
                        ]
                    } else {
                        vec![vec![Value::Number(20.)]]
                    },
                    error: None,
                    dependencies: None,
                },
            )
        })
        .collect();
    wb.apply(
        &Op::JavascriptResults {
            epoch: js.epoch,
            results,
        },
        &actor,
    )
    .unwrap();
    let bytes = runner
        .export_file(&wb, Format::Ods, &ExportOptions::default())
        .unwrap();
    assert!(formats::programs::read_package(&bytes).is_some());
    let imported = runner
        .import_file(Format::Ods, &bytes, &ImportOptions::default())
        .unwrap();
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);
    assert_eq!(imported.functions, wb.functions);
    assert_eq!(
        imported.tabs[0].1.columns[0].formulas,
        vec![(5, "JS(\"return 1/3;\")".into())]
    );
    assert_eq!(
        imported.tabs[0].1.columns[2].formulas,
        vec![(1, "DOUBLE_VALUE(B2)".into())]
    );
    assert_eq!(
        imported.tabs[0].1.columns[6].formulas,
        vec![(1, "SUM(E2#)".into())]
    );
    let values = runner
        .export_file(
            &wb,
            Format::Ods,
            &ExportOptions {
                values_only: true,
                ..Default::default()
            },
        )
        .unwrap();
    let opened = runner
        .import_file(Format::Ods, &values, &ImportOptions::default())
        .unwrap();
    assert!(opened.functions.is_empty());
    assert_eq!(opened.tabs[0].1.columns[2].values[1], Value::Number(20.));
    assert_eq!(opened.tabs[0].1.columns[3].values[1], Value::Number(21.));
    assert_eq!(opened.tabs[0].1.columns[6].values[1], Value::Number(10.));
}
