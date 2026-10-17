//! The signed-in session: `session.json` in the calc home (mode 0600, in a 0700 directory), refreshed by itself
//! before the access token expires or after the service refuses it.
//!
//! Refresh tokens rotate, and presenting a used one ends the whole sign-in, so refreshing is single-flight across
//! every `calc` process sharing the home: a lock file serializes it, the session is read again under the lock (a
//! process that comes second adopts what the first one saved), and the rotated tokens are written atomically
//! before they are used.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use silicon_accounts_client::{AccountsClient, TokenResponse};
use silicon_calc_client::TokenSource;
use tokio::sync::Mutex;

use crate::error::{CliError, CliResult, SIGN_IN_HINT};

/// Silicon Calc's app id at Silicon Accounts.
pub const APP_ID: &str = "calc";
/// Refresh when the access token has less than this left.
const REFRESH_MARGIN_SECS: i64 = 60;
/// A token whose refresh failed for a passing reason is still used while it has this long left.
const USABLE_WITHOUT_REFRESH_SECS: i64 = 10;

/// The account a session belongs to.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SessionAccount {
    pub uuid: String,
    pub id: String,
    pub kind: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// What `session.json` holds.
#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default = "app_id")]
    pub app_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds.
    pub expires_at: i64,
    /// Unix seconds when the sign-in ends at the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_expires_at: Option<i64>,
    pub account: SessionAccount,
    /// The Silicon Accounts the tokens came from (refreshes and sign-outs go there).
    pub accounts_url: String,
    /// The Silicon Calc service used when signing in.
    #[serde(default)]
    pub api_url: String,
    /// `device` (a Carbon's browser approval), `slt` (a short-lived token) or `silicon` (si:id and STK).
    pub method: String,
    /// Unix seconds.
    pub signed_in_at: i64,
}

