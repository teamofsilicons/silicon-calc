//! What a failed command says: a stable code, what happened and why, what to do, and its exit code.
//!
//! Exit codes: 0 success, 1 failure (the service refused, an account or sheet isn't there, the network failed),
//! 2 usage (the command line can't be acted on as written: a bad flag, value, range or file, an ambiguous title,
//! a confirmation that wasn't given), 130 interrupted.

use serde_json::{Value, json};

/// Exit codes.
pub mod exit {
    /// Something failed.
    pub const FAILURE: u8 = 1;
    /// The command line can't be acted on as written.
    pub const USAGE: u8 = 2;
    /// Stopped with Ctrl-C.
    pub const INTERRUPTED: u8 = 130;
}

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub status: Option<u16>,
    pub details: Option<Value>,
    pub exit: u8,
}

pub type CliResult<T> = Result<T, CliError>;

impl CliError {
    /// A failure (exit 1).
    pub fn new(code: impl Into<String>, message: impl Into<String>, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        Self {
            code: code.into(),
            message: message.into(),
            hint: (!hint.is_empty()).then_some(hint),
            status: None,
            details: None,
            exit: exit::FAILURE,
        }
    }

    /// A usage error (exit 2).
    pub fn usage(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { exit: exit::USAGE, ..Self::new("usage", message, hint) }
    }

    /// A usage error with its own code (exit 2).
    pub fn usage_code(code: impl Into<String>, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { exit: exit::USAGE, ..Self::new(code, message, hint) }
    }

    /// A local file couldn't be read or written (exit 2: the path given is the problem).
    pub fn file(message: impl Into<String>) -> Self {
        Self::usage_code("file", message, "Check the path, its permissions and the free space.")
    }

    pub fn not_found(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new("not_found", message, hint)
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn interrupted(message: impl Into<String>) -> Self {
        Self { exit: exit::INTERRUPTED, ..Self::new("interrupted", message, "") }
    }

    /// `{"error": {"code", "message", "hint", "status", "details", "exit_code"}}`.
    pub fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message, "exit_code": self.exit });
        if let Some(hint) = &self.hint {
            error["hint"] = json!(hint);
        }
        if let Some(status) = self.status {
            error["status"] = json!(status);
        }
        if let Some(details) = &self.details {
            error["details"] = details.clone();
        }
        json!({ "error": error })
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<silicon_calc_client::Error> for CliError {
    fn from(e: silicon_calc_client::Error) -> Self {
        use silicon_calc_client::Error as E;
        let exit = match &e {
            E::InvalidInput { .. } => exit::USAGE,
            _ => exit::FAILURE,
        };
        let mut details = e.details().cloned();
        if let Some(after) = e.retry_after() {
            let d = details.get_or_insert_with(|| json!({}));
            if d.is_object() {
                d["retry_after_seconds"] = json!(after.as_secs());
            }
        }
        let hint = match (&e, e.hint()) {
            (_, Some(h)) => Some(h.to_owned()),
            (E::Api { status: 401, .. }, None) => Some(SIGN_IN_HINT.to_owned()),
            _ => None,
        };
        Self { code: e.code().to_owned(), message: e.message().to_owned(), hint, status: e.status(), details, exit }
    }
}

impl From<silicon_accounts_client::Error> for CliError {
    fn from(e: silicon_accounts_client::Error) -> Self {
        let code = if e.is_transport() {
            "network".to_owned()
        } else if let Some(o) = e.as_oauth() {
            o.error.clone()
        } else {
            e.code().to_owned()
        };
        let mut message = e.message();
        if e.is_transport() {
            message = format!("Can't reach Silicon Accounts: {message}");
        }
        let exit = if e.is_code("invalid_input") || e.is_code("invalid_stk") || e.is_code("invalid_id") {
            exit::USAGE
        } else {
            exit::FAILURE
        };
        let mut details = e.details().cloned();
        if let Some(after) = e.retry_after() {
            let d = details.get_or_insert_with(|| json!({}));
            if d.is_object() {
                d["retry_after_seconds"] = json!(after.as_secs());
            }
        }
        Self { code, message, hint: e.hint(), status: e.status(), details, exit }
    }
}

/// How each kind of account signs in.
pub const SIGN_IN_HINT: &str = "Carbons: calc login. Silicons: calc login --silicon si:you --stk-stdin, or \
     silicon-accounts login --app calc -q | calc login --slt-stdin.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_errors_keep_their_words_and_exit_1() {
        let e = silicon_calc_client::Error::from_response(
            403,
            br#"{"error":{"code":"not_allowed","message":"You can't edit \"Q3\": you are a viewer on it.","hint":"Ask its owner."}}"#,
            None,
        );
        let c = CliError::from(e);
        assert_eq!(c.code, "not_allowed");
        assert_eq!(c.exit, 1);
        assert_eq!(c.hint.as_deref(), Some("Ask its owner."));
        let j = c.to_json();
        assert_eq!(j["error"]["status"], 403);
        assert_eq!(j["error"]["exit_code"], 1);
    }

    #[test]
    fn usage_is_exit_2() {
        assert_eq!(CliError::usage("x", "y").exit, 2);
        assert_eq!(CliError::from(silicon_calc_client::Error::invalid_input("x", "y")).exit, 2);
        assert_eq!(CliError::file("nope").exit, 2);
    }

    #[test]
    fn retry_after_lands_in_details() {
        let e = silicon_calc_client::Error::from_response(
            429,
            br#"{"error":{"code":"slow_down","message":"Too many.","hint":"Wait."}}"#,
            Some(std::time::Duration::from_secs(4)),
        );
        let c = CliError::from(e);
        assert_eq!(c.details.unwrap()["retry_after_seconds"], 4);
    }
}
