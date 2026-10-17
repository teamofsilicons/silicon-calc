//! The HTTP client: authentication, request plumbing, streamed uploads and downloads, and one method per route of
//! `docs/API.md`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt, TryStreamExt};
use reqwest::{Method, RequestBuilder, Response, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

use crate::error::{Error, Result};
use crate::events::{EventOptions, EventStream};
use crate::types::*;

/// The production API.
pub const DEFAULT_API_URL: &str = "https://calc-api.teamofsilicons.com";

/// This crate's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Supplies access tokens and refreshes them. The client asks for a token before each request; when the service
/// answers 401 it asks for a fresh one once (passing the refused token) and tries again.
pub trait TokenSource: Send + Sync {
    /// The access token to send now, or `None` to send the request without one (public sheets).
    fn token(&self) -> BoxFuture<'_, Result<Option<String>>>;

    /// The service refused `rejected`: return a fresh token, or `None` when there is none.
    fn refresh<'a>(&'a self, rejected: &'a str) -> BoxFuture<'a, Result<Option<String>>>;
}

/// A token that never changes (tests, short scripts). A refused token stays refused.
#[derive(Clone)]
pub struct StaticToken(pub String);

impl TokenSource for StaticToken {
    fn token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(Some(self.0.clone())) })
    }

    fn refresh<'a>(&'a self, _rejected: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { Ok(None) })
    }
}

#[derive(Clone)]
enum Auth {
    None,
    Source(Arc<dyn TokenSource>),
}

/// A line: the main sheet, or one of its copies. Every route under `{line}` takes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Line {
    /// The sheet's id.
    pub sheet: String,
    /// The copy's id; none is the main sheet.
    pub copy: Option<String>,
}

impl Line {
    /// The main line of `sheet`.
    pub fn main(sheet: impl Into<String>) -> Self {
        Self { sheet: sheet.into(), copy: None }
    }

    /// The copy `copy` of `sheet`.
    pub fn copy(sheet: impl Into<String>, copy: impl Into<String>) -> Self {
        Self { sheet: sheet.into(), copy: Some(copy.into()) }
    }

    /// True for the main sheet.
    pub fn is_main(&self) -> bool {
        self.copy.is_none()
    }

    fn segments(&self) -> Vec<&str> {
        let mut s = vec!["v1", "sheets", self.sheet.as_str()];
        if let Some(c) = &self.copy {
            s.push("copies");
            s.push(c.as_str());
        }
        s
    }
}

/// A file to upload (imports), sent as the raw body and streamed from disk.
#[derive(Debug, Clone)]
pub struct Upload {
    /// The file name sent with it; its extension tells the service the format.
    pub file_name: String,
    source: UploadSource,
}

#[derive(Debug, Clone)]
enum UploadSource {
    Bytes(Bytes),
    File { path: PathBuf, len: u64 },
}

impl Upload {
    /// An upload from memory.
    pub fn from_bytes(file_name: impl Into<String>, bytes: impl Into<Bytes>) -> Self {
        Self { file_name: file_name.into(), source: UploadSource::Bytes(bytes.into()) }
    }

    /// An upload streamed from a local file (read when sent, and again if the request has to be repeated).
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let meta = std::fs::metadata(path).map_err(|e| {
            Error::io(format!("Can't read {}: {e}.", path.display()), "Check the path and that the file is readable.", e)
        })?;
        if !meta.is_file() {
            return Err(Error::invalid_input(
                format!("{} is not a file.", path.display()),
                "Give the path of a .csv, .tsv, .xlsx, .xls, .ods, .numbers, .json or .parquet file.",
            ));
        }
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        Ok(Self { file_name, source: UploadSource::File { path: path.to_owned(), len: meta.len() } })
    }

    /// The same upload under another file name (the name's extension names the format).
    pub fn named(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = file_name.into();
        self
    }

    /// Its size in bytes.
    pub fn len(&self) -> u64 {
        match &self.source {
            UploadSource::Bytes(b) => b.len() as u64,
            UploadSource::File { len, .. } => *len,
        }
    }

    /// True when it is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn body(&self) -> Result<reqwest::Body> {
        match &self.source {
            UploadSource::Bytes(b) => Ok(reqwest::Body::from(b.clone())),
            UploadSource::File { path, .. } => {
                let file = std::fs::File::open(path).map_err(|e| {
                    Error::io(format!("Can't open {}: {e}.", path.display()), "Check the path and its permissions.", e)
                })?;
                let stream = tokio_util::io::ReaderStream::new(tokio::fs::File::from_std(file));
                Ok(reqwest::Body::wrap_stream(stream))
            }
        }
    }

    fn mime(&self) -> &'static str {
        Format::from_file_name(&self.file_name).map(Format::mime).unwrap_or("application/octet-stream")
    }
}

/// A file the service sends back (downloads, CSV reads), streamed.
#[derive(Debug)]
pub struct Download {
    /// The name from `Content-Disposition`.
    pub file_name: Option<String>,
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// `Content-Length`.
    pub content_length: Option<u64>,
    response: Response,
}

impl Download {
    fn new(response: Response) -> Self {
        let header = |name: reqwest::header::HeaderName| {
            response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
        };
        let file_name = header(reqwest::header::CONTENT_DISPOSITION).and_then(|v| content_disposition_filename(&v));
        let content_type = header(reqwest::header::CONTENT_TYPE);
        let content_length = response.content_length();
        Self { file_name, content_type, content_length, response }
    }

    /// The body as a stream of chunks.
    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes>> {
        self.response.bytes_stream().map_err(network_error)
    }

