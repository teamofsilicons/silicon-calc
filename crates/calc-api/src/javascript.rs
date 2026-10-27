//! JavaScript libraries and version-checked browser calculation results. No code runs on the service.
use axum::{Json, Router, extract::{Path, State}, routing::get};
use serde::Deserialize;
use serde_json::{Value, json};
use calc_engine::{Op, javascript::{Function, Answer}};
use crate::{auth::{Caller, MaybeCaller}, code::{both, handle, LinePath}, error::{ApiError, ApiResult, JsonBody}, lines::{ApplyRequest, OpKind}, state::AppState};

pub fn router() -> Router<AppState> {
    let r = Router::new().route("/me/functions", get(library)).route("/me/functions/{name}", axum::routing::put(save_library).delete(delete_library));
    let r = both(r, "/functions", get(sheet_functions).put(save_sheet));
    both(r, "/javascript/results", axum::routing::post(results)).layer(tower_http::limit::RequestBodyLimitLayer::new(8*1024*1024))
}
async fn library(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    let functions: Vec<Value> = sqlx::query_scalar("SELECT definition FROM javascript_functions WHERE account_id=$1 ORDER BY name").bind(caller.uuid).fetch_all(&state.db).await?;
    Ok(Json(json!({"functions": functions})))
}
async fn save_library(State(state): State<AppState>, caller: Caller, Path(name): Path<String>, JsonBody(mut function): JsonBody<Function>) -> ApiResult<Json<Function>> {
    function.name = name.to_ascii_uppercase();
    function.account = Some(caller.uuid.to_string());
    function.validate().map_err(|e| ApiError::invalid_field("function", e))?;
    let mut tx = state.db.begin().await?;
    // Serialize changes to one library so concurrent creates cannot bypass its bound.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))").bind(caller.uuid.to_string()).execute(&mut *tx).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM javascript_functions WHERE account_id=$1 AND name<>$2").bind(caller.uuid).bind(&function.name).fetch_one(&mut *tx).await?;
    if count >= 256 { return Err(ApiError::invalid_field("function", "An account library can hold up to 256 functions. Remove an unused function first.")); }
    sqlx::query("INSERT INTO javascript_functions(account_id,name,definition) VALUES($1,$2,$3) ON CONFLICT(account_id,name) DO UPDATE SET definition=EXCLUDED.definition, updated_at=now()")
        .bind(caller.uuid).bind(&function.name).bind(json!(function)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(function))
}
async fn delete_library(State(state): State<AppState>, caller: Caller, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    sqlx::query("DELETE FROM javascript_functions WHERE account_id=$1 AND name=$2").bind(caller.uuid).bind(name.to_ascii_uppercase()).execute(&state.db).await?;
    Ok(Json(json!({"deleted":true})))
}
async fn sheet_functions(State(state): State<AppState>, MaybeCaller(caller): MaybeCaller, Path(path): Path<LinePath>) -> ApiResult<Json<Value>> {
    let line = handle(&state, caller.as_ref(), &path, false).await?;
    Ok(Json(line.read(|wb, version| {
        let js = wb.javascript.lock().unwrap();
        json!({"version":version, "epoch":js.epoch, "functions":wb.functions, "names":wb.javascript_names(), "calls":js.calls})
    }).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveSheet { function: Function, #[serde(default)] remove: bool, base_version: i64 }
async fn save_sheet(State(state): State<AppState>, caller: Caller, Path(path): Path<LinePath>, JsonBody(request): JsonBody<SaveSheet>) -> ApiResult<Json<Value>> {
    let line = handle(&state, Some(&caller), &path, true).await?;
    let mut function = request.function;
    function.name = function.name.to_ascii_uppercase();
    function.validate().map_err(|e| ApiError::invalid_field("function", e))?;
    if !request.remove && function.account.as_ref().is_some_and(|a| a != &caller.uuid.to_string()) { return Err(ApiError::invalid_field("account", "Import functions from your own account library.")); }
    let result = line.apply_at_version(ApplyRequest { author:caller.uuid, kind:OpKind::Code, summary:Some("updated JavaScript functions".into()), base_version:Some(request.base_version), client_id:None, build:Box::new(move |wb| {
        let mut functions = wb.functions.clone();
        functions.retain(|f| !(f.name.eq_ignore_ascii_case(&function.name) && f.account == function.account));
        if !request.remove { functions.push(function); }
        Ok(Some(Op::SetFunctions { functions }))
    }) }, request.base_version).await?;
    Ok(Json(json!({"version": result.version})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Results { base_version: i64, epoch: u64, results: Vec<(u64, Answer)> }
async fn results(State(state): State<AppState>, caller: Caller, Path(path): Path<LinePath>, JsonBody(request): JsonBody<Results>) -> ApiResult<Json<Value>> {
    if request.results.is_empty() || request.results.len() > 256 { return Err(ApiError::invalid_field("results", "Commit 1 to 256 JavaScript results at a time.")); }
    let total: usize = request.results.iter().flat_map(|(_,a)| &a.rows).map(Vec::len).sum();
    if total > 200_000 { return Err(ApiError::invalid_field("results", "Commit at most 200,000 result cells at a time.")); }
    for (_, answer) in &request.results { answer.validate().map_err(|e| ApiError::invalid_field("results", e))?; }
    let line = handle(&state, Some(&caller), &path, true).await?;
    let result = line.apply_at_version(ApplyRequest { author:caller.uuid, kind:OpKind::Code, summary:Some("recalculated JavaScript formulas".into()), base_version:Some(request.base_version), client_id:None, build:Box::new(move |_| Ok(Some(Op::JavascriptResults { epoch:request.epoch, results:request.results }))) }, request.base_version).await?;
    Ok(Json(json!({"version":result.version})))
}
