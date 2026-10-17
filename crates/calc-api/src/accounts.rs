//! Silicon Accounts: verifying access tokens (locally, against the JWKS), and turning uuids into the ids and names
//! people see (cached, with the app's credentials).

use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountKind, AccountsClient, Jwks};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

/// An account as every answer shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub uuid: Uuid,
    /// `c:ada`, `si:analyst`. Empty for a deleted account.
    pub id: String,
    /// `carbon` or `silicon`.
    pub kind: String,
    pub display_name: String,
    pub pfp_url: String,
}

impl Account {
    /// What to show for an account Accounts no longer knows.
    pub fn unknown(uuid: Uuid) -> Self {
        Self { uuid, id: String::new(), kind: "carbon".into(), display_name: "Deleted account".into(), pfp_url: String::new() }
    }
}

const LOOKUP_TTL: Duration = Duration::from_secs(300);
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);

struct Jw {
    jwks: Option<Jwks>,
    fetched: Option<Instant>,
}

#[derive(Clone)]
pub struct Accounts {
    client: Arc<AccountsClient>,
    app_id: String,
    app_secret: Arc<str>,
    issuer: String,
    jwks: Arc<RwLock<Jw>>,
    cache: Arc<Mutex<lru::LruCache<Uuid, (Account, Instant)>>>,
    by_id: Arc<Mutex<lru::LruCache<String, (Uuid, Instant)>>>,
}

/// What a verified token says about its account.
#[derive(Clone, Debug)]
pub struct TokenAccount {
    pub uuid: Uuid,
    pub id: String,
    pub kind: String,
}

impl Accounts {
    pub fn new(api_url: &str, issuer: &str, app_id: &str, app_secret: &str, allow_http: bool) -> anyhow::Result<Self> {
        let client = AccountsClient::builder()
            .base_url(api_url)
            .allow_insecure_http(allow_http)
            .user_agent("silicon-calc")
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client: Arc::new(client),
            app_id: app_id.to_owned(),
            app_secret: Arc::from(app_secret),
            issuer: issuer.to_owned(),
            jwks: Arc::new(RwLock::new(Jw { jwks: None, fetched: None })),
            cache: Arc::new(Mutex::new(lru::LruCache::new(NonZeroUsize::new(20_000).expect("non-zero")))),
            by_id: Arc::new(Mutex::new(lru::LruCache::new(NonZeroUsize::new(20_000).expect("non-zero")))),
        })
    }

    pub fn client(&self) -> &AccountsClient {
        &self.client
    }

    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    pub fn app_secret(&self) -> &str {
        &self.app_secret
    }

    async fn jwks(&self, force: bool) -> ApiResult<Jwks> {
        {
            let j = self.jwks.read().await;
            if let (Some(k), false) = (&j.jwks, force) {
                return Ok(k.clone());
            }
            if force && j.fetched.is_some_and(|t| t.elapsed() < JWKS_MIN_REFRESH) {
                if let Some(k) = &j.jwks {
                    return Ok(k.clone());
                }
            }
        }
        let fresh = self.client.jwks().await.map_err(|e| {
            tracing::warn!(error = %e, "fetching the Silicon Accounts JWKS failed");
            ApiError::unavailable(
                "Silicon Calc can't reach Silicon Accounts to check sign-ins right now.",
                "Try again in a moment.",
            )
        })?;
        let mut j = self.jwks.write().await;
        j.jwks = Some(fresh.clone());
        j.fetched = Some(Instant::now());
        Ok(fresh)
    }

    /// Checks an access token: signature, expiry, audience (`calc`) and issuer.
    pub async fn verify(&self, token: &str) -> ApiResult<TokenAccount> {
        let app = self.client.as_app(self.app_id.clone(), self.app_secret.to_string());
        let mut jwks = self.jwks(false).await?;
        let claims = match app.verify_access_token_locally(&jwks, token) {
            Ok(c) => c,
            Err(_) => {
                // A key rotation shows up as an unknown key: fetch the keys once more.
                jwks = self.jwks(true).await?;
                app.verify_access_token_locally(&jwks, token).map_err(|_| ApiError::signed_out())?
            }
        };
        if !claims.iss.is_empty() && claims.iss.trim_end_matches('/') != self.issuer {
            return Err(ApiError::signed_out());
        }
        let uuid = Uuid::parse_str(&claims.sub).map_err(|_| ApiError::signed_out())?;
        let kind = match claims.kind {
            Some(AccountKind::Silicon) => "silicon",
            _ => "carbon",
        };
        Ok(TokenAccount { uuid, id: claims.id.unwrap_or_default(), kind: kind.to_owned() })
    }

    /// An account by uuid (cached for five minutes).
    pub async fn get(&self, uuid: Uuid) -> Account {
        if let Some((a, t)) = self.cache.lock().await.get(&uuid).cloned()
            && t.elapsed() < LOOKUP_TTL
        {
            return a;
        }
        let app = self.client.as_app(self.app_id.clone(), self.app_secret.to_string());
        match app.lookup(&uuid.to_string()).await {
            Ok(s) => {
                let a = Account {
                    uuid,
                    id: s.id,
                    kind: if matches!(s.kind, AccountKind::Silicon) { "silicon".into() } else { "carbon".into() },
                    display_name: s.display_name,
                    pfp_url: s.pfp_url,
                };
                self.remember(&a).await;
                a
            }
            Err(e) => {
                if !e.is_not_found() {
                    tracing::warn!(error = %e, %uuid, "account lookup failed");
                }
                Account::unknown(uuid)
            }
        }
    }

    pub async fn get_many(&self, uuids: impl IntoIterator<Item = Uuid>) -> Vec<Account> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for u in uuids {
            if seen.insert(u) {
                out.push(self.get(u).await);
            }
        }
        out
    }

    /// An account by its `c:`/`si:` id, or None when no account has it.
    pub async fn by_public_id(&self, id: &str) -> ApiResult<Option<Account>> {
        let key = id.trim().to_ascii_lowercase();
        if !(key.starts_with("c:") || key.starts_with("si:")) {
            return Err(ApiError::invalid_field("id", format!("{id} is not an account id; ids look like c:ada or si:analyst.")));
        }
        if let Some((u, t)) = self.by_id.lock().await.get(&key).copied()
            && t.elapsed() < LOOKUP_TTL
        {
            return Ok(Some(self.get(u).await));
        }
        let app = self.client.as_app(self.app_id.clone(), self.app_secret.to_string());
        match app.lookup_by_id(&key).await {
            Ok(s) => {
                let Ok(uuid) = Uuid::parse_str(&s.uuid) else { return Ok(None) };
                let a = Account {
                    uuid,
                    id: s.id,
                    kind: if matches!(s.kind, AccountKind::Silicon) { "silicon".into() } else { "carbon".into() },
                    display_name: s.display_name,
                    pfp_url: s.pfp_url,
                };
                self.remember(&a).await;
                Ok(Some(a))
            }
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => {
                tracing::warn!(error = %e, id = %key, "account lookup by id failed");
                Err(ApiError::unavailable("Silicon Calc can't look accounts up in Silicon Accounts right now.", "Try again in a moment."))
            }
        }
    }

    async fn remember(&self, a: &Account) {
        self.cache.lock().await.put(a.uuid, (a.clone(), Instant::now()));
        if !a.id.is_empty() {
            self.by_id.lock().await.put(a.id.to_ascii_lowercase(), (a.uuid, Instant::now()));
        }
    }
}