    /// The whole body in memory.
    pub async fn bytes(self) -> Result<Bytes> {
        self.response.bytes().await.map_err(network_error)
    }

    /// The whole body as text.
    pub async fn text(self) -> Result<String> {
        self.response.text().await.map_err(network_error)
    }

    /// Streams the body into `writer`; returns the bytes written.
    pub async fn write_to<W: tokio::io::AsyncWrite + Unpin>(self, writer: &mut W) -> Result<u64> {
        use tokio::io::AsyncWriteExt;
        let write_err = |e: std::io::Error| {
            Error::io(format!("Can't write the download: {e}."), "Check the destination and the free space.", e)
        };
        let mut stream = std::pin::pin!(self.bytes_stream());
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            writer.write_all(&chunk).await.map_err(write_err)?;
            total += chunk.len() as u64;
        }
        writer.flush().await.map_err(write_err)?;
        Ok(total)
    }

    /// Streams the body into a new file at `path` (written beside it first, then renamed, so a failed download
    /// never leaves half a file). Returns the bytes written.
    pub async fn save(self, path: impl AsRef<Path>) -> Result<u64> {
        let path = path.as_ref();
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let tmp = dir.join(format!(
            ".{}.part-{}",
            path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into()),
            std::process::id()
        ));
        let mut file = tokio::fs::File::create(&tmp).await.map_err(|e| {
            Error::io(format!("Can't create {}: {e}.", tmp.display()), "Check that the folder exists and is writable.", e)
        })?;
        let written = match self.write_to(&mut file).await {
            Ok(n) => n,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };
        drop(file);
        tokio::fs::rename(&tmp, path).await.map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            Error::io(format!("Can't save {}: {e}.", path.display()), "Check that the folder is writable.", e)
        })?;
        Ok(written)
    }
}

/// The file name in a `Content-Disposition` header (`filename*=UTF-8''…` preferred).
pub fn content_disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        let Some((key, val)) = part.split_once('=') else { continue };
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                let encoded = val.trim().trim_matches('"');
                let encoded = encoded.split_once("''").map(|(_, rest)| rest).unwrap_or(encoded);
                let decoded = percent_encoding::percent_decode_str(encoded).decode_utf8_lossy().into_owned();
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
            "filename" => {
                let v = val.trim();
                let v = v.strip_prefix('"').and_then(|v| v.strip_suffix('"')).unwrap_or(v);
                plain = Some(v.replace("\\\"", "\""));
            }
            _ => {}
        }
    }
    plain.filter(|p| !p.is_empty())
}

pub(crate) fn network_error(source: reqwest::Error) -> Error {
    let what = if source.is_timeout() {
        "timed out"
    } else if source.is_connect() {
        "could not connect"
    } else if source.is_body() || source.is_decode() {
        "was cut off"
    } else {
        "failed"
    };
    let url = source.url().map(|u| format!(" at {}", u.origin().ascii_serialization())).unwrap_or_default();
    Error::Network {
        message: format!("The request to Silicon Calc{url} {what}: {source}."),
        hint: "Check the connection and the service URL (--api-url or CALC_API_URL); a running service answers GET /healthz."
            .into(),
        source,
    }
}

/// Builds a [`Client`].
#[derive(Debug, Clone)]
pub struct ClientBuilder {
    base_url: String,
    timeout: Duration,
    connect_timeout: Duration,
    read_timeout: Duration,
    user_agent: Option<String>,
    retries: u32,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_API_URL.into(),
            timeout: Duration::from_secs(300),
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(600),
            user_agent: None,
            retries: 2,
        }
    }
}

impl ClientBuilder {
    /// The service URL (default [`DEFAULT_API_URL`]).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// The most one JSON request may take, answer included (default 300 s). Uploads, downloads and the live
    /// stream have no total limit; they stop only when the service goes quiet for the read timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Time to connect (default 10 s).
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// The longest the service may stay silent while answering (default 600 s).
    pub fn read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// A product token put before this crate's in the User-Agent, like `calc/0.1.0`.
    pub fn user_agent(mut self, product: impl Into<String>) -> Self {
        self.user_agent = Some(product.into());
        self
    }

    /// How many times a request the service answered `slow_down` (429) is tried again (default 2).
    pub fn retries(mut self, retries: u32) -> Self {
        self.retries = retries;
        self
    }

    /// Builds the client, without credentials (add them with [`Client::with_token`] or
    /// [`Client::with_token_source`]).
    pub fn build(self) -> Result<Client> {
        let trimmed = self.base_url.trim().trim_end_matches('/');
        let base = Url::parse(trimmed).map_err(|e| {
            Error::invalid_input(
                format!("The Silicon Calc URL `{trimmed}` is not a full URL ({e})."),
                format!("Use a URL such as {DEFAULT_API_URL} or http://127.0.0.1:8730."),
            )
        })?;
        if !matches!(base.scheme(), "http" | "https") || base.cannot_be_a_base() {
            return Err(Error::invalid_input(
                format!("The Silicon Calc URL `{trimmed}` must start with https:// (or http:// on this machine)."),
                format!("Use a URL such as {DEFAULT_API_URL}."),
            ));
        }
        let user_agent = match &self.user_agent {
            Some(product) => format!("{product} silicon-calc-client/{VERSION}"),
            None => format!("silicon-calc-client/{VERSION}"),
        };
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .connect_timeout(self.connect_timeout)
            .read_timeout(self.read_timeout)
            .build()
            .map_err(|e| Error::invalid_input(format!("Can't set up HTTP: {e}."), "This is a bug in silicon-calc-client."))?;
        Ok(Client { http, base, auth: Auth::None, timeout: self.timeout, retries: self.retries })
    }
}

