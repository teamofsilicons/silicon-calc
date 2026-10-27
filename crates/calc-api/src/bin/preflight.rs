//! Runs the actual trusted converters before a release starts serving requests.

use anyhow::{Context, ensure};
use calc_engine::{
    CellPos, Value, Workbook,
    formats::{self, ExportOptions, Format, ImportOptions},
    ops::Actor,
};
use calc_runner::{RunError, Runner, RunnerConfig, SandboxMode};

fn conversion<T>(result: Result<T, RunError>, format: Format) -> anyhow::Result<T> {
    result.map_err(|e| anyhow::anyhow!("{format:?}: {} {}\n{}", e.message, e.hint, e.stderr))
}

fn main() -> anyhow::Result<()> {
    let sandbox = std::env::var("CALC_CODE_SANDBOX").unwrap_or_else(|_| "auto".into());
    let mode = SandboxMode::parse(&sandbox).context("Unknown CALC_CODE_SANDBOX setting")?;
    ensure!(mode != SandboxMode::Off, "Deployment preflight requires converter confinement.");
    ensure!(
        std::env::var("CALC_RUNNER_NUMBERS_ENABLE").as_deref() != Ok("1"),
        "Native Numbers is for local desktop use; disable it for this service."
    );
    let python = std::env::var("CALC_RUNNER_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut cfg = RunnerConfig::new(python, mode);
    cfg.limits.threads = 2;
    let runner = Runner::new(cfg);
    let imported = formats::import(
        Format::Csv,
        b"name,amount\ncheck,12.5\n",
        &ImportOptions::default(),
    )?;
    let mut workbook = Workbook::empty();
    workbook.apply(&imported.plan(&workbook, None).op, &Actor { account: "preflight", at: 0 })?;
    for format in [Format::Xlsx, Format::Parquet, Format::Xls, Format::Ods, Format::Numbers, Format::Pdf] {
        let bytes = conversion(runner.export_file(&workbook, format, &ExportOptions::default()), format)?;
        ensure!(!bytes.is_empty(), "{format:?} export is empty");
        if format == Format::Pdf {
            ensure!(bytes.starts_with(b"%PDF-"), "PDF export has an invalid signature");
        } else {
            let imported = conversion(runner.import_file(format, &bytes, &ImportOptions::default()), format)?;
            let mut roundtrip = Workbook::empty();
            roundtrip.apply(&imported.plan(&roundtrip, None).op, &Actor { account: "preflight", at: 0 })?;
            let actual = roundtrip.tabs.first().map(|tab| tab.cell_at(CellPos::new(1, 1)).value);
            ensure!(
                roundtrip.tabs.len() == 1 && matches!(actual, Some(Value::Number(n)) if (n - 12.5).abs() < 1e-12),
                "{format:?} conversion changed the test table: {} tabs, B2={actual:?}", roundtrip.tabs.len()
            );
        }
        println!("{format:?}: ok");
    }
    println!("Converters and their OS sandbox are ready.");
    Ok(())
}
