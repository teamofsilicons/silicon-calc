//! Private DuckDB database per trusted Parquet conversion. Queries and paths are
//! generated internally; no submitted command is evaluated here.

use std::{
    ffi::{CStr, CString},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use duckdb::{Connection, ffi};

use crate::{OutColumn, RunError, RunErrorKind, table::Table, values::Collector};

/// How a database is set up.
pub(crate) struct Settings<'a> {
    /// Whether it may read and write files (conversions only, never code).
    pub files: bool,
    pub threads: usize,
    pub memory_bytes: u64,
    /// Where it may spill when a query needs more memory than it has.
    pub spill_dir: &'a Path,
}

/// A database and connection used for one conversion.
pub(crate) struct Duck {
    db: ffi::duckdb_database,
    conn: Option<Connection>,
    /// The tables loaded, by lowercase name (DuckDB compares names without case).
    registered: Vec<String>,
}

// The handles are only used by one thread at a time (the run's), and DuckDB's handles may move between threads.
unsafe impl Send for Duck {}

fn c(s: &str) -> Result<CString, RunError> {
    CString::new(s).map_err(|_| {
        RunError::failed(
            "The text has a NUL character in it.",
            "Remove the NUL character.",
        )
    })
}

/// `"name"`, quoted for SQL.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'`, quoted for SQL.
pub fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

impl Duck {
    pub fn open(s: &Settings<'_>) -> Result<Self, RunError> {
        let spill = s.spill_dir.to_string_lossy().into_owned();
        let memory = format!("{}MiB", (s.memory_bytes >> 20).max(64));
        let threads = s.threads.max(1).to_string();
        let mut opts: Vec<(&str, &str)> = vec![
            ("autoinstall_known_extensions", "false"),
            ("autoload_known_extensions", "false"),
            ("allow_community_extensions", "false"),
            ("threads", &threads),
            ("memory_limit", &memory),
            ("temp_directory", &spill),
            ("max_temp_directory_size", "8GiB"),
            (
                "enable_external_access",
                if s.files { "true" } else { "false" },
            ),
        ];
        if !s.files {
            opts.push(("lock_configuration", "true"));
        }
        unsafe {
            let mut cfg: ffi::duckdb_config = std::ptr::null_mut();
            if ffi::duckdb_create_config(&mut cfg) != ffi::DuckDBSuccess {
                return Err(RunError::internal("DuckDB could not make a configuration"));
            }
            for (k, v) in &opts {
                let (ck, cv) = (c(k)?, c(v)?);
                if ffi::duckdb_set_config(cfg, ck.as_ptr(), cv.as_ptr()) != ffi::DuckDBSuccess {
                    ffi::duckdb_destroy_config(&mut cfg);
                    return Err(RunError::internal(format!(
                        "DuckDB refused the setting {k}={v}"
                    )));
                }
            }
            let mut db: ffi::duckdb_database = std::ptr::null_mut();
            let mut err: *mut std::os::raw::c_char = std::ptr::null_mut();
            let path = c(":memory:")?;
            let r = ffi::duckdb_open_ext(path.as_ptr(), &mut db, cfg, &mut err);
            ffi::duckdb_destroy_config(&mut cfg);
            if r != ffi::DuckDBSuccess {
                let msg = if err.is_null() {
                    "unknown".to_owned()
                } else {
                    CStr::from_ptr(err).to_string_lossy().into_owned()
                };
                if !err.is_null() {
                    ffi::duckdb_free(err as *mut std::ffi::c_void);
                }
                return Err(RunError::internal(format!(
                    "DuckDB could not open a database: {msg}"
                )));
            }
            let conn = match Connection::open_from_raw(db) {
                Ok(c) => c,
                Err(e) => {
                    ffi::duckdb_close(&mut db);
                    return Err(e.into());
                }
            };
            Ok(Self {
                db,
                conn: Some(conn),
                registered: Vec::new(),
            })
        }
    }

    pub fn conn(&self) -> &Connection {
        self.conn.as_ref().expect("open until dropped")
    }