/// The Silicon Calc client. Cheap to clone; clones share connections.
#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    auth: Auth,
    timeout: Duration,
    retries: u32,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auth = match self.auth {
            Auth::None => "none",
            Auth::Source(_) => "token source",
        };
        f.debug_struct("Client").field("base", &self.base.as_str()).field("auth", &auth).finish()
    }
}

fn body(value: impl Serialize) -> Json {
    serde_json::to_value(value).unwrap_or(Json::Null)
}

fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

async fn check(response: Response) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let after = retry_after(&response);
    let bytes = response.bytes().await.unwrap_or_default();
    Err(Error::from_response(status.as_u16(), &bytes, after))
}

async fn decode<T: DeserializeOwned>(response: Response) -> Result<T> {
    let url = response.url().path().to_owned();
    let bytes = response.bytes().await.map_err(network_error)?;
    serde_json::from_slice(&bytes).map_err(|e| {
        let snippet: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        Error::decode(
            format!("Silicon Calc's answer to {url} could not be read ({e}): {snippet}"),
            "The client and the service may be different versions; update the client (calc) or check the service URL.",
        )
    })
}

async fn decode_optional<T: DeserializeOwned>(response: Response) -> Result<Option<T>> {
    if response.status() == StatusCode::NO_CONTENT {
        return Ok(None);
    }
    let url = response.url().path().to_owned();
    let bytes = response.bytes().await.map_err(network_error)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(&bytes).map(Some).map_err(|e| {
        let snippet: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
        Error::decode(
            format!("Silicon Calc's answer to {url} could not be read ({e}): {snippet}"),
            "The client and the service may be different versions; update the client (calc) or check the service URL.",
        )
    })
}

/// What a request body is.
#[derive(Clone)]
enum Payload {
    None,
    Json(Json),
    Upload(Upload),
}

impl Payload {
    fn json(value: Option<Json>) -> Self {
        value.map(Self::Json).unwrap_or(Self::None)
    }
}