fn app_id() -> String {
    APP_ID.to_owned()
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("account", &self.account)
            .field("expires_at", &self.expires_at)
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

pub fn rfc3339(unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

impl Session {
    /// A session from a token response.
    pub fn from_tokens(tokens: &TokenResponse, accounts_url: &str, api_url: &str, method: &str) -> Self {
        let account = tokens
            .account
            .as_ref()
            .map(|a| SessionAccount {
                uuid: a.uuid.clone(),
                id: a.id.clone(),
                kind: a.kind.as_str().to_owned(),
                display_name: a.display_name.clone(),
                email: a.email.clone(),
            })
            .unwrap_or_default();
        Self {
            app_id: APP_ID.to_owned(),
            access_token: tokens.access_token.expose().to_owned(),
            refresh_token: tokens.refresh_token.as_ref().map(|t| t.expose().to_owned()),
            expires_at: now() + i64::try_from(tokens.expires_in).unwrap_or(1800),
            refresh_expires_at: tokens.refresh_token_expires_at.map(|t| t.unix_timestamp()),
            account,
            accounts_url: accounts_url.to_owned(),
            api_url: api_url.to_owned(),
            method: method.to_owned(),
            signed_in_at: now(),
        }
    }

    /// Takes the tokens of a refresh, keeping what they don't say.
    fn apply_refresh(&mut self, tokens: &TokenResponse) {
        self.access_token = tokens.access_token.expose().to_owned();
        if let Some(r) = &tokens.refresh_token {
            self.refresh_token = Some(r.expose().to_owned());
        }
        self.expires_at = now() + i64::try_from(tokens.expires_in).unwrap_or(1800);
        if let Some(at) = tokens.refresh_token_expires_at {
            self.refresh_expires_at = Some(at.unix_timestamp());
        }
        if let Some(a) = &tokens.account {
            if !a.id.is_empty() {
                self.account.id = a.id.clone();
            }
            if !a.display_name.is_empty() {
                self.account.display_name = a.display_name.clone();
            }
            if a.email.is_some() {
                self.account.email = a.email.clone();
            }
        }
    }

    pub fn needs_refresh(&self) -> bool {
        self.expires_at - now() < REFRESH_MARGIN_SECS
    }

    /// True when the sign-in reached its end (it can't be refreshed any more).
    pub fn ended(&self) -> bool {
        self.refresh_expires_at.is_some_and(|t| t <= now()) || (self.refresh_token.is_none() && self.expires_at <= now())
    }
}

/// Where the session lives: `CALC_HOME`, else `$SILICON_HOME/.calc`, else `~/.calc`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// The calc home and what named it.
pub fn home() -> CliResult<(PathBuf, &'static str)> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = var("CALC_HOME") {
        return Ok((dir, "CALC_HOME"));
    }
    if let Some(base) = var("SILICON_HOME") {
        return Ok((base.join(".calc"), "SILICON_HOME"));
    }
    match std::env::home_dir() {
        Some(h) => Ok((h.join(".calc"), "home directory")),
        None => Err(CliError::usage(
            "Can't find a home directory to keep the sign-in in.",
            "Set CALC_HOME to a directory, for example CALC_HOME=/srv/silicons/analyst/.calc.",
        )),
    }
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join("session.json")
    }

    fn ensure_dir(&self) -> CliResult<()> {
        fs::create_dir_all(&self.dir).map_err(|e| {
            CliError::file(format!("Can't create {}: {e}.", self.dir.display()))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }

    /// The stored session, if any. Never writes.
    pub fn load(&self) -> CliResult<Option<Session>> {
        let path = self.path();
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                CliError::new(
                    "session_unreadable",
                    format!("The saved sign-in at {} can't be read ({e}).", path.display()),
                    "Sign in again (calc login --force); that replaces the file.",
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CliError::file(format!("Can't read {}: {e}.", path.display()))),
        }
    }

    /// Writes the session atomically with mode 0600.
    pub fn save(&self, session: &Session) -> CliResult<()> {
        self.ensure_dir()?;
        let path = self.path();
        let tmp = self.dir.join(format!(".session.json.{}.tmp", std::process::id()));
        let json = serde_json::to_vec_pretty(session).map_err(|e| CliError::new("internal", e.to_string(), ""))?;
        let write = || -> std::io::Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            fs::rename(&tmp, &path)
        };
        write().map_err(|e| {
            let _ = fs::remove_file(&tmp);
            CliError::file(format!("Can't save the sign-in to {}: {e}.", path.display()))
        })
    }

    pub fn delete(&self) -> CliResult<bool> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(CliError::file(format!("Can't delete {}: {e}.", self.path().display()))),
        }
    }

    /// Takes the cross-process refresh lock (released when the returned file is dropped).
    pub async fn lock(&self) -> CliResult<File> {
        self.ensure_dir()?;
        let path = self.dir.join("session.lock");
        tokio::task::spawn_blocking(move || -> std::io::Result<File> {
            let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
            file.lock()?;
            Ok(file)
        })
        .await
        .map_err(|e| CliError::new("internal", format!("Waiting for the sign-in lock failed: {e}."), "Retry the command."))?
        .map_err(|e| CliError::file(format!("Can't lock the sign-in: {e}.")))
    }
}

