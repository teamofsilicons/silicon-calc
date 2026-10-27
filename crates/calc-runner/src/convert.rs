use crate::{
    RunError, RunOutput, Runner, SheetSource,
    duck::{Duck, Settings, quote_literal},
    script,
};
use calc_engine::{
    CellPos, Workbook,
    formats::{self, ExportOptions, Format, ImportOptions, Imported},
};
use serde_json::json;

pub const NUMBERS_MAX_CELLS: u64 = 2_000_000;
pub const XLS_MAX_ROWS: u32 = 65_536;
pub const XLS_MAX_COLS: u32 = 256;

impl Runner {
    /// Reads supported formats. Native Numbers automation is an explicit desktop-only opt-in.
    pub fn import_file(
        &self,
        format: Format,
        bytes: &[u8],
        options: &ImportOptions,
    ) -> Result<Imported, RunError> {
        #[cfg(target_os = "macos")]
        if format == Format::Numbers && native_numbers_enabled() {
            let converted = self.numbers(bytes, true)?;
            return formats::import(Format::Xlsx, &converted, options).map_err(format_error);
        }
        if matches!(format, Format::Ods | Format::Xls) {
            let converted = self.office(bytes, format.extension(), "xlsx")?;
            let mut imported = formats::import(Format::Xlsx, &converted, options).map_err(format_error)?;
            if format == Format::Ods && imported.source_tabs.is_empty() && let Some(xml) = formats::programs::read_package(bytes) {
                formats::programs::restore(&xml, &mut imported);
            }
            return Ok(imported);
        }
        if format != Format::Parquet && format != Format::Numbers {
            return formats::import(format, bytes, options).map_err(format_error);
        }
        let scratch = self.scratch()?;
        let input = scratch.path.join(format!("input.{}", format.extension()));
        std::fs::write(&input, bytes)?;
        if format == Format::Parquet {
            let db = Duck::open(&Settings {
                files: true,
                threads: self.cfg.limits.threads,
                memory_bytes: self.cfg.limits.memory_bytes,
                spill_dir: &scratch.path.join("tmp"),
            })?;
            let (columns, row_count) = db.query(
                &format!(
                    "SELECT * FROM read_parquet({})",
                    quote_literal(&input.to_string_lossy())
                ),
                None,
                self.cfg.limits.result_cells,
                self.cfg.limits.timeout,
            )?;
            let output = RunOutput {
                columns,
                row_count,
                has_result: true,
                ..Default::default()
            };
            return Ok(Imported {
                tabs: vec![("Data".into(), output.tab_content(None))],
                ..Default::default()
            });
        }
        self.conversion(&scratch.path, "read_numbers")?;
        formats::import(
            Format::Json,
            &std::fs::read(scratch.path.join("output.json"))?,
            options,
        )
        .map_err(format_error)
    }