impl Client {
    /// A client for `base_url` without credentials (public sheets only).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        Self::builder().base_url(base_url).build()
    }

    /// A [`ClientBuilder`].
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// The same client sending this access token.
    pub fn with_token(self, access_token: impl Into<String>) -> Self {
        self.with_token_source(Arc::new(StaticToken(access_token.into())))
    }

    /// The same client asking `source` for tokens (and for a fresh one after a 401).
    pub fn with_token_source(mut self, source: Arc<dyn TokenSource>) -> Self {
        self.auth = Auth::Source(source);
        self
    }

    /// The same client sending no token.
    pub fn without_token(mut self) -> Self {
        self.auth = Auth::None;
        self
    }

    /// The service URL.
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// `{base}/{segments…}`, each segment percent-encoded.
    pub fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty();
            path.extend(segments);
        }
        url
    }

    fn line_url(&self, line: &Line, rest: &[&str]) -> Url {
        let mut segments = line.segments();
        segments.extend_from_slice(rest);
        self.url(&segments)
    }

    async fn current_token(&self) -> Result<Option<String>> {
        match &self.auth {
            Auth::None => Ok(None),
            Auth::Source(s) => s.token().await,
        }
    }

    /// Sends a request built by `build` (called again for each retry) and returns the successful answer. A 401
    /// asks the token source for a fresh token once; a 429 waits as told and tries again.
    async fn execute(&self, build: impl Fn(&reqwest::Client) -> Result<RequestBuilder>) -> Result<Response> {
        let mut token = self.current_token().await?;
        let mut refreshed = false;
        let mut slowed = 0;
        loop {
            let mut request = build(&self.http)?;
            if let Some(t) = &token {
                request = request.bearer_auth(t);
            }
            let response = request.send().await.map_err(network_error)?;
            let status = response.status();
            if status == StatusCode::UNAUTHORIZED
                && !refreshed
                && let (Auth::Source(source), Some(used)) = (&self.auth, token.as_deref())
            {
                refreshed = true;
                if let Some(fresh) = source.refresh(used).await? {
                    token = Some(fresh);
                    continue;
                }
            }
            if status == StatusCode::TOO_MANY_REQUESTS && slowed < self.retries {
                slowed += 1;
                let wait = retry_after(&response).unwrap_or(Duration::from_secs(1)).min(Duration::from_secs(30));
                tokio::time::sleep(wait).await;
                continue;
            }
            return check(response).await;
        }
    }

    fn request(
        &self,
        method: Method,
        url: Url,
        query: Vec<(&'static str, String)>,
        payload: Payload,
        timed: bool,
    ) -> impl Fn(&reqwest::Client) -> Result<RequestBuilder> {
        let timeout = self.timeout;
        move |http: &reqwest::Client| {
            let mut r = http.request(method.clone(), url.clone());
            if !query.is_empty() {
                r = r.query(&query);
            }
            if timed {
                r = r.timeout(timeout);
            }
            Ok(match &payload {
                Payload::None => r,
                Payload::Json(j) => r.json(j),
                Payload::Upload(upload) => r
                    .header(reqwest::header::CONTENT_TYPE, upload.mime())
                    .header(reqwest::header::CONTENT_LENGTH, upload.len())
                    .body(upload.body()?),
            })
        }
    }

    async fn get<T: DeserializeOwned>(&self, url: Url, query: Vec<(&'static str, String)>) -> Result<T> {
        let response = self.execute(self.request(Method::GET, url, query, Payload::None, true)).await?;
        decode(response).await
    }

    async fn send<T: DeserializeOwned>(&self, method: Method, url: Url, payload: Option<Json>) -> Result<T> {
        let response = self.execute(self.request(method, url, Vec::new(), Payload::json(payload), true)).await?;
        decode(response).await
    }

    async fn send_optional<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        query: Vec<(&'static str, String)>,
        payload: Option<Json>,
    ) -> Result<Option<T>> {
        let response = self.execute(self.request(method, url, query, Payload::json(payload), true)).await?;
        decode_optional(response).await
    }

    async fn send_empty(
        &self,
        method: Method,
        url: Url,
        query: Vec<(&'static str, String)>,
        payload: Option<Json>,
    ) -> Result<()> {
        self.execute(self.request(method, url, query, Payload::json(payload), true)).await.map(|_| ())
    }

    async fn upload<T: DeserializeOwned>(&self, url: Url, query: Vec<(&'static str, String)>, upload: &Upload) -> Result<T> {
        let response = self.execute(self.request(Method::POST, url, query, Payload::Upload(upload.clone()), false)).await?;
        decode(response).await
    }

    async fn download_from(&self, url: Url, query: Vec<(&'static str, String)>) -> Result<Download> {
        let response = self.execute(self.request(Method::GET, url, query, Payload::None, false)).await?;
        Ok(Download::new(response))
    }

    /// Opens the live stream of a line once (no reconnecting); [`Client::events`] wraps it.
    pub(crate) async fn open_events(&self, line: &Line, since: Option<i64>) -> Result<Response> {
        let url = self.line_url(line, &["events"]);
        let mut query = Vec::new();
        if let Some(v) = since {
            query.push(("since", v.to_string()));
        }
        let build = move |http: &reqwest::Client| {
            let mut r = http.get(url.clone()).header(reqwest::header::ACCEPT, "text/event-stream");
            if !query.is_empty() {
                r = r.query(&query);
            }
            Ok(r)
        };
        self.execute(build).await
    }

    // ── Meta ─────────────────────────────────────────────────────────────────────────────────────────────

    /// `GET /healthz`: `Ok` when the service is up.
    pub async fn healthz(&self) -> Result<()> {
        let url = self.url(&["healthz"]);
        self.execute(self.request(Method::GET, url, Vec::new(), Payload::None, true)).await.map(|_| ())
    }

    /// `GET /v1/meta`: the app id, version, Silicon Accounts URL and limits.
    pub async fn meta(&self) -> Result<Meta> {
        self.get(self.url(&["v1", "meta"]), Vec::new()).await
    }

    /// `GET /v1/me`: the signed-in account.
    pub async fn me(&self) -> Result<Account> {
        let me: Me = self.get(self.url(&["v1", "me"]), Vec::new()).await?;
        Ok(me.account)
    }

    /// `GET /v1/accounts/lookup?id=`: an account by its `c:`/`si:` id (404 when there is none).
    pub async fn lookup_account(&self, id: &str) -> Result<Account> {
        self.get(self.url(&["v1", "accounts", "lookup"]), vec![("id", id.trim().to_owned())]).await
    }

    // ── Sheets ───────────────────────────────────────────────────────────────────────────────────────────

    /// `GET /v1/sheets`: one page of sheets and folders.
    pub async fn list_sheets(&self, params: &ListSheets) -> Result<SheetList> {
        let mut q = vec![("scope", params.scope.as_str().to_owned())];
        if let Some(f) = &params.folder {
            q.push(("folder", f.clone()));
        }
        if let Some(text) = params.q.as_ref().filter(|t| !t.is_empty()) {
            q.push(("q", text.clone()));
        }
        if let Some(c) = &params.cursor {
            q.push(("cursor", c.clone()));
        }
        if let Some(l) = params.limit {
            q.push(("limit", l.to_string()));
        }
        self.get(self.url(&["v1", "sheets"]), q).await
    }

    /// Every page of `GET /v1/sheets`, up to `max` sheets.
    pub async fn list_all_sheets(&self, params: &ListSheets, max: usize) -> Result<SheetList> {
        let mut params = params.clone();
        let mut all = SheetList::default();
        loop {
            let page = self.list_sheets(&params).await?;
            all.sheets.extend(page.sheets);
            for f in page.folders {
                if !all.folders.iter().any(|g| g.id == f.id) {
                    all.folders.push(f);
                }
            }
            match page.next_cursor {
                Some(c) if all.sheets.len() < max && params.cursor.as_deref() != Some(c.as_str()) => params.cursor = Some(c),
                other => {
                    all.next_cursor = other;
                    break;
                }
            }
        }
        Ok(all)
    }

    /// `POST /v1/sheets`: a new sheet with one empty tab.
    pub async fn create_sheet(&self, new: &NewSheet) -> Result<Sheet> {
        self.send(Method::POST, self.url(&["v1", "sheets"]), Some(body(new))).await
    }

    /// `POST /v1/sheets/import`: a new sheet from a file (`.csv .tsv .xlsx .xls .ods .numbers .json .parquet`).
    pub async fn import_sheet(&self, upload: &Upload, params: &ImportSheet) -> Result<Sheet> {
        let mut q = vec![("filename", upload.file_name.clone())];
        if let Some(t) = &params.title {
            q.push(("title", t.clone()));
        }
        if let Some(f) = &params.folder_id {
            q.push(("folder_id", f.clone()));
        }
        self.upload(self.url(&["v1", "sheets", "import"]), q, upload).await
    }

    /// `GET /v1/sheets/{sheet}`: the sheet with its tabs.
    pub async fn sheet(&self, sheet: &str) -> Result<Sheet> {
        self.get(self.url(&["v1", "sheets", sheet]), Vec::new()).await
    }

    /// `PATCH /v1/sheets/{sheet}`: rename or move.
    pub async fn update_sheet(&self, sheet: &str, patch: &SheetPatch) -> Result<Sheet> {
        self.send(Method::PATCH, self.url(&["v1", "sheets", sheet]), Some(body(patch))).await
    }

    /// `DELETE /v1/sheets/{sheet}`: moves it to the trash (owner).
    pub async fn trash_sheet(&self, sheet: &str) -> Result<()> {
        self.send_empty(Method::DELETE, self.url(&["v1", "sheets", sheet]), Vec::new(), None).await
    }

    /// `POST /v1/sheets/{sheet}/restore`: out of the trash.
    pub async fn restore_sheet(&self, sheet: &str) -> Result<Sheet> {
        self.send(Method::POST, self.url(&["v1", "sheets", sheet, "restore"]), None).await
    }

    /// `DELETE /v1/sheets/{sheet}/forever?confirm=`: only from the trash; `title` must be the sheet's title.
    pub async fn delete_sheet_forever(&self, sheet: &str, title: &str) -> Result<()> {
        let url = self.url(&["v1", "sheets", sheet, "forever"]);
        self.send_empty(Method::DELETE, url, vec![("confirm", title.to_owned())], None).await
    }

    // ── Transfers ────────────────────────────────────────────────────────────────────────────────────────

    /// `POST {item}/transfer`: offers a sheet or folder to a new owner (`c:ada`).
    pub async fn transfer(&self, item: &Item, to: &str) -> Result<Transfer> {
        let [kind, id] = item.segments();
        self.send(Method::POST, self.url(&["v1", kind, id, "transfer"]), Some(json!({"to": to.trim()}))).await
    }

    /// `GET /v1/transfers`.
    pub async fn transfers(&self) -> Result<Transfers> {
        self.get(self.url(&["v1", "transfers"]), Vec::new()).await
    }

    /// `POST /v1/transfers/{id}/accept|decline|cancel`.
    pub async fn transfer_action(&self, transfer: &str, action: TransferAction) -> Result<Transfer> {
        self.send(Method::POST, self.url(&["v1", "transfers", transfer, action.as_str()]), None).await
    }

    // ── Folders ──────────────────────────────────────────────────────────────────────────────────────────

    /// `POST /v1/folders`.
    pub async fn create_folder(&self, new: &NewFolder) -> Result<Folder> {
        self.send(Method::POST, self.url(&["v1", "folders"]), Some(body(new))).await
    }

    /// `GET /v1/folders/{id}`: the folder, its path and its contents.
    pub async fn folder(&self, folder: &str) -> Result<FolderView> {
        self.get(self.url(&["v1", "folders", folder]), Vec::new()).await
    }

    /// `PATCH /v1/folders/{id}`: rename or move.
    pub async fn update_folder(&self, folder: &str, patch: &FolderPatch) -> Result<Option<Folder>> {
        self.send_optional(Method::PATCH, self.url(&["v1", "folders", folder]), Vec::new(), Some(body(patch))).await
    }

    /// `DELETE /v1/folders/{id}`: to the trash, with everything in it.
    pub async fn trash_folder(&self, folder: &str) -> Result<()> {
        self.send_empty(Method::DELETE, self.url(&["v1", "folders", folder]), Vec::new(), None).await
    }

    /// `POST /v1/folders/{id}/restore`.
    pub async fn restore_folder(&self, folder: &str) -> Result<Option<Folder>> {
        self.send_optional(Method::POST, self.url(&["v1", "folders", folder, "restore"]), Vec::new(), None).await
    }

    /// `DELETE /v1/folders/{id}/forever?confirm=`: `name` must be the folder's name.
    pub async fn delete_folder_forever(&self, folder: &str, name: &str) -> Result<()> {
        let url = self.url(&["v1", "folders", folder, "forever"]);
        self.send_empty(Method::DELETE, url, vec![("confirm", name.to_owned())], None).await
    }

    // ── Sharing ──────────────────────────────────────────────────────────────────────────────────────────

    /// `GET {item}/sharing`.
    pub async fn sharing(&self, item: &Item) -> Result<Sharing> {
        let [kind, id] = item.segments();
        self.get(self.url(&["v1", kind, id, "sharing"]), Vec::new()).await
    }

    /// `POST {item}/sharing`: invites ids and emails with a role.
    pub async fn share(&self, item: &Item, request: &ShareRequest) -> Result<Sharing> {
        let [kind, id] = item.segments();
        self.send(Method::POST, self.url(&["v1", kind, id, "sharing"]), Some(body(request))).await
    }

    /// `PATCH {item}/sharing/{account uuid}`: changes a member's role.
    pub async fn set_member_role(&self, item: &Item, account_uuid: &str, role: Role) -> Result<Option<Sharing>> {
        let [kind, id] = item.segments();
        let url = self.url(&["v1", kind, id, "sharing", account_uuid]);
        self.send_optional(Method::PATCH, url, Vec::new(), Some(json!({"role": role}))).await
    }

    /// `DELETE {item}/sharing/{account uuid}`: removes a member (or the caller leaves).
    pub async fn remove_member(&self, item: &Item, account_uuid: &str) -> Result<()> {
        let [kind, id] = item.segments();
        self.send_empty(Method::DELETE, self.url(&["v1", kind, id, "sharing", account_uuid]), Vec::new(), None).await
    }

    /// `DELETE {item}/invites/{email}`.
    pub async fn remove_invite(&self, item: &Item, email: &str) -> Result<()> {
        let [kind, id] = item.segments();
        self.send_empty(Method::DELETE, self.url(&["v1", kind, id, "invites", email.trim()]), Vec::new(), None).await
    }

    /// `PUT /v1/sheets/{sheet}/public`: the public link.
    pub async fn set_public(&self, sheet: &str, setting: &PublicSetting) -> Result<PublicLink> {
        self.send(Method::PUT, self.url(&["v1", "sheets", sheet, "public"]), Some(body(setting))).await
    }

    // ── Cells ────────────────────────────────────────────────────────────────────────────────────────────

    /// `GET {line}/tabs`.
    pub async fn tabs(&self, line: &Line) -> Result<TabList> {
        self.get(self.line_url(line, &["tabs"]), Vec::new()).await
    }

    fn cells_query(range: Option<&str>, version: Option<i64>, format: Option<&str>) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(r) = range.filter(|r| !r.trim().is_empty()) {
            q.push(("range", r.trim().to_owned()));
        }
        if let Some(v) = version {
            q.push(("version", v.to_string()));
        }
        if let Some(f) = format {
            q.push(("format", f.to_owned()));
        }
        q
    }

    /// `GET {line}/tabs/{tab}/cells`: the non-empty cells of a range (default the used area), at a version
    /// (default the latest).
    pub async fn cells(&self, line: &Line, tab: &TabRef, range: Option<&str>, version: Option<i64>) -> Result<Cells> {
        let tab = tab.path();
        self.get(self.line_url(line, &["tabs", &tab, "cells"]), Self::cells_query(range, version, None)).await
    }

    /// `GET {line}/tabs/{tab}/cells?format=csv`: the range as CSV of shown values, streamed.
    pub async fn cells_csv(&self, line: &Line, tab: &TabRef, range: Option<&str>, version: Option<i64>) -> Result<Download> {
        let tab = tab.path();
        self.download_from(self.line_url(line, &["tabs", &tab, "cells"]), Self::cells_query(range, version, Some("csv"))).await
    }

    /// `GET {line}/tabs/{tab}/cells?format=values`: the range as rows of plain values.
    pub async fn cell_values(&self, line: &Line, tab: &TabRef, range: Option<&str>, version: Option<i64>) -> Result<CellValues> {
        let tab = tab.path();
        self.get(self.line_url(line, &["tabs", &tab, "cells"]), Self::cells_query(range, version, Some("values"))).await
    }

    /// `GET {line}/tabs/{tab}/cells/{a1}`: one cell with its look, its author and its last 20 changes.
    pub async fn cell(&self, line: &Line, tab: &TabRef, a1: &str) -> Result<CellDetail> {
        let tab = tab.path();
        self.get(self.line_url(line, &["tabs", &tab, "cells", a1.trim()]), Vec::new()).await
    }

    /// `POST {line}/edits`: the edits, in order, as one version (all or none).
    pub async fn edit(&self, line: &Line, request: &EditRequest) -> Result<EditAnswer> {
        if request.edits.is_empty() {
            return Err(Error::invalid_input("An edit request needs at least one edit.", "Add an edit, such as a set."));
        }
        self.send(Method::POST, self.line_url(line, &["edits"]), Some(body(request))).await
    }

    /// The live events of a line (`GET {line}/events`), reconnecting from the last version seen.
    pub fn events(&self, line: &Line, options: EventOptions) -> EventStream {
        EventStream::new(self.clone(), line.clone(), options)
    }

    /// `POST {line}/presence`: shows the caller viewing a tab (expires after 45 seconds).
    pub async fn presence(&self, line: &Line, update: &PresenceUpdate) -> Result<()> {
        self.send_empty(Method::POST, self.line_url(line, &["presence"]), Vec::new(), Some(body(update))).await
    }

    // ── Code ─────────────────────────────────────────────────────────────────────────────────────────────

    /// `POST {line}/run`: runs SQL, Python or JavaScript on the line.
    pub async fn run(&self, line: &Line, request: &RunRequest) -> Result<RunAnswer> {
        self.send(Method::POST, self.line_url(line, &["run"]), Some(body(request))).await
    }

    /// `POST {line}/tabs/{tab}/rerun`: reruns a code-made tab as the caller.
    pub async fn rerun(&self, line: &Line, tab: &TabRef) -> Result<RunAnswer> {
        let tab = tab.path();
        self.send(Method::POST, self.line_url(line, &["tabs", &tab, "rerun"]), None).await
    }

    /// `PATCH {line}/tabs/{tab}/code`: turns keep-updated on or off.
    pub async fn set_keep_updated(&self, line: &Line, tab: &TabRef, keep_updated: bool) -> Result<Tab> {
        let tab = tab.path();
        self.send(Method::PATCH, self.line_url(line, &["tabs", &tab, "code"]), Some(body(CodePatch { keep_updated }))).await
    }

    // ── Files ────────────────────────────────────────────────────────────────────────────────────────────

    /// `POST {line}/tabs/import`: a file's tabs as new tabs, or its first tab replacing one tab's contents.
    pub async fn import_tabs(&self, line: &Line, upload: &Upload, mode: &ImportMode) -> Result<ImportedTabs> {
        let mut q = vec![("filename", upload.file_name.clone())];
        match mode {
            ImportMode::NewTab => q.push(("mode", "new_tab".into())),
            ImportMode::Replace(tab) => {
                q.push(("mode", "replace".into()));
                q.push(("tab", tab.path()));
            }
        }
        self.upload(self.line_url(line, &["tabs", "import"]), q, upload).await
    }

    /// `GET {line}/download`: the sheet (or one tab) as a file, streamed.
    pub async fn download(&self, line: &Line, query: &DownloadQuery) -> Result<Download> {
        let mut q = vec![("format", query.format.ext().to_owned())];
        if let Some(t) = &query.tab {
            q.push(("tab", t.path()));
        }
        if let Some(v) = query.version {
            q.push(("version", v.to_string()));
        }
        if query.formulas {
            q.push(("formulas", "true".into()));
        }
        self.download_from(self.line_url(line, &["download"]), q).await
    }

    // ── Versions ─────────────────────────────────────────────────────────────────────────────────────────

    /// `GET {line}/versions`: newest first.
    pub async fn versions(&self, line: &Line, cursor: Option<&str>, limit: Option<u32>) -> Result<VersionList> {
        let mut q = Vec::new();
        if let Some(c) = cursor {
            q.push(("cursor", c.to_owned()));
        }
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        self.get(self.line_url(line, &["versions"]), q).await
    }

    /// `POST {line}/versions/{v}/name`.
    pub async fn name_version(&self, line: &Line, version: i64, name: &str) -> Result<Option<Version>> {
        let v = version.to_string();
        self.send_optional(Method::POST, self.line_url(line, &["versions", &v, "name"]), Vec::new(), Some(json!({"name": name})))
            .await
    }

    /// `DELETE {line}/versions/{v}/name`.
    pub async fn unname_version(&self, line: &Line, version: i64) -> Result<()> {
        let v = version.to_string();
        self.send_empty(Method::DELETE, self.line_url(line, &["versions", &v, "name"]), Vec::new(), None).await
    }

    /// `GET {line}/versions/{v}/tabs`: the tabs as they were.
    pub async fn tabs_at(&self, line: &Line, version: i64) -> Result<TabList> {
        let v = version.to_string();
        self.get(self.line_url(line, &["versions", &v, "tabs"]), Vec::new()).await
    }

    /// `GET {line}/versions/{v}/tabs/{tab}/cells`: cells as they were.
    pub async fn cells_at(&self, line: &Line, version: i64, tab: &TabRef, range: Option<&str>) -> Result<Cells> {
        let (v, tab) = (version.to_string(), tab.path());
        self.get(self.line_url(line, &["versions", &v, "tabs", &tab, "cells"]), Self::cells_query(range, None, None)).await
    }

    /// `GET {line}/compare`: two versions compared cell by cell.
    pub async fn compare(&self, line: &Line, query: &CompareQuery) -> Result<WorkbookDiff> {
        let mut q = vec![("from", query.from.to_string())];
        q.push(("to", query.to.map(|v| v.to_string()).unwrap_or_else(|| "head".into())));
        if let Some(l) = query.limit {
            q.push(("limit", l.to_string()));
        }
        if let Some(t) = &query.tab {
            q.push(("tab", t.path()));
        }
        if let Some(r) = &query.range {
            q.push(("range", r.clone()));
        }
        self.get(self.line_url(line, &["compare"]), q).await
    }

    /// `POST {line}/versions/{v}/restore`: the sheet (or one tab or range) as it was, as a new version.
    pub async fn restore_version(&self, line: &Line, version: i64, request: &RestoreRequest) -> Result<VersionAnswer> {
        let v = version.to_string();
        self.send(Method::POST, self.line_url(line, &["versions", &v, "restore"]), Some(body(request))).await
    }

    /// `POST {line}/versions/{v}/undo-account`: undoes everything `account` (`si:analyst`) changed after `v`.
    pub async fn undo_account(&self, line: &Line, version: i64, account: &str) -> Result<UndoAccountAnswer> {
        let v = version.to_string();
        self.send(Method::POST, self.line_url(line, &["versions", &v, "undo-account"]), Some(json!({"account": account.trim()})))
            .await
    }

    /// `POST {line}/versions/{v}/new-sheet`: a new sheet holding the line as it was at `v`.
    pub async fn version_to_new_sheet(&self, line: &Line, version: i64, title: Option<&str>) -> Result<Sheet> {
        let v = version.to_string();
        let payload = match title {
            Some(t) => json!({"title": t}),
            None => json!({}),
        };
        self.send(Method::POST, self.line_url(line, &["versions", &v, "new-sheet"]), Some(payload)).await
    }

    // ── Copies ───────────────────────────────────────────────────────────────────────────────────────────

    /// `GET /v1/sheets/{sheet}/copies`: open copies, or all of them.
    pub async fn copies(&self, sheet: &str, all: bool) -> Result<Vec<SheetCopy>> {
        let q = vec![("status", if all { "all" } else { "open" }.to_owned())];
        let list: CopyList = self.get(self.url(&["v1", "sheets", sheet, "copies"]), q).await?;
        Ok(list.copies)
    }

    /// `POST {line}/copies`: a copy of the line as it is now (a copy of a copy nests).
    pub async fn create_copy(&self, line: &Line, name: &str) -> Result<SheetCopy> {
        self.send(Method::POST, self.line_url(line, &["copies"]), Some(json!({"name": name}))).await
    }

    /// `PATCH /v1/sheets/{sheet}/copies/{copy}`: renames it.
    pub async fn rename_copy(&self, sheet: &str, copy: &str, name: &str) -> Result<Option<SheetCopy>> {
        let url = self.url(&["v1", "sheets", sheet, "copies", copy]);
        self.send_optional(Method::PATCH, url, Vec::new(), Some(json!({"name": name}))).await
    }

    /// `DELETE /v1/sheets/{sheet}/copies/{copy}`: its history stays readable.
    pub async fn delete_copy(&self, sheet: &str, copy: &str) -> Result<()> {
        self.send_empty(Method::DELETE, self.url(&["v1", "sheets", sheet, "copies", copy]), Vec::new(), None).await
    }

    /// `GET /v1/sheets/{sheet}/copies/{copy}/compare`: the copy against its parent at the fork, with conflicts.
    pub async fn compare_copy(&self, sheet: &str, copy: &str, limit: Option<u32>) -> Result<CopyCompare> {
        let q = limit.map(|l| vec![("limit", l.to_string())]).unwrap_or_default();
        self.get(self.url(&["v1", "sheets", sheet, "copies", copy, "compare"]), q).await
    }

    /// `POST /v1/sheets/{sheet}/copies/{copy}/merge`: merges the copy into its parent, in whole or in part.
    pub async fn merge_copy(&self, sheet: &str, copy: &str, request: &MergeRequest) -> Result<MergeAnswer> {
        self.send(Method::POST, self.url(&["v1", "sheets", sheet, "copies", copy, "merge"]), Some(body(request))).await
    }

    // ── Comments ─────────────────────────────────────────────────────────────────────────────────────────

    /// `GET {line}/comments`.
    pub async fn threads(&self, line: &Line, filter: ThreadFilter, tab: Option<&TabRef>) -> Result<Vec<Thread>> {
        let mut q = vec![("status", filter.as_str().to_owned())];
        if let Some(t) = tab {
            q.push(("tab", t.path()));
        }
        let list: ThreadList = self.get(self.line_url(line, &["comments"]), q).await?;
        Ok(list.threads)
    }

    /// `GET /v1/comments/{thread}`: one thread, for services that offer it (others answer 404 or 405).
    pub async fn thread(&self, thread: &str) -> Result<Thread> {
        self.get(self.url(&["v1", "comments", thread]), Vec::new()).await
    }

    /// `POST {line}/comments`: starts a thread on a range (none: the whole tab).
    pub async fn create_thread(&self, line: &Line, new: &NewThread) -> Result<Thread> {
        self.send(Method::POST, self.line_url(line, &["comments"]), Some(body(new))).await
    }

    /// `POST /v1/comments/{thread}/replies`.
    pub async fn reply(&self, thread: &str, text: &str) -> Result<Comment> {
        self.send(Method::POST, self.url(&["v1", "comments", thread, "replies"]), Some(json!({"body": text}))).await
    }

    /// `PATCH /v1/comments/{comment}` (author only).
    pub async fn edit_comment(&self, comment: &str, text: &str) -> Result<Option<Comment>> {
        self.send_optional(Method::PATCH, self.url(&["v1", "comments", comment]), Vec::new(), Some(json!({"body": text}))).await
    }

    /// `DELETE /v1/comments/{comment}` (author or owner).
    pub async fn delete_comment(&self, comment: &str) -> Result<()> {
        self.send_empty(Method::DELETE, self.url(&["v1", "comments", comment]), Vec::new(), None).await
    }

    /// `POST /v1/comments/{thread}/resolve`.
    pub async fn resolve_thread(&self, thread: &str) -> Result<Thread> {
        self.send(Method::POST, self.url(&["v1", "comments", thread, "resolve"]), None).await
    }

    /// `POST /v1/comments/{thread}/reopen`.
    pub async fn reopen_thread(&self, thread: &str) -> Result<Thread> {
        self.send(Method::POST, self.url(&["v1", "comments", thread, "reopen"]), None).await
    }

    /// `POST /v1/comments/{comment}/reactions`.
    pub async fn react(&self, comment: &str, emoji: &str) -> Result<Option<Json>> {
        let url = self.url(&["v1", "comments", comment, "reactions"]);
        self.send_optional(Method::POST, url, Vec::new(), Some(json!({"emoji": emoji}))).await
    }

    /// `DELETE /v1/comments/{comment}/reactions/{emoji}`.
    pub async fn unreact(&self, comment: &str, emoji: &str) -> Result<()> {
        self.send_empty(Method::DELETE, self.url(&["v1", "comments", comment, "reactions", emoji]), Vec::new(), None).await
    }

    // ── Search ───────────────────────────────────────────────────────────────────────────────────────────

    /// `GET /v1/search`: titles and cell text of every sheet the caller can read.
    pub async fn search(&self, text: &str, limit: Option<u32>) -> Result<Vec<SearchResult>> {
        let mut q = vec![("q", text.to_owned())];
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        let r: SearchResults = self.get(self.url(&["v1", "search"]), q).await?;
        Ok(r.results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition() {
        assert_eq!(content_disposition_filename("attachment; filename=\"Q3 revenue.xlsx\"").as_deref(), Some("Q3 revenue.xlsx"));
        assert_eq!(
            content_disposition_filename("attachment; filename=\"x.csv\"; filename*=UTF-8''Caf%C3%A9.csv").as_deref(),
            Some("Café.csv")
        );
        assert_eq!(content_disposition_filename("attachment").as_deref(), None);
    }

    #[test]
    fn urls_are_encoded_per_segment() {
        let c = Client::new("http://127.0.0.1:8730/").unwrap();
        let line = Line::copy("k3j9xq2mpa7d", "c2abc");
        let url = c.line_url(&line, &["tabs", "My tab/x", "cells"]);
        assert_eq!(url.as_str(), "http://127.0.0.1:8730/v1/sheets/k3j9xq2mpa7d/copies/c2abc/tabs/My%20tab%2Fx/cells");
        let c = Client::new("https://example.test/api").unwrap();
        assert_eq!(c.url(&["v1", "me"]).as_str(), "https://example.test/api/v1/me");
    }

    #[test]
    fn refuses_bad_urls() {
        assert!(Client::new("not a url").is_err());
        assert!(Client::new("ftp://example.test").is_err());
    }
}