    /// Whether a table of this name is loaded.
    pub fn has(&self, name: &str) -> bool {
        self.registered.contains(&name.to_lowercase())
    }

    /// Loads a table under `name` (once per name).
    pub fn register_as(&mut self, name: &str, table: &Table) -> Result<(), RunError> {
        if self.has(name) {
            return Ok(());
        }
        let cols: Vec<String> = table
            .columns
            .iter()
            .zip(&table.types)
            .map(|(n, t)| format!("{} {}", quote_ident(n), t.sql()))
            .collect();
        self.conn().execute_batch(&format!(
            "CREATE TABLE {} ({})",
            quote_ident(name),
            cols.join(", ")
        ))?;
        if table.rows() > 0 {
            let mut app = self.conn().appender(name)?;
            app.append_record_batch(table.batch.clone())?;
            app.flush()?;
        }
        self.registered.push(name.to_lowercase());
        Ok(())
    }

    /// Runs a query, stopping it after `timeout`.
    pub fn query(
        &self,
        sql: &str,
        keep: Option<usize>,
        max_cells: u64,
        timeout: Duration,
    ) -> Result<(Vec<OutColumn>, u64), RunError> {
        let interrupt = self.conn().interrupt_handle();
        let timed_out = Arc::new(AtomicBool::new(false));
        let (done, wait) = mpsc::channel::<()>();
        let flag = timed_out.clone();
        let watchdog = std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                flag.store(true, Ordering::SeqCst);
                interrupt.interrupt();
            }
        });
        let result = (|| -> Result<(Vec<OutColumn>, u64), RunError> {
            let mut stmt = self.conn().prepare(sql).map_err(|e| duck_error(&e))?;
            let arrow = stmt.query_arrow([]).map_err(|e| duck_error(&e))?;
            let schema = arrow.get_schema();
            let mut collector = Collector::new(&schema, keep, max_cells);
            for batch in arrow {
                collector.push_batch(&batch)?;
            }
            Ok(collector.finish())
        })();
        drop(done);
        let _ = watchdog.join();
        if timed_out.load(Ordering::SeqCst) {
            return Err(RunError::new(
                RunErrorKind::Timeout,
                format!(
                    "The query ran for more than {} seconds and was stopped.",
                    timeout.as_secs().max(1)
                ),
                "Try a smaller Parquet file.",
            ));
        }
        result
    }

    /// Runs statements the runner wrote itself (conversions).
    pub fn execute(&self, sql: &str) -> Result<(), RunError> {
        self.conn().execute_batch(sql).map_err(|e| {
            let msg = e.to_string();
            RunError::failed(
                msg.lines().next().unwrap_or("DuckDB failed").to_owned(),
                "Check the file.",
            )
            .with_stderr(msg)
        })
    }

    /// Writes a loaded table to a Parquet file.
    pub fn write_parquet(&self, table: &str, path: &Path) -> Result<(), RunError> {
        self.execute(&format!(
            "COPY {} TO {} (FORMAT parquet, COMPRESSION snappy)",
            quote_ident(table),
            quote_literal(&path.to_string_lossy())
        ))
    }
}

impl Drop for Duck {
    fn drop(&mut self) {
        self.conn.take();
        unsafe {
            ffi::duckdb_close(&mut self.db);
        }
    }
}

fn duck_error(e: &duckdb::Error) -> RunError {
    let msg = match e {
        duckdb::Error::DuckDBFailure(_, Some(m)) => m.clone(),
        other => other.to_string(),
    };
    if msg.contains("INTERRUPT") || msg.contains("Interrupted") {
        return RunError::new(
            RunErrorKind::Timeout,
            "The query was stopped: it ran too long.",
            "Try a smaller Parquet file.",
        );
    }
    if msg.contains("Out of Memory") {
        return RunError::new(
            RunErrorKind::Memory,
            "The query needed more memory than a run may use.",
            "Try a smaller Parquet file.",
        )
        .with_stderr(msg);
    }
    RunError::failed(
        "The Parquet conversion failed.",
        "Check the file and converter details.",
    )
    .with_stderr(msg)
}
