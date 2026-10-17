//! One error shape everywhere: `{"error": {"code", "message", "hint", "details"?}}`.

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub hint: String,
    pub details: Option<Value>,
    pub retry_after: Option<u64>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self { status, code, message: message.into(), hint: hint.into(), details: None, retry_after: None }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message, hint)
    }

    pub fn signed_out() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "signed_out",
            "This request has no valid Silicon Accounts sign-in.",
            "Sign in again (calc login), then retry.",
        )
    }

    pub fn not_allowed(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "not_allowed", message, hint)
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        let what = what.into();
        Self::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("{what} doesn't exist, or you don't have access to it."),
            "Check the id, or ask its owner to share it with you.",
        )
    }

    pub fn conflict(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message, hint)
    }

    pub fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_field", message.clone(), "Fix the field and try again.")
            .with_details(json!({"fields": {field: message}}))
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message, hint)
    }

    pub fn too_large(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", message, hint)
    }

    pub fn unavailable(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message, hint)
    }

    pub fn internal(err: impl std::fmt::Display) -> Self {
        tracing::error!(error = %err, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something failed inside Silicon Calc while handling this request.",
            "Try again in a moment; if it keeps failing, the service's log has the reason.",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({"error": {"code": self.code, "message": self.message, "hint": self.hint}});
        if let Some(d) = self.details {
            body["error"]["details"] = d;
        }
        let mut res = (self.status, axum::Json(body)).into_response();
        if let Some(secs) = self.retry_after
            && let Ok(v) = HeaderValue::from_str(&secs.to_string())
        {
            res.headers_mut().insert("retry-after", v);
        }
        res
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => Self::not_found("That item"),
            _ => Self::internal(e),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e)
    }
}

impl From<calc_engine::OpError> for ApiError {
    fn from(e: calc_engine::OpError) -> Self {
        use calc_engine::OpError as E;
        match &e {
            E::NoSuchTab(_) => Self::new(StatusCode::NOT_FOUND, "no_such_tab", e.to_string(), "List the tabs with GET …/tabs and use one of their ids or names."),
            E::TabNameTaken(_) => Self::new(StatusCode::CONFLICT, "tab_name_taken", e.to_string(), "Pick a name no other tab in this sheet has."),
            E::BadTabName => Self::invalid_field("name", e.to_string()),
            E::LastTab => Self::new(StatusCode::CONFLICT, "last_tab", e.to_string(), "Add another tab first, then delete this one."),
            E::Formula { .. } => Self::unprocessable("bad_formula", e.to_string(), "Fix the formula; formulas use Excel's syntax, like =SUM(A1:A9)."),
            E::Invalid(_) => Self::unprocessable("invalid_edit", e.to_string(), "Change the edit and try again."),
        }
    }
}
