//! Trusted file conversion helpers. User commands run only in the browser.
//!
//! Parquet uses an isolated DuckDB database per conversion. LibreOffice and the
//! fixed Python converter run with OS sandbox and resource limits. Native Numbers
//! automation is a separate, explicit desktop-only opt-in; it is not OS confined.
//! These blocking helpers are called on a service blocking thread.

mod convert;
mod duck;
mod error;
mod sandbox;
mod script;
mod source;
pub mod table;
pub mod values;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use calc_engine::{
    TabId, Value,
    model::CodeSource,
    ops::{CellWrite, ColumnContent, TabContent},
    style::{Style, StylePatch},
};

pub use convert::{NUMBERS_MAX_CELLS, XLS_MAX_COLS, XLS_MAX_ROWS};
pub use error::{RunError, RunErrorKind};
pub use source::{CellRead, SheetSource};
pub use table::{ColumnType, Table};

/// How trusted converter subprocesses are isolated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxMode {
    /// `sandbox-exec` on macOS, `bwrap` on Linux, whichever is there.
    Auto,
    MacOs,
    Bwrap,
    /// No sandbox for converters. For local development only.
    Off,
}

impl SandboxMode {
    /// `auto`, `macos`, `bwrap` or `off` (what `CALC_CODE_SANDBOX` holds).
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Self::Auto,
            "macos" | "sandbox-exec" | "seatbelt" => Self::MacOs,
            "bwrap" | "bubblewrap" | "linux" => Self::Bwrap,
            "off" | "none" | "0" | "false" => Self::Off,
            _ => return None,
        })
    }
}

/// What one run (or conversion) may use.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Wall-clock time, after which the run is stopped.
    pub timeout: Duration,
    /// Memory: DuckDB memory and converter process resident memory.
    pub memory_bytes: u64,
    /// How much of standard output and of standard error is kept, each.
    pub output_bytes: usize,
    /// The most cells a result kept whole (written into a tab) may have.
    pub result_cells: u64,
    /// Threads DuckDB (and numpy, pyarrow) may use.
    pub threads: usize,
    /// The most bytes a converter may keep in its scratch folder.
    pub scratch_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            memory_bytes: 2 << 30,
            output_bytes: 1 << 20,
            result_cells: 20_000_000,
            threads: 4,
            scratch_bytes: 8 << 30,
        }
    }
}

/// How the runner is set up.
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    /// The Python with pandas, numpy, pyarrow, duckdb, numbers-parser and xlwt (`scripts/setup-runner.sh` makes
    /// one at `.dev/runner-venv`).
    pub python: PathBuf,
    pub sandbox: SandboxMode,
    /// Where each run gets its scratch folder (default: the system's temporary folder).
    pub scratch_root: PathBuf,
    pub limits: Limits,
}

impl RunnerConfig {
    /// A configuration with the given interpreters, the default limits and scratch folders in the system's
    /// temporary folder.
    pub fn new(python: impl Into<PathBuf>, sandbox: SandboxMode) -> Self {
        Self {
            python: python.into(),
            sandbox,
            scratch_root: std::env::temp_dir().join("silicon-calc-runs"),
            limits: Limits::default(),
        }
    }
}

/// One column of a result.
#[derive(Clone, Debug, PartialEq)]
pub struct OutColumn {
    pub name: String,
    /// The number format its cells get when written (dates, times), if any.
    pub format: Option<String>,
    pub values: Vec<Value>,
}

/// Materialized tabular data used by converters and browser result commits.
#[derive(Clone, Debug, Default)]
pub struct RunOutput {
    pub columns: Vec<OutColumn>,
    /// How many rows the result has in all (the columns hold fewer when they were cut to `keep_rows`).
    pub row_count: u64,
    /// Whether a tabular result is present.
    pub has_result: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
    /// Input tab metadata, when supplied by a caller.
    pub inputs: Vec<TabId>,
}

impl RunOutput {
    /// How many rows the columns hold.
    pub fn kept_rows(&self) -> usize {
        self.columns.first().map_or(0, |c| c.values.len())
    }

    /// Whether the result has more rows than the columns hold.
    pub fn truncated(&self) -> bool {
        (self.kept_rows() as u64) < self.row_count
    }

    /// Row `i` of the result.
    pub fn row(&self, i: usize) -> impl Iterator<Item = &Value> {
        self.columns
            .iter()
            .map(move |c| c.values.get(i).unwrap_or(&Value::Empty))
    }

    /// The column names as the header row cells get them.
    fn header(&self) -> Vec<Value> {
        self.columns
            .iter()
            .map(|c| {
                if c.name.is_empty() {
                    Value::Empty
                } else {
                    Value::text(c.name.as_str())
                }
            })
            .collect()
    }

