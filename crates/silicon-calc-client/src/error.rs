//! Typed errors. Every error carries a stable `code`, a `message` saying what happened and why, and a `hint`
//! saying what to do, exactly like the service's `{"error": {"code", "message", "hint", "details"}}`.

use std::time::Duration;

use serde_json::{Value, json};

/// Result alias used throughout the crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong when talking to Silicon Calc.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The service answered with an error status: its `{"error": {…}}` body, or a fitting default when the body
    /// was not JSON.
    #[error("{message}")]
    Api {
        /// HTTP status code.
        status: u16,
        /// Stable code: `invalid_request`, `signed_out`, `not_allowed`, `not_found`, `conflict`, `too_large`,
        /// `invalid_field`, `slow_down`, `internal`, `unavailable`, or a specific one such as `protected`,
        /// `bad_formula`, `code_failed`, `no_such_tab`.
        code: String,
        /// What happened and why.
        message: String,
        /// What to do.
        hint: Option<String>,
        /// The specifics (`fields` for `invalid_field`, `index` for a failed edit, `stderr`/`line` for code).
        details: Option<Value>,
        /// How long the service asked to wait before retrying (`Retry-After`), for `slow_down`.
        retry_after: Option<Duration>,
    },
    /// The request never got an answer (connection refused, DNS, TLS, timeout…).
    #[error("{message}")]
    Network {
        /// What failed.
        message: String,
        /// What to do.
        hint: String,
        /// The transport error.
        #[source]
        source: reqwest::Error,
    },
    /// The service answered, but not with what this client expected.
    #[error("{message}")]
    Decode {
        /// What failed.
        message: String,
        /// What to do.
        hint: String,
    },
    /// Getting an access token failed (raised by a [`crate::TokenSource`]).
    #[error("{message}")]
    Auth {
        /// Stable code, usually `signed_out`.
        code: String,
        /// What failed.
        message: String,
        /// What to do.
        hint: String,
    },
    /// A value passed to the client was refused before anything was sent.
    #[error("{message}")]
    InvalidInput {
        /// What is wrong.
        message: String,
        /// How to fix it.
        hint: String,
    },
    /// Reading or writing a local file failed.
    #[error("{message}")]
    Io {
        /// What failed.
        message: String,
        /// What to do.
        hint: String,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// An [`Error::InvalidInput`].
    pub fn invalid_input(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::InvalidInput { message: message.into(), hint: hint.into() }
    }

    /// An [`Error::Auth`] with the code `signed_out`.
    pub fn signed_out(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Auth { code: "signed_out".into(), message: message.into(), hint: hint.into() }
    }

    /// An [`Error::Io`].
    pub fn io(message: impl Into<String>, hint: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { message: message.into(), hint: hint.into(), source }
    }

    pub(crate) fn decode(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Decode { message: message.into(), hint: hint.into() }
    }

    /// The stable machine-readable code.
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } | Self::Auth { code, .. } => code,
            Self::Network { .. } => "network",
            Self::Decode { .. } => "unexpected_answer",
            Self::InvalidInput { .. } => "invalid_input",
            Self::Io { .. } => "io",
        }
    }

    /// What happened and why.
    pub fn message(&self) -> &str {
        match self {
            Self::Api { message, .. }
            | Self::Network { message, .. }
            | Self::Decode { message, .. }
            | Self::Auth { message, .. }
            | Self::InvalidInput { message, .. }
            | Self::Io { message, .. } => message,
        }
    }

    /// What to do, when known.
    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::Api { hint, .. } => hint.as_deref().filter(|h| !h.is_empty()),
            Self::Network { hint, .. }
            | Self::Decode { hint, .. }
            | Self::Auth { hint, .. }
            | Self::InvalidInput { hint, .. }
            | Self::Io { hint, .. } => Some(hint).filter(|h| !h.is_empty()).map(String::as_str),
        }
    }

    /// The HTTP status, for errors the service answered.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The error's `details`, when the service gave any.
    pub fn details(&self) -> Option<&Value> {
        match self {
            Self::Api { details, .. } => details.as_ref(),
            _ => None,
        }
    }

    /// How long to wait before retrying, when the service said (`Retry-After`).
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Api { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// True when the error has this code.
    pub fn is_code(&self, code: &str) -> bool {
        self.code() == code
    }

    /// True for `not_found` (404).
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404) || self.is_code("not_found")
    }

    /// True when there is no valid sign-in: the service answered 401, or no token could be had.
    pub fn is_signed_out(&self) -> bool {
        self.status() == Some(401) || matches!(self, Self::Auth { .. })
    }

    /// True when no answer was received at all.
    pub fn is_network(&self) -> bool {
        matches!(self, Self::Network { .. })
    }

    /// True when trying again later may work: no answer, `slow_down`, or the service being briefly unable.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Network { .. } => true,
            Self::Api { status, .. } => matches!(status, 429 | 502 | 503 | 504),
            _ => false,
        }
    }

    /// The error as the service writes it: `{"error": {"code", "message", "hint", "status"?, "details"?}}`.
    pub fn to_json(&self) -> Value {
        let mut error = json!({ "code": self.code(), "message": self.message() });
        if let Some(hint) = self.hint() {
            error["hint"] = json!(hint);
        }
        if let Some(status) = self.status() {
            error["status"] = json!(status);
        }
        if let Some(details) = self.details() {
            error["details"] = details.clone();
        }
        if let Some(after) = self.retry_after() {
            error["retry_after"] = json!(after.as_secs());
        }
        json!({ "error": error })
    }

    /// Builds the error for an answer with an error status from its status, body and `Retry-After`.
    pub fn from_response(status: u16, body: &[u8], retry_after: Option<Duration>) -> Self {
        let parsed: Option<Value> = serde_json::from_slice(body).ok();
        let error = parsed.as_ref().and_then(|v| v.get("error"));
        let text = |key: &str| error.and_then(|e| e.get(key)).and_then(Value::as_str).map(str::to_owned);
        // Some gateways answer `{"error": "text"}`.
        let flat = error.and_then(Value::as_str).map(str::to_owned);
        let code = text("code").unwrap_or_else(|| default_code(status).to_owned());
        let message = text("message").or(flat).unwrap_or_else(|| {
            let snippet: String = String::from_utf8_lossy(body).trim().chars().take(200).collect();
            if snippet.is_empty() {
                format!("Silicon Calc answered HTTP {status} with no explanation.")
            } else {
                format!("Silicon Calc answered HTTP {status}: {snippet}")
            }
        });
        let hint = text("hint").filter(|h| !h.is_empty()).or_else(|| default_hint(&code).map(str::to_owned));
        let details = error.and_then(|e| e.get("details")).cloned().filter(|d| !d.is_null());
        let retry_after = retry_after.or_else(|| {
            details
                .as_ref()
                .and_then(|d| d.get("retry_after_seconds").or_else(|| d.get("retry_after")))
                .and_then(Value::as_u64)
                .map(Duration::from_secs)
        });
        Self::Api { status, code, message, hint, details, retry_after }
    }
}

