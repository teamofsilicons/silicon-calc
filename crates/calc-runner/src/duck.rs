//! DuckDB inside the service: one in-memory database per run (or per conversion), holding the tables a query
//! needs. A database for code can't touch files or extensions and its settings are locked; only the
//! conversions the runner itself does (Parquet in and out) get a database that reads and writes files, and only
//! at paths the runner chose.

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

/// A database and two connections: one that runs things, one that only reads statements to check them.
pub(crate) struct Duck {
    db: ffi::duckdb_database,
    check: ffi::duckdb_connection,
    conn: Option<Connection>,
    /// The tables loaded, by lowercase name (DuckDB compares names without case).
    registered: Vec<String>,
}

// The handles are only used by one thread at a time (the run's), and DuckDB's handles may move between threads.
unsafe impl Send for Duck {}

fn c(s: &str) -> Result<CString, RunError> {
    CString::new(s).map_err(|_| RunError::failed("The text has a NUL character in it.", "Remove the NUL character."))
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
            ("enable_external_access", if s.files { "true" } else { "false" }),
            ("autoinstall_known_extensions", "false"),
            ("autoload_known_extensions", "false"),
            ("allow_community_extensions", "false"),
            ("threads", &threads),
            ("memory_limit", &memory),
            ("temp_directory", &spill),
            ("max_temp_directory_size", "8GiB"),
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
                    return Err(RunError::internal(format!("DuckDB refused the setting {k}={v}")));
                }
            }
            let mut db: ffi::duckdb_database = std::ptr::null_mut();
            let mut err: *mut std::os::raw::c_char = std::ptr::null_mut();
            let path = c(":memory:")?;
            let r = ffi::duckdb_open_ext(path.as_ptr(), &mut db, cfg, &mut err);
            ffi::duckdb_destroy_config(&mut cfg);
            if r != ffi::DuckDBSuccess {
                let msg = if err.is_null() { "unknown".to_owned() } else { CStr::from_ptr(err).to_string_lossy().into_owned() };
                if !err.is_null() {
                    ffi::duckdb_free(err as *mut std::ffi::c_void);
                }
                return Err(RunError::internal(format!("DuckDB could not open a database: {msg}")));
            }
            let mut check: ffi::duckdb_connection = std::ptr::null_mut();
            if ffi::duckdb_connect(db, &mut check) != ffi::DuckDBSuccess {
                ffi::duckdb_close(&mut db);
                return Err(RunError::internal("DuckDB could not connect"));
            }
            let conn = match Connection::open_from_raw(db) {
                Ok(c) => c,
                Err(e) => {
                    ffi::duckdb_disconnect(&mut check);
                    ffi::duckdb_close(&mut db);
                    return Err(e.into());
                }
            };
            Ok(Self { db, check, conn: Some(conn), registered: Vec::new() })
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
        let cols: Vec<String> =
            table.columns.iter().zip(&table.types).map(|(n, t)| format!("{} {}", quote_ident(n), t.sql())).collect();
        self.conn().execute_batch(&format!("CREATE TABLE {} ({})", quote_ident(name), cols.join(", ")))?;
        if table.rows() > 0 {
            let mut app = self.conn().appender(name)?;
            app.append_record_batch(table.batch.clone())?;
            app.flush()?;
        }
        self.registered.push(name.to_lowercase());
        Ok(())
    }

    /// The statements `sql` holds and the type of the first once bound. A statement that can't be read or bound
    /// gives DuckDB's message.
    pub fn first_statement(&self, sql: &str) -> Result<(usize, ffi::duckdb_statement_type), String> {
        let text = CString::new(sql).map_err(|_| "The SQL has a NUL character in it.".to_owned())?;
        unsafe {
            let mut extracted: ffi::duckdb_extracted_statements = std::ptr::null_mut();
            let n = ffi::duckdb_extract_statements(self.check, text.as_ptr(), &mut extracted);
            if n == 0 {
                let e = ffi::duckdb_extract_statements_error(extracted);
                let msg = if e.is_null() { String::new() } else { CStr::from_ptr(e).to_string_lossy().into_owned() };
                ffi::duckdb_destroy_extracted(&mut extracted);
                return if msg.is_empty() { Ok((0, ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_INVALID)) } else { Err(msg) };
            }
            let mut st: ffi::duckdb_prepared_statement = std::ptr::null_mut();
            let r = ffi::duckdb_prepare_extracted_statement(self.check, extracted, 0, &mut st);
            if r != ffi::DuckDBSuccess {
                let e = ffi::duckdb_prepare_error(st);
                let msg = if e.is_null() { "the statement can't be prepared".to_owned() } else { CStr::from_ptr(e).to_string_lossy().into_owned() };
                ffi::duckdb_destroy_prepare(&mut st);
                ffi::duckdb_destroy_extracted(&mut extracted);
                return Err(msg);
            }
            let ty = ffi::duckdb_prepared_statement_type(st);
            ffi::duckdb_destroy_prepare(&mut st);
            ffi::duckdb_destroy_extracted(&mut extracted);
            Ok((n as usize, ty))
        }
    }

    /// Checks a statement list that bound: one statement that only reads (a query, or EXPLAIN of one). A single
    /// PIVOT, which DuckDB reads as two statements (the first lists the pivot's columns), is allowed too.
    pub fn check_read_only(sql: &str, n: usize, first: ffi::duckdb_statement_type) -> Result<(), RunError> {
        let pivot = first_keyword(sql).is_some_and(|k| k.eq_ignore_ascii_case("pivot") || k.eq_ignore_ascii_case("unpivot"));
        if n == 0 {
            return Err(RunError::failed("The SQL is empty: there is no statement to run.", "Write a query, like select * from Sales."));
        }
        if pivot && n == 2 && first == ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_CREATE {
            return Ok(());
        }
        if n > 1 {
            return Err(RunError::failed(
                format!("Run one statement at a time: this SQL has {n} statements."),
                "Combine them into one query (with … select …), or run them one after the other.",
            ));
        }
        match first {
            ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_SELECT | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_EXPLAIN => Ok(()),
            other => Err(RunError::failed(
                format!("SQL in Silicon Calc only reads the sheet, so {} statements can't run here.", statement_word(other)),
                "Ask with select. To change cells, write the result into a tab (into), or edit the cells themselves.",
            )),
        }
    }

    /// Runs a query, stopping it after `timeout`.
    pub fn query(&self, sql: &str, keep: Option<usize>, max_cells: u64, timeout: Duration) -> Result<(Vec<OutColumn>, u64), RunError> {
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
                format!("The query ran for more than {} seconds and was stopped.", timeout.as_secs().max(1)),
                "Make it lighter: filter rows early, aggregate, or join on fewer rows.",
            ));
        }
        result
    }

    /// Runs statements the runner wrote itself (conversions).
    pub fn execute(&self, sql: &str) -> Result<(), RunError> {
        self.conn().execute_batch(sql).map_err(|e| {
            let msg = e.to_string();
            RunError::failed(msg.lines().next().unwrap_or("DuckDB failed").to_owned(), "Check the file.").with_stderr(msg)
        })
    }

    /// Writes a loaded table to a Parquet file (conversions and Python's `calc.tab()`).
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
            ffi::duckdb_disconnect(&mut self.check);
            ffi::duckdb_close(&mut self.db);
        }
    }
}