    /// Writes a workbook in any supported format.
    pub fn export_file(
        &self,
        wb: &Workbook,
        format: Format,
        options: &ExportOptions,
    ) -> Result<Vec<u8>, RunError> {
        formats::programs::ensure_calculated(wb, format, options).map_err(format_error)?;
        for tab in wb
            .tabs
            .iter()
            .filter(|t| options.tab.is_none_or(|id| id == t.id))
        {
            if let Some(rect) = tab.used_rect() {
                if format == Format::Xls && (rect.r2 >= XLS_MAX_ROWS || rect.c2 >= XLS_MAX_COLS) {
                    return Err(RunError::too_large(
                        "XLS supports at most 65,536 rows and 256 columns.",
                        "Download XLSX or Parquet instead.",
                    ));
                }
                if format == Format::Numbers
                    && u64::from(rect.r2 + 1) * u64::from(rect.c2 + 1) > NUMBERS_MAX_CELLS
                {
                    return Err(RunError::too_large(
                        "This tab is too large for the Numbers converter.",
                        "Download XLSX or Parquet instead.",
                    ));
                }
            }
        }
        #[cfg(target_os = "macos")]
        if format == Format::Numbers && native_numbers_enabled() {
            let xlsx = formats::export(wb, Format::Xlsx, options).map_err(format_error)?;
            return self.numbers(&xlsx, false);
        }
        if matches!(format, Format::Ods | Format::Pdf | Format::Xls) {
            let xlsx = formats::export(wb, Format::Xlsx, options).map_err(format_error)?;
            let converted = self.office(&xlsx, "xlsx", format.extension())?;
            if format == Format::Ods {
                return formats::programs::append_package(converted, formats::programs::read_package(&xlsx).as_deref(), true).map_err(format_error);
            }
            return Ok(converted);
        }
        if !matches!(format, Format::Parquet | Format::Numbers | Format::Xls) {
            return formats::export(wb, format, options).map_err(format_error);
        }
        let scratch = self.scratch()?;
        let tabs: Vec<_> = wb
            .tabs
            .iter()
            .filter(|t| options.tab.is_none_or(|id| id == t.id))
            .collect();
        if tabs.is_empty() {
            return Err(RunError::failed(
                "There is no tab to download.",
                "Choose an existing tab.",
            ));
        }
        if format == Format::Parquet {
            let table = wb.table(&tabs[0].name).ok_or_else(|| {
                RunError::failed("This tab has no columns.", "Add a header row first.")
            })?;
            let mut db = Duck::open(&Settings {
                files: true,
                threads: self.cfg.limits.threads,
                memory_bytes: self.cfg.limits.memory_bytes,
                spill_dir: &scratch.path.join("tmp"),
            })?;
            db.register_as("data", &table)?;
            db.write_parquet("data", &scratch.path.join("output.parquet"))?;
            return Ok(std::fs::read(scratch.path.join("output.parquet"))?);
        }
        let mut payload = Vec::new();
        for t in tabs {
            let rect = t.used_rect();
            let (rows, cols) = rect.map(|r| (r.r2 + 1, r.c2 + 1)).unwrap_or((1, 1));
            if format == Format::Xls && (rows > XLS_MAX_ROWS || cols > XLS_MAX_COLS) {
                return Err(RunError::too_large(
                    "XLS supports at most 65,536 rows and 256 columns.",
                    "Download XLSX or Parquet instead.",
                ));
            }
            if format == Format::Numbers && u64::from(rows) * u64::from(cols) > NUMBERS_MAX_CELLS {
                return Err(RunError::too_large(
                    "This tab is too large for the Numbers converter.",
                    "Download XLSX or Parquet instead.",
                ));
            }
            let mut cells = Vec::new();
            for r in 0..rows {
                let mut row = Vec::new();
                for c in 0..cols {
                    let cell = t.cell_at(CellPos::new(r, c));
                    row.push(json!({"v": crate::values::answer_json(&cell.value, None), "f": if options.values_only {None} else {wb.formula_text(t.id,r,c)}, "style": wb.style_of(cell.style)}));
                }
                cells.push(row);
            }
            payload.push(json!({"name":t.name, "rows":cells}));
        }
        std::fs::write(
            scratch.path.join("input.json"),
            serde_json::to_vec(&json!({"tabs":payload})).map_err(RunError::internal)?,
        )?;
        self.conversion(
            &scratch.path,
            if format == Format::Xls {
                "write_xls"
            } else {
                "write_numbers"
            },
        )?;
        Ok(std::fs::read(
            scratch.path.join(format!("output.{}", format.extension())),
        )?)
    }

    fn conversion(&self, scratch: &std::path::Path, operation: &str) -> Result<(), RunError> {
        std::fs::write(
            scratch.join("convert.py"),
            include_str!("../../../runner/convert.py"),
        )?;
        let output = script::execute(
            self,
            scratch,
            vec![
                "-I".into(),
                "-B".into(),
                "convert.py".into(),
                operation.into(),
            ],
            &mut |_| b"{\"ok\":false}".to_vec(),
        )?;
        if !output.success() {
            return Err(RunError::failed("The file conversion could not finish.", "Check the file and the converter details; try XLSX if the format cannot hold this content.").with_stderr(output.stderr));
        }
        Ok(())
    }

