//! Files: `download` a sheet (or one tab, at any version) in any format Silicon Calc writes. Opening files is
//! `import` (a new sheet) and `tabs import` (new tabs, or one tab's contents replaced).

use std::path::{Path, PathBuf};

use serde_json::json;
use silicon_calc_client::{DownloadQuery, Format, TabRef};

use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::resolve;

pub struct DownloadArgs {
    pub sheet: String,
    pub copy: Option<String>,
    pub format: Option<String>,
    pub output: Option<String>,
    pub tab: Option<String>,
    pub version: Option<String>,
    pub formulas: bool,
    pub force: bool,
}

pub fn parse_format(s: &str) -> CliResult<Format> {
    Format::parse(s).ok_or_else(|| {
        CliError::usage(
            format!("\"{s}\" is not a format Silicon Calc writes."),
            "Use xlsx, ods, pdf, html, csv, tsv, xls, numbers, json or parquet.",
        )
    })
}

/// A file name the service suggested, safe to create in a folder.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '\0' | ':') || c.is_control() { '-' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    if cleaned.is_empty() { "sheet".to_owned() } else { cleaned }
}

/// The format: `--format`, else the output's extension, else xlsx.
fn pick_format(format: Option<&str>, output: Option<&str>) -> CliResult<Format> {
    if let Some(f) = format {
        return parse_format(f);
    }
    if let Some(out) = output.filter(|o| *o != "-")
        && !Path::new(out).is_dir()
        && let Some(f) = Format::from_file_name(out)
    {
        return Ok(f);
    }
    Ok(Format::Xlsx)
}

pub async fn download(ctx: &Ctx, a: DownloadArgs) -> CliResult<()> {
    let format = pick_format(a.format.as_deref(), a.output.as_deref())?;
    if a.formulas && format != Format::Json {
        return Err(CliError::usage(
            "--formulas works with --format json. Spreadsheet downloads keep compatible formulas and use calculated values for JavaScript.",
            "Add --format json, or drop --formulas.",
        ));
    }
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let version = match &a.version {
        Some(v) => Some(resolve::version(ctx, &target.line, v, target.version).await?),
        None => None,
    };
    let tab = match &a.tab {
        Some(t) => {
            let tabs = match version {
                Some(v) if v != target.version => ctx.client.tabs_at(&target.line, v).await?.tabs,
                _ => target.tabs.clone(),
            };
            Some(TabRef::Id(resolve::find_tab(&tabs, Some(t), &target.place())?.id))
        }
        None => None,
    };
    let query = DownloadQuery { format, tab, version, formulas: a.formulas };
    let download = ctx.client.download(&target.line, &query).await?;
    let suggested = safe_file_name(&download.file_name.clone().unwrap_or_else(|| format!("{}.{}", target.sheet.title, format.ext())));
    let to_stdout = a.output.as_deref() == Some("-");
    if to_stdout {
        let mut out = tokio::io::stdout();
        let bytes = download.write_to(&mut out).await?;
        ctx.note(format!("Wrote {} of {} to stdout.", size(bytes), format.ext()));
        return Ok(());
    }
    let path: PathBuf = match a.output.as_deref() {
        Some(o) if Path::new(o).is_dir() => Path::new(o).join(&suggested),
        Some(o) => PathBuf::from(o),
        None => PathBuf::from(&suggested),
    };
    if path.exists() && !a.force {
        return Err(CliError::usage_code(
            "exists",
            format!("{} already exists, so nothing was downloaded.", path.display()),
            "Pass --force to replace it, or -o another path.",
        ));
    }
    let bytes = download.save(&path).await?;
    let shown_version = version.unwrap_or(target.version);
    ctx.emit(
        &json!({"path": path.display().to_string(), "bytes": bytes, "format": format.ext(), "sheet": target.sheet.id, "copy": target.copy.as_ref().map(|c| c.id.clone()), "version": shown_version}),
        || format!("Saved {} ({}) from {} at version {shown_version}.", path.display(), size(bytes), target.place()),
    );
    Ok(())
}

/// `12.3 KB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_names() {
        assert_eq!(pick_format(None, None).unwrap(), Format::Xlsx);
        assert_eq!(pick_format(None, Some("out.csv")).unwrap(), Format::Csv);
        assert_eq!(pick_format(Some("pdf"), Some("out.csv")).unwrap(), Format::Pdf);
        assert_eq!(pick_format(None, Some("-")).unwrap(), Format::Xlsx);
        assert!(pick_format(Some("docx"), None).is_err());
        assert_eq!(safe_file_name("Q3/Q4 revenue.xlsx"), "Q3-Q4 revenue.xlsx");
        assert_eq!(safe_file_name("..hidden"), "hidden");
        assert_eq!(safe_file_name(""), "sheet");
        assert_eq!(size(512), "512 bytes");
        assert_eq!(size(12_595), "12.3 KB");
    }
}
