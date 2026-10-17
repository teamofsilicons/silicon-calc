//! Settings, all from the environment, checked once at start.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context as _, bail};

#[derive(Clone, Debug)]
pub struct Config {
    /// `CALC_BIND` (default `127.0.0.1:8730`).
    pub bind: SocketAddr,
    /// `CALC_DATABASE_URL`.
    pub database_url: String,
    /// `CALC_ACCOUNTS_URL`: Silicon Accounts' public origin, the issuer of every token.
    pub accounts_url: String,
    /// `CALC_ACCOUNTS_API_URL`: where this service reaches the Accounts API (defaults to `accounts_url`).
    pub accounts_api_url: String,
    /// `CALC_APP_ID` (default `calc`).
    pub app_id: String,
    /// `CALC_APP_SECRET`: the app secret (`sa_app_…`).
    pub app_secret: String,
    /// `CALC_WEB_URL`: the web app's origin, for links in notifications (default `http://127.0.0.1:8731`).
    pub web_url: String,
    /// `CALC_BLOB_DIR`: where operation payloads and snapshots are kept (default `.dev/blobs`).
    pub blob_dir: PathBuf,
    /// `CALC_TING_URL`: Silicon Ting's origin. Without it notifications are kept and marked as not sent.
    pub ting_url: Option<String>,
    /// `CALC_TING_APP_ID` (default `ting`): the receiving app of Calc's proof for Ting.
    pub ting_app_id: String,
    /// `CALC_RUNNER_PYTHON`: the Python with pandas the code runner uses (default `python3`).
    pub runner_python: String,
    /// `CALC_RUNNER_NODE` (default `node`).
    pub runner_node: String,
    /// `CALC_CODE_SANDBOX`: `auto` (default), `macos`, `bwrap`, or `off` (local development only: code may reach
    /// the network and the disk).
    pub code_sandbox: String,
    /// `CALC_CODE_TIMEOUT_SECONDS` (default 60).
    pub code_timeout: Duration,
    /// `CALC_MAX_UPLOAD_BYTES` (default 500 MB).
    pub max_upload_bytes: usize,
    /// `CALC_SNAPSHOT_EVERY` (default 200): operations between snapshots.
    pub snapshot_every: u64,
    /// `CALC_LINE_IDLE_SECONDS` (default 600): an open line with no viewers is unloaded after this.
    pub line_idle: Duration,
    /// `CALC_ALLOW_HTTP_ACCOUNTS`: allow `http://` Accounts URLs (local stacks).
    pub allow_http: bool,
}

/// The most cells one edit request may change (docs/API.md, `/v1/meta` limits).
pub const MAX_CELLS_PER_EDIT: usize = 200_000;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = var("CALC_BIND").unwrap_or_else(|| "127.0.0.1:8730".into());
        let bind: SocketAddr = bind.parse().with_context(|| format!("CALC_BIND={bind} is not an address like 127.0.0.1:8730"))?;
        let database_url = var("CALC_DATABASE_URL").context("CALC_DATABASE_URL is not set: give the Postgres URL")?;
        let accounts_url = var("CALC_ACCOUNTS_URL")
            .context("CALC_ACCOUNTS_URL is not set: give Silicon Accounts' public origin (https://accounts.teamofsilicons.com)")?;
        let accounts_url = accounts_url.trim_end_matches('/').to_owned();
        let accounts_api_url = var("CALC_ACCOUNTS_API_URL").unwrap_or_else(|| accounts_url.clone()).trim_end_matches('/').to_owned();
        let app_secret = var("CALC_APP_SECRET").context("CALC_APP_SECRET is not set: give the calc app's secret (sa_app_…)")?;
        let allow_http = var("CALC_ALLOW_HTTP_ACCOUNTS").is_some_and(|v| v == "1" || v == "true")
            || accounts_url.starts_with("http://localhost")
            || accounts_url.starts_with("http://127.0.0.1");
        if !allow_http && !accounts_url.starts_with("https://") {
            bail!("CALC_ACCOUNTS_URL must be https outside localhost (or set CALC_ALLOW_HTTP_ACCOUNTS=1)");
        }
        let secs = |name: &str, default: u64| -> anyhow::Result<u64> {
            match var(name) {
                Some(v) => v.parse().with_context(|| format!("{name}={v} is not a number of seconds")),
                None => Ok(default),
            }
        };
        Ok(Self {
            bind,
            database_url,
            accounts_url,
            accounts_api_url,
            app_id: var("CALC_APP_ID").unwrap_or_else(|| "calc".into()),
            app_secret,
            web_url: var("CALC_WEB_URL").unwrap_or_else(|| "http://127.0.0.1:8731".into()).trim_end_matches('/').to_owned(),
            blob_dir: PathBuf::from(var("CALC_BLOB_DIR").unwrap_or_else(|| ".dev/blobs".into())),
            ting_url: var("CALC_TING_URL").map(|u| u.trim_end_matches('/').to_owned()),
            ting_app_id: var("CALC_TING_APP_ID").unwrap_or_else(|| "ting".into()),
            runner_python: var("CALC_RUNNER_PYTHON").unwrap_or_else(|| "python3".into()),
            runner_node: var("CALC_RUNNER_NODE").unwrap_or_else(|| "node".into()),
            code_sandbox: var("CALC_CODE_SANDBOX").unwrap_or_else(|| "auto".into()),
            code_timeout: Duration::from_secs(secs("CALC_CODE_TIMEOUT_SECONDS", 60)?),
            max_upload_bytes: var("CALC_MAX_UPLOAD_BYTES").map(|v| v.parse().unwrap_or(500 << 20)).unwrap_or(500 << 20),
            snapshot_every: secs("CALC_SNAPSHOT_EVERY", 200)?,
            line_idle: Duration::from_secs(secs("CALC_LINE_IDLE_SECONDS", 600)?),
            allow_http,
        })
    }
}

impl Config {
    /// The web app's link to a sheet.
    pub fn sheet_url(&self, sheet_id: &str) -> String {
        format!("{}/s/{sheet_id}", self.web_url)
    }

    /// The web app's link to a folder.
    pub fn folder_url(&self, folder_id: &str) -> String {
        format!("{}/f/{folder_id}", self.web_url)
    }

    /// The web app's link to one place in a sheet: a copy, a tab, a range, a comment thread (each optional).
    pub fn sheet_place_url(
        &self,
        sheet_id: &str,
        copy_id: Option<&str>,
        tab: Option<u32>,
        range: Option<&str>,
        thread: Option<&str>,
    ) -> String {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        if let Some(c) = copy_id {
            q.append_pair("copy", c);
        }
        if let Some(t) = tab {
            q.append_pair("tab", &t.to_string());
        }
        if let Some(r) = range {
            q.append_pair("range", r);
        }
        if let Some(t) = thread {
            q.append_pair("thread", t);
        }
        let q = q.finish();
        if q.is_empty() { self.sheet_url(sheet_id) } else { format!("{}?{q}", self.sheet_url(sheet_id)) }
    }
}
