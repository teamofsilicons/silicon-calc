//! Explicit, account-scoped Ting consent. Reading status never registers a recipient.
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::get};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use silicon_accounts_client::{IssueAppVerification, IssueUserVerification};

use crate::{
    auth::Caller,
    error::{ApiError, ApiResult, JsonBody},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/me/notifications",
        get(status).post(enable).delete(disable),
    )
}

#[derive(Serialize)]
struct Preferences {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Enable {}

fn unavailable() -> ApiError {
    ApiError::unavailable(
        "Calc couldn't confirm your notification settings with Silicon Ting.",
        "Try again in a moment. No notification setting is assumed after an error.",
    )
}

/// No redirects: a proof may only be sent to the configured service origin.
fn http(state: &AppState) -> ApiResult<(reqwest::Client, String)> {
    let url = state.cfg.ting_url.as_deref().ok_or_else(unavailable)?;
    let parsed = url::Url::parse(url).map_err(|_| unavailable())?;
    let loopback = matches!(
        parsed.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if !(parsed.scheme() == "https"
        || (state.cfg.allow_http && loopback && parsed.scheme() == "http"))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(unavailable());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable())?;
    Ok((client, url.trim_end_matches('/').to_owned()))
}

async fn proof(state: &AppState, caller: Option<&Caller>, scope: &str) -> ApiResult<String> {
    let app = state
        .accounts
        .client()
        .as_app(state.accounts.app_id(), state.accounts.app_secret());
    let key = format!("calc-notification-consent-{}", uuid::Uuid::now_v7());
    let issued = match caller {
        Some(caller) => {
            app.issue_user_verification(
                &IssueUserVerification {
                    subject_token: caller.token.to_string(),
                    receiving_app: state.cfg.ting_app_id.clone(),
                    scopes: vec![scope.into()],
                    access_ttl_seconds: Some(60),
                },
                Some(&key),
            )
            .await
        }
        None => {
            app.issue_app_verification(
                &IssueAppVerification {
                    receiving_app: state.cfg.ting_app_id.clone(),
                    scopes: vec![scope.into()],
                    access_ttl_seconds: Some(60),
                },
                Some(&key),
            )
            .await
        }
    }
    .map_err(|_| unavailable())?;
    // Accounts is the authority; reject a mismatched receipt before forwarding its credential.
    if issued
        .receiving_app
        .as_deref()
        .is_some_and(|app| app != state.cfg.ting_app_id)
        || issued
            .issuing_app
            .as_deref()
            .is_some_and(|app| app != state.accounts.app_id())
        || (!issued.scopes.is_empty() && !issued.scopes.iter().any(|s| s == scope))
        || caller.is_some_and(|c| {
            issued
                .user
                .as_ref()
                .is_none_or(|u| u.uuid != c.uuid.to_string())
        })
    {
        return Err(unavailable());
    }
    let token = issued.proof_token.expose().trim();
    if token.is_empty() {
        return Err(unavailable());
    }
    Ok(token.to_owned())
}

async fn request(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: &str,
    body: Value,
) -> ApiResult<Value> {
    let response = client
        .post(format!("{base}/v1/{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    // These are small receipts. Never return or log upstream bodies, which may contain credentials.
    if response.content_length().is_some_and(|n| n > 1_048_576) {
        return Err(unavailable());
    }
    let bytes = response.bytes().await.map_err(|_| unavailable())?;
    if bytes.len() > 1_048_576 {
        return Err(unavailable());
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

#[derive(Deserialize)]
struct Grant {
    id: String,
    app_id: String,
    #[serde(rename = "for")]
    recipient: String,
    active: bool,
}

fn grant(value: Value, state: &AppState, caller: &Caller) -> ApiResult<Grant> {
    let grant: Grant = serde_json::from_value(value).map_err(|_| unavailable())?;
    if grant.id.is_empty()
        || grant.app_id != state.accounts.app_id()
        || (grant.recipient != caller.uuid.to_string() && grant.recipient != caller.id)
    {
        return Err(unavailable());
    }
    Ok(grant)
}

async fn grants(
    state: &AppState,
    caller: &Caller,
    client: &reqwest::Client,
    base: &str,
) -> ApiResult<Vec<Grant>> {
    let token = proof(state, None, "subscriptions.read").await?;
    let mut cursor = None;
    let mut grants = vec![];
    for _ in 0..20 {
        let mut body = json!({"app_id":state.accounts.app_id(),"for":caller.uuid,"limit":100});
        if let Some(cursor) = &cursor {
            body["cursor"] = json!(cursor);
        }
        let mut answer = request(client, base, "subscriptions/query", &token, body).await?;
        let items = answer
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .ok_or_else(unavailable)?;
        for item in items.drain(..) {
            grants.push(grant(item, state, caller)?);
        }
        match answer.get("next_cursor") {
            None | Some(Value::Null) => return Ok(grants),
            Some(Value::String(next)) if !next.is_empty() && cursor.as_ref() != Some(next) => {
                cursor = Some(next.clone())
            }
            _ => return Err(unavailable()),
        }
    }
    Err(unavailable())
}

async fn status(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Preferences>> {
    let (client, base) = http(&state)?;
    Ok(Json(Preferences {
        enabled: grants(&state, &caller, &client, &base)
            .await?
            .iter()
            .any(|g| g.active),
    }))
}

async fn enable(
    State(state): State<AppState>,
    caller: Caller,
    JsonBody(_): JsonBody<Enable>,
) -> ApiResult<Json<Preferences>> {
    let (client, base) = http(&state)?;
    // Only this explicit action exchanges the signed-in recipient's token for delegation.
    let token = proof(&state, Some(&caller), "subscriptions.register").await?;
    let answer = request(
        &client,
        &base,
        "subscriptions",
        &token,
        json!({"app_id":state.accounts.app_id(),"for":caller.uuid}),
    )
    .await?;
    if !grant(answer, &state, &caller)?.active {
        return Err(unavailable());
    }
    Ok(Json(Preferences { enabled: true }))
}

async fn disable(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Preferences>> {
    let (client, base) = http(&state)?;
    let active = grants(&state, &caller, &client, &base)
        .await?
        .into_iter()
        .filter(|g| g.active)
        .collect::<Vec<_>>();
    if !active.is_empty() {
        let token = proof(&state, None, "subscriptions.revoke").await?;
        for grant in active {
            // IDs come only from the caller-scoped query, never from a caller-supplied recipient or grant.
            let answer = request(
                &client,
                &base,
                "subscriptions/revoke",
                &token,
                json!({"app_id":state.accounts.app_id(),"id":grant.id}),
            )
            .await?;
            if answer["id"].as_str() != Some(&grant.id) || answer["active"].as_bool() != Some(false)
            {
                return Err(unavailable());
            }
        }
    }
    Ok(Json(Preferences { enabled: false }))
}
