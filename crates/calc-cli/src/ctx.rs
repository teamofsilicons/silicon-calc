//! What every command needs: the URLs, the session, the client, and how to print.
//!
//! A sign-in belongs to the Silicon Calc service and the Silicon Accounts it was made with. A command pointed at
//! other URLs (a flag or an environment variable) doesn't use it, so tokens never travel to another service.

use std::io::{IsTerminal, Write as _};
use std::sync::Arc;

use serde::Serialize;
use silicon_calc_client::Client;

use crate::error::{CliError, CliResult, SIGN_IN_HINT};
use crate::session::{Session, SessionProvider, Store};

pub const DEFAULT_API_URL: &str = "https://calc-api.teamofsilicons.com";
pub const DEFAULT_ACCOUNTS_URL: &str = "https://accounts.teamofsilicons.com";
pub const WEB_URL: &str = "https://calc.teamofsilicons.com";
pub const REPOSITORY_URL: &str = "https://github.com/teamofsilicons/silicon-calc";
pub const DOCS_URL: &str = "https://github.com/teamofsilicons/silicon-calc/blob/main/docs/CLI.md";

pub struct Options {
    pub json: bool,
    pub quiet: bool,
    pub api_url: Option<String>,
    pub accounts_url: Option<String>,
}

pub struct Ctx {
    pub json: bool,
    pub quiet: bool,
    pub api_url: String,
    pub accounts_url: String,
    pub store: Store,
    pub provider: Arc<SessionProvider>,
    pub client: Client,
    pub signed_in: bool,
    /// Where each URL came from (`flag or environment`, `sign-in`, `default`).
    pub api_url_source: &'static str,
    pub accounts_url_source: &'static str,
    /// A sign-in stored here for other URLs than this command uses (so it isn't used).
    pub elsewhere: Option<Session>,
    /// Why the stored sign-in couldn't be read, when it couldn't.
    pub session_problem: Option<CliError>,
}

pub fn clean(url: Option<String>) -> Option<String> {
    url.map(|u| u.trim().trim_end_matches('/').to_owned()).filter(|u| !u.is_empty())
}

/// True when a stored sign-in was made with these URLs.
pub fn session_matches(s: &Session, api_url: &str, accounts_url: &str) -> bool {
    let same = |a: &str, b: &str| a.trim_end_matches('/').eq_ignore_ascii_case(b.trim_end_matches('/'));
    (s.api_url.is_empty() || same(&s.api_url, api_url)) && same(&s.accounts_url, accounts_url)
}

impl Ctx {
    pub fn new(options: Options) -> CliResult<Self> {
        let (dir, _) = crate::session::home()?;
        let store = Store::new(dir);
        let (session, session_problem) = match store.load() {
            Ok(s) => (s, None),
            Err(e) => (None, Some(e)),
        };
        // A flag or environment variable wins, then the URLs the session was made with, then production.
        let from_session = |f: fn(&Session) -> &str| clean(session.as_ref().map(|s| f(s).to_owned()));
        let (api_url, api_url_source) = match (clean(options.api_url), from_session(|s| &s.api_url)) {
            (Some(u), _) => (u, "flag or environment"),
            (None, Some(u)) => (u, "sign-in"),
            (None, None) => (DEFAULT_API_URL.to_owned(), "default"),
        };
        let (accounts_url, accounts_url_source) = match (clean(options.accounts_url), from_session(|s| &s.accounts_url)) {
            (Some(u), _) => (u, "flag or environment"),
            (None, Some(u)) => (u, "sign-in"),
            (None, None) => (DEFAULT_ACCOUNTS_URL.to_owned(), "default"),
        };
        let (usable, elsewhere) = match session {
            Some(s) if session_matches(&s, &api_url, &accounts_url) => (Some(s), None),
            Some(s) => (None, Some(s)),
            None => (None, None),
        };
        let signed_in = usable.is_some();
        let provider = Arc::new(SessionProvider::new(store.clone(), usable));
        let client = Client::builder()
            .base_url(&api_url)
            .user_agent(format!("calc/{}", env!("CARGO_PKG_VERSION")))
            .build()?
            .with_token_source(provider.clone());
        Ok(Self {
            json: options.json,
            quiet: options.quiet,
            api_url,
            accounts_url,
            store,
            provider,
            client,
            signed_in,
            api_url_source,
            accounts_url_source,
            elsewhere,
            session_problem,
        })
    }