    /// The result as a new tab's contents: the column names in the first row, the rows below, date and time
    /// columns formatted, the header row frozen, and the code that made it.
    pub fn tab_content(&self, code: Option<CodeSource>) -> TabContent {
        let mut styles: Vec<Style> = Vec::new();
        let mut columns = Vec::with_capacity(self.columns.len());
        let header = self.header();
        for (i, col) in self.columns.iter().enumerate() {
            let mut values = Vec::with_capacity(col.values.len() + 1);
            values.push(header[i].clone());
            values.extend(col.values.iter().cloned());
            while values.last().is_some_and(Value::is_empty) {
                values.pop();
            }
            let mut cc = ColumnContent {
                values,
                ..Default::default()
            };
            if let Some(fmt) = &col.format {
                let ix = match styles
                    .iter()
                    .position(|s| s.num_fmt.as_deref() == Some(fmt))
                {
                    Some(ix) => ix,
                    None => {
                        styles.push(Style {
                            num_fmt: Some(fmt.clone()),
                            ..Default::default()
                        });
                        styles.len() - 1
                    }
                } as u32;
                cc.styles = col
                    .values
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.is_empty())
                    .map(|(r, _)| (r as u32 + 1, ix))
                    .collect();
            }
            columns.push(cc);
        }
        let rows = if self.columns.is_empty() {
            0
        } else {
            self.kept_rows() as u32 + 1
        };
        TabContent {
            rows,
            cols: self.columns.len() as u32,
            columns,
            styles,
            frozen_rows: u32::from(rows > 1),
            code,
            ..Default::default()
        }
    }

    /// The result as cell writes into an existing tab with its top-left corner at (`row`, `col`): the column
    /// names, then the rows. Values replace what the cells held; date and time columns get their format, other
    /// cells keep theirs.
    pub fn cell_writes(&self, row: u32, col: u32) -> Vec<(u32, u32, CellWrite)> {
        let mut out = Vec::with_capacity((self.kept_rows() + 1) * self.columns.len());
        for (c, v) in self.header().into_iter().enumerate() {
            out.push((
                row,
                col + c as u32,
                CellWrite {
                    clear: true,
                    value: Some(v),
                    ..Default::default()
                },
            ));
        }
        for (c, column) in self.columns.iter().enumerate() {
            let patch = column.format.as_ref().map(|f| StylePatch {
                num_fmt: Some(Some(f.clone())),
                ..Default::default()
            });
            for (r, v) in column.values.iter().enumerate() {
                out.push((
                    row + 1 + r as u32,
                    col + c as u32,
                    CellWrite {
                        clear: true,
                        value: Some(v.clone()),
                        style_patch: if v.is_empty() { None } else { patch.clone() },
                        ..Default::default()
                    },
                ));
            }
        }
        out
    }
}

/// Converts files using fixed, trusted conversion routines.
pub struct Runner {
    cfg: RunnerConfig,
    probes: Mutex<Probes>,
}

#[derive(Default)]
struct Probes {
    python: Option<Result<Arc<script::Probe>, RunError>>,
    sandbox: Option<Result<sandbox::Kind, RunError>>,
}

impl Runner {
    pub fn new(cfg: RunnerConfig) -> Self {
        Self {
            cfg,
            probes: Mutex::new(Probes::default()),
        }
    }

    pub fn config(&self) -> &RunnerConfig {
        &self.cfg
    }

    /// The sandbox trusted converters run in, found once.
    pub(crate) fn sandbox(&self) -> Result<sandbox::Kind, RunError> {
        let mut p = self.probes.lock().expect("probes lock");
        p.sandbox
            .get_or_insert_with(|| sandbox::Kind::find(self.cfg.sandbox))
            .clone()
    }

    /// Which sandbox converters run in: `macos`, `bwrap` or `off`, or why there is none.
    pub fn sandbox_name(&self) -> Result<&'static str, RunError> {
        self.sandbox().map(|k| k.name())
    }

    /// Find the configured Python interpreter and its import paths once.
    pub(crate) fn probe(&self) -> Result<Arc<script::Probe>, RunError> {
        if let Some(found) = self.probes.lock().expect("probes lock").python.clone() {
            return found;
        }
        let found = script::Probe::find(self).map(Arc::new);
        self.probes
            .lock()
            .expect("probes lock")
            .python
            .get_or_insert(found)
            .clone()
    }

    /// A new scratch folder for one run or conversion, removed when the returned guard is dropped.
    pub(crate) fn scratch(&self) -> Result<Scratch, RunError> {
        Scratch::new(&self.cfg.scratch_root)
    }
}

/// A run's own folder, removed with everything in it when dropped.
pub(crate) struct Scratch {
    pub path: PathBuf,
}

impl Scratch {
    fn new(root: &Path) -> Result<Self, RunError> {
        use rand::Rng as _;
        std::fs::create_dir_all(root).map_err(|e| {
            RunError::internal(format!("making the scratch folder {}: {e}", root.display()))
        })?;
        let root = root
            .canonicalize()
            .map_err(|e| RunError::internal(format!("finding the scratch folder: {e}")))?;
        for _ in 0..16 {
            let name: String = {
                let mut rng = rand::rng();
                (0..16)
                    .map(|_| {
                        char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[rng.random_range(0..36)])
                    })
                    .collect()
            };
            let path = root.join(format!("run-{name}"));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        let _ =
                            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
                    }
                    std::fs::create_dir_all(path.join("tmp")).map_err(RunError::internal)?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(RunError::internal(format!("making a scratch folder: {e}"))),
            }
        }
        Err(RunError::internal(
            "sixteen scratch folder names in a row were taken",
        ))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), error = %e, "removing a run's scratch folder failed");
        }
    }
}
