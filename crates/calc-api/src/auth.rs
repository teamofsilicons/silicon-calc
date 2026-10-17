//! Who is calling: the bearer token, verified, as a `Caller`.
//!
//! The first time Calc sees an account (and every ten minutes after) it records it, with its verified email when the
//! sign-in shared one, and turns any pending email invites for that address into memberships.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use axum::{extract::FromRequestParts, http::request::Parts};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{error::ApiError, state::AppState};

/// A signed-in account.
#[derive(Clone, Debug)]
pub struct Caller {
    pub uuid: Uuid,
    /// The `c:`/`si:` id the token carried.
    pub id: String,
    pub kind: String,
    pub token: Arc<str>,
}

impl Caller {
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }
}

/// A caller that may be signed out (public sheets).
#[derive(Clone, Debug)]
pub struct MaybeCaller(pub Option<Caller>);

fn bearer(parts: &Parts) -> Option<String> {
    let h = parts.headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let t = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer "))?.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// The token for an EventSource, which can't send headers: `?access_token=` is accepted on `/events` only.
fn query_token(parts: &Parts) -> Option<String> {
    if !parts.uri.path().ends_with("/events") {
        return None;
    }
    let q = parts.uri.query()?;
    q.split('&').find_map(|kv| kv.strip_prefix("access_token=")).map(|t| t.to_owned())
}

static SEEN: LazyLock<Mutex<HashMap<Uuid, Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
const SEEN_REFRESH: Duration = Duration::from_secs(600);

async fn record_seen(state: &AppState, caller: &Caller) {
    {
        let seen = SEEN.lock().await;
        if seen.get(&caller.uuid).is_some_and(|t| t.elapsed() < SEEN_REFRESH) {
            return;
        }
    }
    SEEN.lock().await.insert(caller.uuid, Instant::now());
    let known: Option<(Option<String>,)> = sqlx::query_as("select email::text from known_accounts where uuid = $1")
        .bind(caller.uuid)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
    let account = state.accounts.get(caller.uuid).await;
    let mut email = known.as_ref().and_then(|k| k.0.clone());
    if email.is_none() {
        // The sign-in shares a verified email when the account agreed to (Carbons, through the email scope).
        let app = state.accounts.client().as_app(state.accounts.app_id().to_owned(), state.accounts.app_secret().to_owned());
        if let Ok(info) = app.userinfo(&caller.token).await
            && info.account.email_verified.unwrap_or(false)
        {
            email = info.account.email.filter(|e| !e.is_empty());
        }
    }
    let res = sqlx::query(
        "insert into known_accounts (uuid, public_id, kind, display_name, pfp_url, email)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (uuid) do update set public_id = excluded.public_id, kind = excluded.kind,
           display_name = excluded.display_name, pfp_url = excluded.pfp_url,
           email = coalesce(excluded.email, known_accounts.email), last_seen_at = now()",
    )
    .bind(caller.uuid)
    .bind(if account.id.is_empty() { caller.id.clone() } else { account.id.clone() })
    .bind(&caller.kind)
    .bind(&account.display_name)
    .bind(&account.pfp_url)
    .bind(&email)
    .execute(&state.db)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "recording an account failed");
        return;
    }
    if let Some(email) = email {
        // Invites to this address become memberships, and each new member hears about it through Ting.
        if let Err(e) = crate::sharing::accept_invites(state, caller.uuid, &email).await {
            tracing::warn!(error = %e.message, "turning invites into memberships failed");
        }
    }
}

async fn caller_from(parts: &Parts, state: &AppState) -> Result<Option<Caller>, ApiError> {
    let Some(token) = bearer(parts).or_else(|| query_token(parts)) else { return Ok(None) };
    let t = state.accounts.verify(&token).await?;
    let caller = Caller { uuid: t.uuid, id: t.id, kind: t.kind, token: Arc::from(token.as_str()) };
    record_seen(state, &caller).await;
    Ok(Some(caller))
}

impl FromRequestParts<AppState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(c) = parts.extensions.get::<Caller>() {
            return Ok(c.clone());
        }
        let c = caller_from(parts, state).await?.ok_or_else(ApiError::signed_out)?;
        parts.extensions.insert(c.clone());
        Ok(c)
    }
}

impl FromRequestParts<AppState> for MaybeCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        if let Some(c) = parts.extensions.get::<Caller>() {
            return Ok(Self(Some(c.clone())));
        }
        let c = caller_from(parts, state).await?;
        if let Some(c) = &c {
            parts.extensions.insert(c.clone());
        }
        Ok(Self(c))
    }
}