    /// Prints the result: pretty JSON on stdout with `--json`, otherwise the readable text.
    pub fn emit<T: Serialize + ?Sized>(&self, value: &T, readable: impl FnOnce() -> String) {
        let mut out = std::io::stdout().lock();
        if self.json {
            let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into());
            let _ = writeln!(out, "{text}");
        } else {
            let text = readable();
            if !text.is_empty() {
                let _ = if text.ends_with('\n') { write!(out, "{text}") } else { writeln!(out, "{text}") };
            }
        }
        let _ = out.flush();
    }

    /// Prints one JSON line on stdout (streams: `watch`).
    pub fn line<T: Serialize + ?Sized>(&self, value: &T) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{}", serde_json::to_string(value).unwrap_or_else(|_| "null".into()));
        let _ = out.flush();
    }

    /// A notice on stderr (not with `--json` or `-q`).
    pub fn note(&self, text: impl AsRef<str>) {
        if !self.json && !self.quiet {
            eprintln!("{}", text.as_ref());
        }
    }

    /// A warning on stderr (shown even with `-q`; with `--json`, as a JSON line on stderr).
    pub fn warn(&self, text: impl AsRef<str>) {
        if self.json {
            eprintln!("{}", serde_json::json!({"warning": text.as_ref()}));
        } else {
            eprintln!("note: {}", text.as_ref());
        }
    }

    /// Why this command has no sign-in to use.
    pub fn signed_out_error(&self) -> CliError {
        if let Some(problem) = &self.session_problem {
            return problem.clone();
        }
        if let Some(s) = &self.elsewhere {
            let mut e = CliError::new(
                "signed_out",
                format!(
                    "You are signed in to Silicon Calc as {} at {} (Silicon Accounts {}), but this command uses {} (Silicon Accounts {}); a sign-in is never sent to another service.",
                    s.account.id, s.api_url, s.accounts_url, self.api_url, self.accounts_url
                ),
                "Sign in here too with calc login --force (that replaces the other sign-in in this CALC_HOME), use another CALC_HOME, or drop --api-url / --accounts-url.",
            );
            e.details = Some(serde_json::json!({"signed_in_at": {"api_url": s.api_url, "accounts_url": s.accounts_url, "id": s.account.id}}));
            return e;
        }
        CliError::new("signed_out", "You are not signed in to Silicon Calc.", SIGN_IN_HINT)
    }

    pub fn require_session(&self) -> CliResult<()> {
        if self.signed_in { Ok(()) } else { Err(self.signed_out_error()) }
    }

    /// True when questions may be asked: stdin and stderr are terminals and `--json` is off.
    pub fn interactive(&self) -> bool {
        !self.json && std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
    }
}

/// Asks for a line of text on stderr.
pub fn ask(question: &str) -> CliResult<String> {
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|e| CliError::file(format!("Can't read the answer: {e}.")))?;
    Ok(answer.trim_end_matches(['\n', '\r']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionAccount;

    #[test]
    fn sign_ins_stay_with_their_service() {
        let s = Session {
            app_id: "calc".into(),
            access_token: "t".into(),
            refresh_token: None,
            expires_at: 0,
            refresh_expires_at: None,
            account: SessionAccount::default(),
            accounts_url: "https://accounts.teamofsilicons.com".into(),
            api_url: "https://calc-api.teamofsilicons.com".into(),
            method: "slt".into(),
            signed_in_at: 0,
        };
        assert!(session_matches(&s, "https://calc-api.teamofsilicons.com/", "https://accounts.teamofsilicons.com"));
        assert!(!session_matches(&s, "http://127.0.0.1:8730", "https://accounts.teamofsilicons.com"));
        assert!(!session_matches(&s, "https://calc-api.teamofsilicons.com", "http://127.0.0.1:9989"));
        assert_eq!(clean(Some(" http://127.0.0.1:8730/ ".into())).as_deref(), Some("http://127.0.0.1:8730"));
        assert_eq!(clean(Some("  ".into())), None);
    }
}