fn default_code(status: u16) -> &'static str {
    match status {
        400 => "invalid_request",
        401 => "signed_out",
        403 => "not_allowed",
        404 => "not_found",
        405 => "not_supported",
        409 => "conflict",
        413 => "too_large",
        422 => "invalid_field",
        429 => "slow_down",
        503 => "unavailable",
        _ if status >= 500 => "internal",
        _ => "http_error",
    }
}

fn default_hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "signed_out" => "Sign in again (calc login), then retry.",
        "not_allowed" => "Ask the sheet's owner for a role that allows this.",
        "not_found" => "Check the id; it may have been deleted, or it isn't shared with you.",
        "not_supported" => "This Silicon Calc service doesn't offer that yet; update the service or the client.",
        "slow_down" => "Wait a moment, then retry.",
        "unavailable" | "internal" => "Silicon Calc is having trouble; wait a moment and retry.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_service_error_shape() {
        let body = br#"{"error":{"code":"invalid_field","message":"The title is too long.","hint":"Use at most 200 characters.","details":{"fields":{"title":"too long"}}}}"#;
        let e = Error::from_response(422, body, None);
        assert_eq!(e.code(), "invalid_field");
        assert_eq!(e.message(), "The title is too long.");
        assert_eq!(e.hint(), Some("Use at most 200 characters."));
        assert_eq!(e.status(), Some(422));
        assert_eq!(e.details().unwrap()["fields"]["title"], "too long");
        let j = e.to_json();
        assert_eq!(j["error"]["details"]["fields"]["title"], "too long");
        assert_eq!(j["error"]["status"], 422);
    }

    #[test]
    fn falls_back_when_the_body_is_not_json() {
        let e = Error::from_response(502, b"<html>bad gateway</html>", None);
        assert_eq!(e.code(), "internal");
        assert!(e.message().contains("502"));
        assert!(e.hint().is_some());
        assert!(e.is_transient());
        let e = Error::from_response(404, b"", None);
        assert!(e.is_not_found());
        assert!(e.message().contains("404"));
    }

    #[test]
    fn keeps_retry_after() {
        let e = Error::from_response(
            429,
            br#"{"error":{"code":"slow_down","message":"Too many edits.","hint":"Wait."}}"#,
            Some(Duration::from_secs(3)),
        );
        assert_eq!(e.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(e.to_json()["error"]["retry_after"], 3);
        assert!(e.is_transient());
    }

    #[test]
    fn signed_out_is_recognised() {
        let e = Error::from_response(401, br#"{"error":{"code":"signed_out","message":"No sign-in.","hint":"calc login"}}"#, None);
        assert!(e.is_signed_out());
        assert!(Error::signed_out("x", "y").is_signed_out());
    }
}
