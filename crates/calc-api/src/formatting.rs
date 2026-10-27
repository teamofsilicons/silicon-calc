//! A preview uses the exact formatter that renders saved cells.
use axum::{Json, Router, routing::post};
use serde::Deserialize;
use serde_json::json;
use crate::{auth::Caller, error::{ApiError, ApiResult}, state::AppState};

#[derive(Deserialize)]
struct Preview { value: calc_engine::Value, code: String }

async fn preview(_caller: Caller, Json(body): Json<Preview>) -> ApiResult<Json<serde_json::Value>> {
    if body.code.len() > 512 || body.value.display().len() > 4096 {
        return Err(ApiError::invalid_field("code", "Use a format up to 512 characters and a value up to 4,096 characters."));
    }
    let (text, color) = calc_engine::numfmt::format_value(&body.value, Some(&body.code));
    Ok(Json(json!({"text": text, "color": color})))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/formats/preview", post(preview)).layer(tower_http::limit::RequestBodyLimitLayer::new(32768))
}
