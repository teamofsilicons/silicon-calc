//! SQL runs: DuckDB in this process, with only the tables a query names, loaded from the sheet when DuckDB first
//! asks for them (so a query over one tab of a big sheet never reads the others).

use std::path::Path;

use calc_engine::TabId;

use crate::{
    OutColumn, RunError, RunOutput, RunRequest, Runner, SheetSource,
    duck::{Duck, Settings, missing_hint, missing_table, sql_error},
};

/// The most tables one query may load.
const MAX_TABLES: usize = 64;

/// A database for code, and the tables it has loaded so far.
pub(crate) struct Session<'a> {
    duck: Duck,
    sheet: &'a dyn SheetSource,
    /// The tabs read, in the order they were first read.
    pub inputs: Vec<TabId>,
}

impl<'a> Session<'a> {
    pub fn open(runner: &Runner, sheet: &'a dyn SheetSource, spill_dir: &Path) -> Result<Self, RunError> {
        let l = &runner.config().limits;
        let duck = Duck::open(&Settings { files: false, threads: l.threads, memory_bytes: l.memory_bytes, spill_dir })?;
        Ok(Self { duck, sheet, inputs: Vec::new() })
    }

    /// Reads `sql`, loading every table it names that isn't loaded yet, and checks it only reads.
    pub fn prepare(&mut self, sql: &str) -> Result<(), RunError> {
        if sql.trim().is_empty() {
            return Err(RunError::failed("The SQL is empty: there is no statement to run.", "Write a query, like select * from Sales."));
        }
        for _ in 0..=MAX_TABLES {
            match self.duck.first_statement(sql) {
                Ok((n, first)) => return Duck::check_read_only(sql, n, first),
                Err(msg) => {
                    let Some(name) = missing_table(&msg) else {
                        return Err(sql_error(&msg, Some(&self.sheet.tab_names())));
                    };
                    if self.duck.has(&name) {
                        return Err(sql_error(&msg, Some(&self.sheet.tab_names())));
                    }
                    let Some(table) = self.sheet.table(&name) else {
                        let tabs = self.sheet.tab_names();
                        return Err(RunError::failed(format!("There is no tab named {name} in this sheet."), missing_hint(Some(&tabs)))
                            .with_line(crate::duck::error_line(&msg))
                            .with_stderr(msg.trim().to_owned()));
                    };
                    self.duck.register_as(&name, &table)?;
                    if !self.inputs.contains(&table.tab) {
                        self.inputs.push(table.tab);
                    }
                }
            }
        }
        Err(RunError::failed(
            format!("The query reads more than {MAX_TABLES} tables, more than one query may."),
            "Split it into smaller queries.",
        ))
    }

    /// Runs a query that `prepare` accepted.
    pub fn query(&self, runner: &Runner, sql: &str, keep: Option<usize>) -> Result<(Vec<OutColumn>, u64), RunError> {
        let l = &runner.config().limits;
        self.duck.query(sql, keep, l.result_cells, l.timeout)
    }
}

pub(crate) fn run(runner: &Runner, req: &RunRequest<'_>) -> Result<RunOutput, RunError> {
    let scratch = runner.scratch()?;
    let mut session = Session::open(runner, req.sheet, &scratch.path.join("tmp"))?;
    session.prepare(req.code)?;
    let (columns, row_count) = session.query(runner, req.code, req.keep_rows)?;
    Ok(RunOutput { columns, row_count, has_result: true, inputs: session.inputs, ..Default::default() })
}