/// The first word of the SQL, after comments and opening parentheses.
pub(crate) fn first_keyword(sql: &str) -> Option<&str> {
    let mut s = sql;
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        if let Some(rest) = s.strip_prefix("--") {
            s = rest.split_once('\n').map(|(_, r)| r).unwrap_or("");
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map(|(_, r)| r).unwrap_or("");
        } else {
            break;
        }
    }
    let end = s.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(s.len());
    (end > 0).then(|| &s[..end])
}

fn statement_word(t: ffi::duckdb_statement_type) -> &'static str {
    match t {
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_INSERT => "INSERT",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_UPDATE => "UPDATE",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_DELETE => "DELETE",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_CREATE | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_CREATE_FUNC => "CREATE",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_DROP => "DROP",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_ALTER => "ALTER",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_COPY | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_COPY_DATABASE => "COPY",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_SET | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_VARIABLE_SET => "SET",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_PRAGMA => "PRAGMA",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_ATTACH => "ATTACH",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_DETACH => "DETACH",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_LOAD | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_UPDATE_EXTENSIONS => "INSTALL and LOAD",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_CALL => "CALL",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_EXPORT => "EXPORT",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_VACUUM | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_ANALYZE => "VACUUM and ANALYZE",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_TRANSACTION => "transaction",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_PREPARE | ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_EXECUTE => "PREPARE and EXECUTE",
        ffi::duckdb_statement_type_DUCKDB_STATEMENT_TYPE_MERGE_INTO => "MERGE",
        _ => "these",
    }
}

