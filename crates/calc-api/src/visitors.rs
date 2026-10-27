//! Optional public-link email collection. An entered address is a claim, never authentication.
use crate::{
    perms::{self, SheetRow},
    auth::{Caller, MaybeCaller},
    error::{ApiError, ApiResult, JsonBody},
    state::AppState,
};
use axum::{
    Json, Router,
    routing::{get, post},
    extract::DefaultBodyLimit,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn required() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "email_required",
        "Enter your email to view this file.",
        "The file owner will see your email. No sign-in or email verification is required.",
    )
}
fn normalize_email(value: &str) -> ApiResult<String> {
    let value = value.trim().to_lowercase();
    let valid = value.len() <= 254
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
        && value.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !domain.contains('@')
        });
    if !valid {
        return Err(ApiError::invalid_field("email", "Enter a valid email address."));
    }
    Ok(value)
}
fn hash(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
async fn account_email(state: &AppState, caller: &Caller) -> ApiResult<Option<String>> {
    // Only an Accounts-provided email can be labelled as an account email.
    let email: Option<String> = sqlx::query_scalar("select email::text from known_accounts where uuid=$1")
        .bind(caller.uuid).fetch_optional(&state.db).await?.flatten();
    if let Some(email) = email.filter(|e| !e.is_empty()) { return Ok(Some(email.to_lowercase())); }
    let app = state.accounts.client().as_app(state.accounts.app_id().to_owned(), state.accounts.app_secret().to_owned());
    if let Ok(info) = app.userinfo(&caller.token).await {
        if let Some(email) = info.account.email.filter(|e| !e.is_empty()) {
            // Do not put unverified/entered emails in known_accounts: that table also accepts invitations.
            return Ok(Some(email.trim().to_lowercase()));
        }
    }
    Ok(None)
}
async fn record(
    state: &AppState,
    doc: &SheetRow,
    caller: Option<&Caller>,
    email: &str,
    source: &str,
) -> ApiResult<()> {
    let key = if source == "account" {
        format!("account:{}", caller.unwrap().uuid)
    } else {
        format!("email:{email}")
    };
    sqlx::query("insert into sheet_viewers (sheet_id, viewer_key, email, account_uuid, email_source) values ($1,$2,$3,$4,$5)
        on conflict (sheet_id, viewer_key) do update set email = excluded.email, account_uuid = excluded.account_uuid,
        last_viewed_at = now() where sheet_viewers.last_viewed_at < now() - interval '1 minute'")
        .bind(&doc.id).bind(key).bind(email).bind(caller.map(|c| &c.uuid)).bind(source).execute(&state.db).await?;
    Ok(())
}
pub async fn ensure(
    state: &AppState,
    caller: Option<&Caller>,
    doc: &SheetRow,
    headers: &HeaderMap,
    log: bool,
) -> ApiResult<()> {
    if !doc.require_email
        || !doc.public_enabled
        || caller.is_some_and(|c| c.uuid == doc.owner)
    {
        return Ok(());
    }
    if let Some(caller) = caller {
        if let Some(email) = account_email(state, caller).await? {
            if log {
                record(state, doc, Some(caller), &email, "account").await?;
            }
            return Ok(());
        }
    }
    let hashes: Vec<String> = headers
        .get("x-calc-view-grants")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .split(',')
        .take(32)
        .filter(|v| v.len() >= 20 && v.len() <= 100)
        .map(hash)
        .collect();
    let email: Option<String> = sqlx::query_scalar("select email from sheet_view_grants where sheet_id=$1 and token_hash=any($2) and gate_version=$3 and expires_at > now() limit 1")
        .bind(&doc.id).bind(&hashes).bind(doc.email_gate_version).fetch_optional(&state.db).await?;
    let Some(email) = email else {
        return Err(required());
    };
    if log {
        record(state, doc, caller, &email, "entered").await?;
    }
    Ok(())
}

/// Applied by both auth extractors, including downloads, copies and historical versions.
pub async fn guard(state: &AppState, caller: Option<&Caller>, path: &str, headers: &HeaderMap, method: &axum::http::Method) -> ApiResult<()> {
    let path = path.strip_prefix("/v1").unwrap_or(path);
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    if parts.len() < 2 || parts[0] != "sheets" || parts[1] == "import" { return Ok(()); }
    if parts[1].contains('%') { return Err(ApiError::invalid_field("sheet", "Use the unencoded sheet identifier.")); }
    if parts.get(2).is_some_and(|p| matches!(*p, "visits" | "viewers" | "public" | "sharing")) { return Ok(()); }
    let access = perms::sheet_access(state, caller, parts[1]).await?;
    ensure(state, caller, &access.sheet, headers, method == axum::http::Method::GET).await
}

/// Rechecked on live events so changing the public-link policy also closes existing streams.
pub async fn live_access(state: &AppState, caller: Option<&Caller>, id: &str, headers: &HeaderMap) -> ApiResult<()> {
    let access = perms::sheet_access(state, caller, id).await?;
    ensure(state, caller, &access.sheet, headers, false).await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sheets/{sheet}/visits", post(visit).layer(DefaultBodyLimit::max(1024)))
        .route("/sheets/{sheet}/viewers", get(list))
}
#[derive(Deserialize)]
pub struct VisitBody {
    #[serde(default)]
    email: String,
}
pub async fn visit(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<VisitBody>,
) -> ApiResult<Json<Value>> {
    let access = perms::sheet_access(&state, caller.as_ref(), &id).await?;
    let doc = access.sheet;
    if !doc.require_email || !doc.public_enabled || access.in_trash {
        return Err(ApiError::conflict(
            "This file isn't collecting viewer emails.", "Open the file again.",
        ));
    }
    if caller
        .as_ref()
        .is_some_and(|caller| caller.uuid == doc.owner)
    {
        return Ok(Json(json!({"ok":true})));
    }
    if let Some(caller) = &caller {
        if let Some(email) = account_email(&state, caller).await? {
            if caller.uuid != doc.owner {
                record(&state, &doc, Some(caller), &email, "account").await?;
            }
            return Ok(Json(json!({"ok":true})));
        }
    }
    let email = normalize_email(&body.email)?;
    let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    sqlx::query("delete from sheet_view_grants where sheet_id=$1 and expires_at < now()")
        .bind(&id)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "insert into sheet_view_grants (token_hash,sheet_id,email,gate_version) values ($1,$2,$3,$4)",
    )
    .bind(hash(&token))
    .bind(&id)
    .bind(&email)
    .bind(doc.email_gate_version)
    .execute(&state.db)
    .await?;
    record(&state, &doc, caller.as_ref(), &email, "entered").await?;
    Ok(Json(json!({"ok":true,"view_grant":token})))
}
#[derive(Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    offset: i64,
}
pub async fn list(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let access = perms::sheet_access(&state, Some(&caller), &id).await?;
    if access.sheet.owner != caller.uuid { return Err(ApiError::not_allowed("Only the owner can see viewers.", "Ask the file owner.")); }
    let offset = query.offset.clamp(0, 1_000_000);
    let rows: Vec<(String, Option<uuid::Uuid>, String, time::OffsetDateTime, time::OffsetDateTime)> = sqlx::query_as("select email, account_uuid, email_source, first_viewed_at, last_viewed_at from sheet_viewers where sheet_id=$1 order by last_viewed_at desc, viewer_key limit 50 offset $2")
        .bind(&id).bind(offset).fetch_all(&state.db).await?;
    let total: i64 = sqlx::query_scalar("select count(*) from sheet_viewers where sheet_id=$1")
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let fmt = &time::format_description::well_known::Rfc3339;
    let viewers: Vec<_> = rows.into_iter().map(|r| json!({"email":r.0,"email_source":r.2,"first_viewed_at":r.3.format(fmt).ok(),"last_viewed_at":r.4.format(fmt).ok()})).collect();
    Ok(Json(
        json!({"viewers":viewers,"total":total,"next_offset": if offset+50<total {Some(offset+50)} else {None}}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn email_is_a_validated_claim_not_authentication() {
        assert_eq!(
            normalize_email(" Person@Example.com ").unwrap(),
            "person@example.com"
        );
        for bad in [
            "",
            "bad",
            "x@@test.com",
            "x@.com",
            "x@test.",
            "x\n@test.com",
            "x@y com",
        ] {
            assert!(normalize_email(bad).is_err());
        }
    }
}
