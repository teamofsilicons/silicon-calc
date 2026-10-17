//! What can go wrong running code or converting a file, in words for the Carbon or Silicon who asked.

/// Why a run (or a conversion) failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunErrorKind {
    /// The code itself failed: it can't be read, it raised an error, or its SQL doesn't fit the sheet. For a
    /// conversion: the file can't be read or written in that format.
    Failed,
    /// It ran longer than its time limit and was stopped.
    Timeout,
    /// It used more memory than it may and was stopped.
    Memory,
    /// What it gave back is more than one run may write.
    TooLarge,
    /// Code can't run here at all: no sandbox, or no Python or Node.
    Unavailable,
    /// Something inside the runner failed.
    Internal,
}

/// A failed run: what happened and why (`message`), what to do (`hint`), and what the code printed.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct RunError {
    pub kind: RunErrorKind,
    pub message: String,
    pub hint: String,
    /// What the code wrote to standard error (a traceback, DuckDB's full message).
    pub stderr: String,
    /// What the code printed before it failed.
    pub stdout: String,
    /// The line of the code the failure points at (1-based), when it is known.
    pub line: Option<u32>,
}

impl RunError {
    pub fn new(kind: RunErrorKind, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { kind, message: message.into(), hint: hint.into(), stderr: String::new(), stdout: String::new(), line: None }
    }

    pub fn failed(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(RunErrorKind::Failed, message, hint)
    }

    pub fn too_large(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(RunErrorKind::TooLarge, message, hint)
    }

    pub fn unavailable(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(RunErrorKind::Unavailable, message, hint)
    }

    pub fn internal(what: impl std::fmt::Display) -> Self {
        Self::new(
            RunErrorKind::Internal,
            format!("Silicon Calc's code runner failed: {what}"),
            "Try again; if it keeps failing, the service's log has the details.",
        )
    }

    pub fn with_output(mut self, stdout: impl Into<String>, stderr: impl Into<String>) -> Self {
        self.stdout = stdout.into();
        self.stderr = stderr.into();
        self
    }

    pub fn with_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }

    pub fn with_stderr(mut self, stderr: impl Into<String>) -> Self {
        self.stderr = stderr.into();
        self
    }
}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        Self::internal(e)
    }
}

impl From<duckdb::Error> for RunError {
    fn from(e: duckdb::Error) -> Self {
        Self::internal(format!("DuckDB: {e}"))
    }
}