    /// Converts with a fresh LibreOffice profile, inside the OS sandbox, with macros disabled.
    fn office(&self, bytes: &[u8], input_ext: &str, output_ext: &str) -> Result<Vec<u8>, RunError> {
        use std::path::PathBuf;
        let configured = std::env::var_os("CALC_RUNNER_LIBREOFFICE").map(PathBuf::from);
        let program = configured
            .or_else(|| {
                [
                    "/Applications/LibreOffice.app/Contents/MacOS/soffice",
                    "/usr/bin/soffice",
                    "/usr/bin/libreoffice",
                ]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
            })
            .ok_or_else(|| {
                RunError::unavailable(
                    "LibreOffice is needed to open ODS and download ODS or PDF.",
                    "Install LibreOffice or set CALC_RUNNER_LIBREOFFICE to its soffice executable.",
                )
            })?;
        let program = program.canonicalize()?;
        // LibreOffice's UNIX IPC socket name includes a hash and must fit in sockaddr_un (104 bytes on macOS).
        let scratch = crate::Scratch::new(std::path::Path::new("/tmp/sc"))?;
        let input = scratch.path.join(format!("input.{input_ext}"));
        std::fs::write(&input, bytes)?;
        let output = scratch.path.join("output");
        let profile = scratch.path.join("profile");
        std::fs::create_dir_all(&output)?;
        std::fs::create_dir_all(profile.join("user"))?;
        std::fs::write(
            profile.join("user/registrymodifications.xcu"),
            r#"<?xml version="1.0"?><oor:items xmlns:oor="http://openoffice.org/2001/registry"><item oor:path="/org.openoffice.Office.Common/Security/Scripting"><prop oor:name="MacroSecurityLevel" oor:op="fuse"><value>3</value></prop></item><item oor:path="/org.openoffice.Office.Calc/Content/Update"><prop oor:name="Link" oor:op="fuse"><value>2</value></prop></item></oor:items>"#,
        )?;
        let root = if cfg!(target_os = "macos") {
            program.ancestors().nth(3).unwrap_or(&program).to_owned()
        } else {
            program.parent().unwrap_or(&program).to_owned()
        };
        let spawn = crate::sandbox::Spawn {
            program: program.clone(),
            args: vec![
                format!("-env:UserInstallation=file://{}", profile.display()).into(),
                format!("-env:OSL_SOCKET_PATH={}", scratch.path.display()).into(),
                "--headless".into(),
                "--nologo".into(),
                "--nodefault".into(),
                "--norestore".into(),
                "--convert-to".into(),
                output_ext.into(),
                "--outdir".into(),
                output.clone().into(),
                input.into(),
            ],
            env: vec![
                ("HOME".into(), scratch.path.to_string_lossy().into()),
                (
                    "TMPDIR".into(),
                    scratch.path.join("tmp").to_string_lossy().into(),
                ),
                ("SAL_USE_VCLPLUGIN".into(), "svp".into()),
                ("LANG".into(), "en_US.UTF-8".into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
            scratch: &scratch.path,
            read: office_read_paths(root),
            exec: vec![program],
            limits: &self.cfg.limits,
            address_space: false,
            desktop_converter: true,
        };
        let finished = crate::sandbox::run(&self.sandbox()?, &spawn, &mut |_| b"{}".to_vec())?;
        let path = output.join(format!("input.{output_ext}"));
        if !finished.success() || !path.is_file() {
            return Err(RunError::failed(format!("LibreOffice could not convert this file (exit {:?}, signal {:?}, stopped {:?}).",finished.code,finished.signal,finished.stop),"Check the converter details and that LibreOffice can open the file.").with_output(finished.stdout,finished.stderr));
        }
        let converted = std::fs::read(path)?;
        if converted.is_empty() {
            return Err(RunError::failed(
                "LibreOffice returned an empty file.",
                "Try XLSX and check the converter installation.",
            ));
        }
        Ok(converted)
    }

    #[cfg(target_os = "macos")]
    fn numbers(&self, bytes: &[u8], opening: bool) -> Result<Vec<u8>, RunError> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _serial = SERIAL.lock().map_err(RunError::internal)?;
        let scratch = self.scratch()?;
        let input = scratch.path.join(if opening {
            "input.numbers"
        } else {
            "input.xlsx"
        });
        let output = scratch.path.join(if opening {
            "output.xlsx"
        } else {
            "output.numbers"
        });
        std::fs::write(&input, bytes)?;
        std::fs::write(
            scratch.path.join("numbers.applescript"),
            include_str!("../../../runner/numbers.applescript"),
        )?;
        // AppleEvents can report -600 for a closed Numbers app without launching it.
        // LaunchServices starts the explicitly opted-in desktop app first; the script
        // retries document events briefly while a cold launch finishes.
        let launch = crate::sandbox::Spawn {
            program: "/usr/bin/open".into(),
            args: vec!["-g".into(), "-a".into(), "/Applications/Numbers.app".into()],
            env: vec![],
            scratch: &scratch.path,
            read: vec![],
            exec: vec!["/usr/bin/open".into()],
            limits: &self.cfg.limits,
            address_space: false,
            desktop_converter: true,
        };
        let launched =
            crate::sandbox::run(&crate::sandbox::Kind::Off, &launch, &mut |_| b"{}".to_vec())?;
        if !launched.success() {
            return Err(RunError::unavailable(
                "Apple Numbers could not start.",
                "Install Numbers on this desktop or download XLSX.",
            )
            .with_output(launched.stdout, launched.stderr));
        }
        let spawn = crate::sandbox::Spawn {
            program: "/usr/bin/osascript".into(),
            args: vec![
                scratch.path.join("numbers.applescript").into(),
                input.into(),
                output.clone().into(),
                if opening {
                    "open".into()
                } else {
                    "save".into()
                },
            ],
            env: vec![
                ("HOME".into(), scratch.path.to_string_lossy().into_owned()),
                (
                    "TMPDIR".into(),
                    scratch.path.join("tmp").to_string_lossy().into_owned(),
                ),
            ],
            scratch: &scratch.path,
            read: vec!["/Applications/Numbers.app".into(), "/System".into()],
            exec: vec!["/usr/bin/osascript".into()],
            limits: &self.cfg.limits,
            address_space: false,
            desktop_converter: true,
        };
        // AppleEvents cannot grant a native app access to its input from a restricted launcher.
        // This fixed script bridge is opt-in: Numbers has its own app permissions and is NOT
        // confined by our child sandbox, process group, memory or timeout watchdog.
        let finished =
            crate::sandbox::run(&crate::sandbox::Kind::Off, &spawn, &mut |_| b"{}".to_vec())?;
        if !finished.success() || !output.is_file() {
            return Err(RunError::unavailable("Apple Numbers could not convert this file.","Install Numbers and allow the service to control Numbers in macOS Automation settings, or download XLSX.").with_output(finished.stdout,finished.stderr));
        }
        Ok(std::fs::read(output)?)
    }
}

fn office_read_paths(root: std::path::PathBuf) -> Vec<std::path::PathBuf> {
    // Ubuntu's /usr/lib/libreoffice/share registry and printer configuration
    // link into /etc/libreoffice. These paths are read-only; bwrap tolerates
    // their absence on other distributions with --ro-bind-try.
    let mut paths = vec![root];
    paths.extend(
        [
            "/etc/libreoffice",
            "/etc/fonts",
            "/usr/share/fonts",
            "/var/cache/fontconfig",
        ]
        .into_iter()
        .map(std::path::PathBuf::from),
    );
    paths
}

fn format_error(error: formats::FormatError) -> RunError {
    RunError::failed(error.to_string(), error.hint())
}

#[cfg(target_os = "macos")]
fn native_numbers_enabled() -> bool {
    std::env::var("CALC_RUNNER_NUMBERS_ENABLE").is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::office_read_paths;
    use crate::{Limits, sandbox};
    use std::path::Path;

    #[test]
    fn linux_office_configuration_is_read_only_and_scratch_is_only_writable_bind() {
        let limits = Limits::default();
        let scratch = Path::new("/tmp/sc/office-mount-test");
        let spawn = sandbox::Spawn {
            program: "/usr/lib/libreoffice/program/soffice".into(),
            args: vec![],
            env: vec![],
            scratch,
            read: office_read_paths("/usr/lib/libreoffice/program".into()),
            exec: vec![],
            limits: &limits,
            address_space: false,
            desktop_converter: true,
        };
        let args: Vec<_> = sandbox::bwrap_args(&spawn)
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.windows(3)
                .any(|a| { a == ["--ro-bind-try", "/etc/libreoffice", "/etc/libreoffice"] })
        );
        assert!(!args.iter().any(|a| a == "/etc"));
        let writable: Vec<_> = args.windows(3).filter(|a| a[0] == "--bind").collect();
        assert_eq!(writable.len(), 1);
        assert_eq!(writable[0][1], scratch.to_str().unwrap());
        assert_eq!(writable[0][2], scratch.to_str().unwrap());
        assert!(args.iter().any(|a| a == "--unshare-all"));
    }
}