fn duck_error(e: &duckdb::Error) -> RunError {
    let msg = match e {
        duckdb::Error::DuckDBFailure(_, Some(m)) => m.clone(),
        other => other.to_string(),
    };
    if msg.contains("INTERRUPT") || msg.contains("Interrupted") {
        return RunError::new(RunErrorKind::Timeout, "The query was stopped: it ran too long.", "Make it lighter and run it again.");
    }
    if msg.contains("Out of Memory") {
        return RunError::new(
            RunErrorKind::Memory,
            "The query needed more memory than a run may use.",
            "Filter or aggregate earlier, or select fewer columns.",
        )
        .with_stderr(msg);
    }
    sql_error(&msg, None)
}

/// The name of the table a DuckDB message says doesn't exist (`Table with name Sales does not exist!`).
pub(crate) fn missing_table(msg: &str) -> Option<String> {
    let rest = msg.split_once("Table with name ")?.1;
    let (name, _) = rest.split_once(" does not exist")?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// The line a DuckDB message points at (`LINE 3: …`).
pub(crate) fn error_line(msg: &str) -> Option<u32> {
    let i = msg.find("LINE ")?;
    let digits: String = msg[i + 5..].chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// A DuckDB message made into an error: what happened first, DuckDB's whole text as `stderr`.
pub(crate) fn sql_error(msg: &str, tabs: Option<&[String]>) -> RunError {
    let line = error_line(msg);
    let first = msg.split("\n\n").next().unwrap_or(msg).trim();
    let first = first.split_once("Error: ").map(|(_, rest)| rest).unwrap_or(first).trim();
    let one_line = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let (message, hint) = match missing_table(msg) {
        Some(name) => (format!("There is no tab named {name} in this sheet."), missing_hint(tabs)),
        None if msg.contains("disabled by configuration") || msg.starts_with("Permission Error") => (
            format!("The SQL tried to reach files or the network, which SQL in Silicon Calc can't: {one_line}"),
            "Query the sheet's tabs by name (select * from Sales); to bring in a file, import it as a tab first.".to_owned(),
        ),
        None => {
            let at = line.map(|l| format!(" (line {l})")).unwrap_or_default();
            (format!("The SQL failed{at}: {one_line}"), "Fix the query and run it again. The SQL is DuckDB's.".to_owned())
        }
    };
    RunError::failed(message, hint).with_line(line).with_stderr(msg.trim().to_owned())
}

/// What to say when a query names a tab that isn't there.
pub(crate) fn missing_hint(tabs: Option<&[String]>) -> String {
    match tabs {
        Some(t) if !t.is_empty() => format!(
            "The tabs are {}. Put names with spaces or other signs in double quotes (\"By region\"); a range is a table too (\"Sales!A1:C9\").",
            t.iter().map(|n| quote_ident(n)).collect::<Vec<_>>().join(", ")
        ),
        _ => "Use a tab's name; put names with spaces in double quotes (\"By region\"). A range is a table too (\"Sales!A1:C9\").".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords() {
        assert_eq!(first_keyword("  -- hi\n /* x */ (select 1)"), Some("select"));
        assert_eq!(first_keyword("PIVOT t on x"), Some("PIVOT"));
        assert_eq!(first_keyword("   "), None);
    }

    #[test]
    fn error_messages() {
        let e = sql_error("Catalog Error: Table with name salse does not exist!\nDid you mean \"Sales\"?\n\nLINE 1: select * from salse\n                      ^", Some(&["Sales".to_owned()]));
        assert_eq!(e.message, "There is no tab named salse in this sheet.");
        assert_eq!(e.line, Some(1));
        assert!(e.hint.contains("\"Sales\""));
        let e = sql_error("Parser Error: syntax error at or near \"form\"\n\nLINE 3: select * form x\n                 ^", None);
        assert_eq!(e.message, "The SQL failed (line 3): syntax error at or near \"form\"");
        assert_eq!(e.line, Some(3));
        assert_eq!(missing_table("Catalog Error: Table with name By region does not exist!"), Some("By region".to_owned()));
        assert_eq!(missing_table("Catalog Error: Table Function with name foo does not exist!"), None);
    }
}