/// A Silicon Accounts client for `url`.
pub fn accounts_client(url: &str) -> CliResult<AccountsClient> {
    let allow_http = std::env::var("ACCOUNTS_ALLOW_INSECURE_HTTP").is_ok_and(|v| v == "1" || v == "true");
    AccountsClient::builder()
        .base_url(url)
        .allow_insecure_http(allow_http)
        .user_agent(format!("calc/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(CliError::from)
}

/// Why there is no token.
pub fn signed_out_error() -> CliError {
    CliError::new("signed_out", "You are not signed in to Silicon Calc.", SIGN_IN_HINT)
}

fn ended_error(id: &str, why: &str) -> CliError {
    CliError::new(
        "signed_out",
        format!("Your Silicon Calc sign-in as {id} ended: {why}"),
        format!("Sign in again. {SIGN_IN_HINT}"),
    )
}

/// The session as a [`TokenSource`] for the client.
pub struct SessionProvider {
    store: Store,
    current: Mutex<Option<Session>>,
}

impl SessionProvider {
    pub fn new(store: Store, session: Option<Session>) -> Self {
        Self { store, current: Mutex::new(session) }
    }

    /// The session as it is now (after any refresh).
    pub async fn session(&self) -> Option<Session> {
        self.current.lock().await.clone()
    }

    /// A valid access token, refreshed first when it is about to expire.
    pub async fn valid_token(&self) -> CliResult<Option<String>> {
        let mut current = self.current.lock().await;
        let Some(session) = current.as_ref() else { return Ok(None) };
        if !session.needs_refresh() {
            return Ok(Some(session.access_token.clone()));
        }
        let rejected = session.access_token.clone();
        let still_valid = session.expires_at - now() > USABLE_WITHOUT_REFRESH_SECS;
        match self.refresh_locked(&mut current, &rejected).await {
            Ok(fresh) => Ok(Some(fresh.access_token)),
            // Silicon Accounts is briefly unreachable but the token still works: use it.
            Err(e) if e.code == "network" && still_valid => Ok(Some(rejected)),
            Err(e) => Err(e),
        }
    }

    /// Refreshes because `rejected` expired or was refused. Single-flight in this process (the mutex) and across
    /// processes (the lock file); whoever comes second adopts the tokens the first one saved.
    async fn refresh_locked(&self, current: &mut Option<Session>, rejected: &str) -> CliResult<Session> {
        let _lock = self.store.lock().await?;
        let on_disk = self.store.load()?.ok_or_else(signed_out_error)?;
        if on_disk.access_token != rejected && !on_disk.needs_refresh() {
            *current = Some(on_disk.clone());
            return Ok(on_disk);
        }
        let Some(refresh_token) = on_disk.refresh_token.clone() else {
            return Err(ended_error(&on_disk.account.id, "the access token expired and the sign-in has no refresh token."));
        };
        let accounts = accounts_client(&on_disk.accounts_url)?;
        match accounts.refresh_app_public_client(&on_disk.app_id, &refresh_token).await {
            Ok(tokens) => {
                let mut session = on_disk;
                session.apply_refresh(&tokens);
                self.store.save(&session)?;
                *current = Some(session.clone());
                Ok(session)
            }
            Err(e) if e.as_oauth().is_some_and(|o| o.error == "invalid_grant") => {
                // The sign-in ended: revoked, reused, the account removed Calc's access, or 900 days passed.
                let _ = self.store.delete();
                *current = None;
                let why = e.as_oauth().map(|o| o.message()).unwrap_or_else(|| e.message());
                Err(ended_error(&on_disk.account.id, &format!("Silicon Accounts refused to refresh it ({why}).")))
            }
            Err(e) => Err(CliError::from(e)),
        }
    }
}

fn to_client_error(e: CliError) -> silicon_calc_client::Error {
    if e.code == "network" {
        // Keep it recognisable as a passing failure.
        return silicon_calc_client::Error::Auth { code: "network".into(), message: e.message, hint: e.hint.unwrap_or_default() };
    }
    silicon_calc_client::Error::Auth { code: e.code, message: e.message, hint: e.hint.unwrap_or_default() }
}

impl TokenSource for SessionProvider {
    fn token(&self) -> BoxFuture<'_, silicon_calc_client::Result<Option<String>>> {
        Box::pin(async move { self.valid_token().await.map_err(to_client_error) })
    }

    fn refresh<'a>(&'a self, rejected: &'a str) -> BoxFuture<'a, silicon_calc_client::Result<Option<String>>> {
        Box::pin(async move {
            let mut current = self.current.lock().await;
            match current.as_ref() {
                None => Ok(None),
                Some(s) if s.access_token != rejected => Ok(Some(s.access_token.clone())),
                Some(_) => self.refresh_locked(&mut current, rejected).await.map(|s| Some(s.access_token)).map_err(to_client_error),
            }
        })
    }
}

/// Ends a sign-in at Silicon Accounts (RFC 7009 revoke with Calc's `client_id`).
///
/// `silicon-accounts-client` 0.6's `revoke_public_client` only takes Silicon Accounts' own client ids, so the
/// revoke for the `calc` public client is sent directly.
pub async fn revoke(accounts_url: &str, app_id: &str, token: &str) -> CliResult<()> {
    let url = format!("{}/v1/oauth/revoke", accounts_url.trim_end_matches('/'));
    let hint = if token.starts_with("sar_") { "refresh_token" } else { "access_token" };
    let response = reqwest::Client::new()
        .post(&url)
        .header("user-agent", format!("calc/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(15))
        .form(&[("token", token), ("token_type_hint", hint), ("client_id", app_id)])
        .send()
        .await
        .map_err(|e| {
            CliError::new(
                "network",
                format!("Can't reach Silicon Accounts at {accounts_url} to end the sign-in: {e}."),
                "Check the connection; the sign-in can also be ended on the account site under Apps.",
            )
        })?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let detail = body
        .pointer("/error_description")
        .or_else(|| body.pointer("/error/message"))
        .or_else(|| body.pointer("/error"))
        .and_then(|v| v.as_str())
        .unwrap_or("no explanation")
        .to_owned();
    Err(CliError::new(
        "revoke_failed",
        format!("Silicon Accounts refused to end the sign-in (HTTP {status}): {detail}"),
        "It was removed from this machine anyway; it can also be ended on the account site under Apps.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            app_id: APP_ID.into(),
            access_token: "at".into(),
            refresh_token: Some("sar_x".into()),
            expires_at: now() + 1800,
            refresh_expires_at: None,
            account: SessionAccount { uuid: "u".into(), id: "si:analyst".into(), kind: "silicon".into(), ..Default::default() },
            accounts_url: "http://127.0.0.1:9989".into(),
            api_url: "http://127.0.0.1:8730".into(),
            method: "slt".into(),
            signed_in_at: now(),
        }
    }

    #[test]
    fn saves_atomically_and_privately() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("calc"));
        assert!(store.load().unwrap().is_none());
        store.save(&sample()).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.account.id, "si:analyst");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dmode = fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
            assert_eq!(dmode, 0o700);
        }
        let debug = format!("{loaded:?}");
        assert!(debug.contains("si:analyst"));
        assert!(!debug.contains("sar_x"), "Debug must never print tokens");
        assert!(store.delete().unwrap());
        assert!(!store.delete().unwrap());
    }

    #[test]
    fn unreadable_sessions_say_so() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        fs::write(store.path(), "{not json").unwrap();
        let e = store.load().unwrap_err();
        assert_eq!(e.code, "session_unreadable");
    }

    #[tokio::test]
    async fn adopts_tokens_another_process_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let mut stale = sample();
        stale.expires_at = now() - 10;
        let mut fresh = sample();
        fresh.access_token = "at2".into();
        store.save(&fresh).unwrap();
        let provider = SessionProvider::new(store, Some(stale));
        // No network: the fresh token on disk is adopted.
        assert_eq!(provider.valid_token().await.unwrap().as_deref(), Some("at2"));
        let t = TokenSource::refresh(&provider, "at").await.unwrap();
        assert_eq!(t.as_deref(), Some("at2"));
    }

    #[test]
    fn knows_when_a_sign_in_ended() {
        let mut s = sample();
        assert!(!s.ended());
        s.refresh_expires_at = Some(now() - 1);
        assert!(s.ended());
        let mut s = sample();
        s.refresh_token = None;
        s.expires_at = now() - 1;
        assert!(s.ended());
    }
}
