//! Signing in: `login`, `login status`, `logout`, `whoami` and `accounts`.
//!
//! Carbons sign in with Silicon Accounts' device flow for the app `calc` (approve a code in the browser).
//! Silicons sign in with a short-lived token exchanged as Calc's public client: either one they got themselves
//! (`calc login --slt`), or one this command gets for them from their si:id and STK (`calc login --silicon`).

use std::io::{IsTerminal, Read as _};
use std::time::Duration;

use serde_json::{Value, json};
use silicon_accounts_client::{DevicePoll, TokenResponse};

use crate::ctx::{Ctx, WEB_URL};
use crate::error::{CliError, CliResult, SIGN_IN_HINT};
use crate::session::{APP_ID, Session, accounts_client, revoke, rfc3339};

pub struct LoginArgs {
    pub slt: Option<String>,
    pub slt_stdin: bool,
    pub silicon: Option<String>,
    pub stk: Option<String>,
    pub stk_stdin: bool,
    pub force: bool,
    pub label: Option<String>,
    pub open: bool,
}

fn session_json(session: &Session, api_url: &str, verified: Option<bool>) -> Value {
    let mut v = json!({
        "authenticated": true,
        "app_id": session.app_id,
        "uuid": session.account.uuid,
        "id": session.account.id,
        "kind": session.account.kind,
        "display_name": session.account.display_name,
        "method": session.method,
        "expires_at": rfc3339(session.expires_at),
        "api_url": api_url,
        "accounts_url": session.accounts_url,
    });
    if let Some(at) = session.refresh_expires_at {
        v["refresh_expires_at"] = json!(rfc3339(at));
    }
    if let Some(email) = &session.account.email {
        v["email"] = json!(email);
    }
    if let Some(verified) = verified {
        v["verified"] = json!(verified);
    }
    v
}

fn kind_word(kind: &str) -> &'static str {
    if kind == "silicon" { "a Silicon" } else { "a Carbon" }
}

fn machine_label() -> String {
    let host = ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        });
    match host {
        Some(h) => format!("calc CLI on {h}"),
        None => "calc CLI".to_owned(),
    }
}

fn read_stdin_secret(what: &str, flag: &str) -> CliResult<String> {
    if std::io::stdin().is_terminal() {
        return Err(CliError::usage(
            format!("{flag} reads the {what} from stdin, but stdin is a terminal."),
            format!("Pipe it in, for example: printf '%s' \"$VALUE\" | calc login {flag} …"),
        ));
    }
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).map_err(|e| CliError::file(format!("Can't read the {what} from stdin: {e}.")))?;
    let v = buf.trim().to_owned();
    if v.is_empty() {
        return Err(CliError::usage(format!("The {what} on stdin is empty."), format!("Pipe the {what} itself into {flag}.")));
    }
    Ok(v)
}

fn check_slt(slt: &str) -> CliResult<()> {
    if slt.starts_with("slt_") && slt.len() > 8 {
        return Ok(());
    }
    let what = if slt.starts_with("sar_") {
        "a refresh token"
    } else if slt.starts_with("eyJ") {
        "an access token"
    } else if slt.starts_with("stk-") {
        "an STK"
    } else {
        "not a short-lived token"
    };
    Err(CliError::usage(
        format!("That is {what}; calc login --slt takes a short-lived token, which starts with slt_."),
        "Get one for Silicon Calc with: silicon-accounts login --app calc -q (it works once, for 2 minutes).",
    ))
}

fn check_stk(stk: &str) -> CliResult<()> {
    let ok = stk
        .strip_prefix("stk-")
        .is_some_and(|hex| (8..=32).contains(&hex.len()) && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if ok {
        Ok(())
    } else {
        Err(CliError::usage_code(
            "invalid_stk",
            "That is not an STK: an STK is stk- followed by 8 to 32 hex characters.",
            "Pass the STK exactly as it was shown when the Silicon was made.",
        ))
    }
}

/// Which way this `login` signs in.
enum Way {
    Device,
    Slt(String),
    Silicon { id: String, stk: String },
}

fn way(ctx: &Ctx, args: &LoginArgs) -> CliResult<Way> {
    if args.slt_stdin || args.slt.as_deref() == Some("-") {
        let slt = read_stdin_secret("short-lived token", "--slt-stdin")?;
        check_slt(&slt)?;
        return Ok(Way::Slt(slt));
    }
    if let Some(slt) = &args.slt {
        let slt = slt.trim().to_owned();
        check_slt(&slt)?;
        return Ok(Way::Slt(slt));
    }
    let env = |n: &str| std::env::var(n).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let silicon = args.silicon.clone().or_else(|| env("ACCOUNTS_SILICON"));
    let Some(id) = silicon else {
        if args.stk.is_some() || args.stk_stdin {
            return Err(CliError::usage(
                "An STK was given without the Silicon it belongs to.",
                "Add --silicon si:you (or set ACCOUNTS_SILICON).",
            ));
        }
        return Ok(Way::Device);
    };
    let id = id.trim().to_ascii_lowercase();
    if !id.starts_with("si:") || id.len() < 4 {
        return Err(CliError::usage_code(
            "invalid_id",
            format!("\"{id}\" is not a Silicon's id; Silicons are named si:something."),
            "Carbons sign in with plain calc login (a code approved in the browser).",
        ));
    }
    let stk = if args.stk_stdin {
        read_stdin_secret("STK", "--stk-stdin")?
    } else if let Some(stk) = &args.stk {
        ctx.warn("--stk puts the STK where every process on this machine can see it; prefer --stk-stdin or ACCOUNTS_STK.");
        stk.trim().to_owned()
    } else if let Some(stk) = env("ACCOUNTS_STK") {
        stk
    } else if ctx.interactive() {
        rpassword::prompt_password(format!("STK for {id} (hidden): "))
            .map_err(|e| CliError::file(format!("Can't read the STK: {e}.")))?
            .trim()
            .to_owned()
    } else {
        return Err(CliError::usage(
            format!("No STK was given for {id}."),
            "Pipe it with --stk-stdin, or set ACCOUNTS_STK.",
        ));
    };
    check_stk(&stk)?;
    Ok(Way::Silicon { id, stk })
}

pub async fn login(ctx: &Ctx, args: LoginArgs) -> CliResult<()> {
    let way = way(ctx, &args)?;
    // An unreadable session file is replaced by --force (there is nothing in it to end).
    let existing = match ctx.store.load() {
        Ok(s) => s,
        Err(_) if args.force => None,
        Err(e) => return Err(e),
    };
    if let Some(old) = &existing
        && !args.force
    {
        let here = crate::ctx::session_matches(old, &ctx.api_url, &ctx.accounts_url);
        if let Way::Silicon { id, .. } = &way
            && here
            && old.account.id.eq_ignore_ascii_case(id)
            && !old.ended()
        {
            let value = session_json(old, &ctx.api_url, None);
            ctx.emit(&value, || format!("Already signed in to Silicon Calc as {id}."));
            return Ok(());
        }
        let message = if here {
            format!("You are already signed in to Silicon Calc as {}.", old.account.id)
        } else {
            format!(
                "You are already signed in as {} to the Silicon Calc at {}; this CALC_HOME holds one sign-in.",
                old.account.id, old.api_url
            )
        };
        return Err(CliError::new(
            "conflict",
            message,
            "Sign out first (calc logout), pass --force to sign in again here (that ends the other sign-in), or use another CALC_HOME.",
        ));
    }
    let accounts = accounts_client(&ctx.accounts_url)?;
    let label = args.label.clone().unwrap_or_else(machine_label);
    let (tokens, method): (TokenResponse, &str) = match way {
        Way::Slt(slt) => (exchange(&accounts, &slt).await?, "slt"),
        Way::Silicon { id, stk } => {
            ctx.note(format!("Signing {id} in to Silicon Accounts…"));
            let first = accounts.silicon_login(&id, &stk, Some(&label)).await.map_err(CliError::from)?;
            let minted = accounts.with_token(first.access_token.expose()).short_lived_token(APP_ID).await;
            // That sign-in was only for minting the token: end it whatever happened.
            let first_token = first.refresh_token.as_ref().map(|t| t.expose().to_owned()).unwrap_or_else(|| first.access_token.expose().to_owned());
            let slt = match minted {
                Ok(slt) => slt,
                Err(e) => {
                    let _ = accounts.revoke_first_party(&first_token).await;
                    return Err(e.into());
                }
            };
            let exchanged = exchange(&accounts, slt.slt.expose()).await;
            let _ = accounts.revoke_first_party(&first_token).await;
            (exchanged?, "silicon")
        }
        Way::Device => (device_flow(ctx, &accounts, &label, args.open).await?, "device"),
    };
    let mut session = Session::from_tokens(&tokens, &ctx.accounts_url, &ctx.api_url, method);
    if session.account.uuid.is_empty() || session.account.id.is_empty() {
        // The token answer had no account: ask Silicon Calc who this is.
        let me = ctx.client.clone().with_token(session.access_token.clone()).me().await?;
        session.account.uuid = me.uuid;
        session.account.id = me.id;
        session.account.kind = me.kind;
        session.account.display_name = me.display_name;
    }
    {
        let _lock = ctx.store.lock().await?;
        ctx.store.save(&session)?;
    }
    let mut value = session_json(&session, &ctx.api_url, None);
    if let Some(old) = existing
        && old.refresh_token != session.refresh_token
    {
        // The replaced sign-in can't be used from here again: end it.
        let token = old.refresh_token.clone().unwrap_or(old.access_token.clone());
        let revoked = revoke(&old.accounts_url, &old.app_id, &token).await.is_ok();
        value["replaced"] = json!({"id": old.account.id, "uuid": old.account.uuid, "revoked": revoked});
    }
    ctx.emit(&value, || {
        format!(
            "Signed in to Silicon Calc as {} ({}, {}).",
            session.account.id,
            session.account.display_name,
            kind_word(&session.account.kind)
        )
    });
    ctx.note("Next: calc ls lists your sheets; calc new \"Title\" makes one; calc docs is the guide.");
    Ok(())
}

async fn exchange(accounts: &silicon_accounts_client::AccountsClient, slt: &str) -> CliResult<TokenResponse> {
    accounts.exchange_slt_public_client(APP_ID, slt).await.map_err(|e| {
        let mut err = CliError::from(e);
        if err.code == "invalid_grant" {
            err.code = "signed_out".into();
            err.hint = Some(
                "Short-lived tokens work once, for 2 minutes, for one app. Get a fresh one (silicon-accounts login --app calc -q) and sign in straight away, or use calc login --silicon si:you --stk-stdin."
                    .into(),
            );
        }
        if err.code == "unauthorized_client" || err.code == "invalid_client" {
            err.hint = Some("Silicon Accounts doesn't let the calc app take short-lived tokens without a secret here; check --accounts-url points at the Silicon Accounts Silicon Calc uses.".into());
        }
        err
    })
}

async fn device_flow(
    ctx: &Ctx,
    accounts: &silicon_accounts_client::AccountsClient,
    label: &str,
    open: bool,
) -> CliResult<TokenResponse> {
    let auth = accounts.app_device_authorize(APP_ID, Some("email"), Some(label)).await?;
    let link = auth.verification_uri_complete.clone().unwrap_or_else(|| auth.verification_uri.clone());
    let minutes = auth.expires_in.div_ceil(60);
    if ctx.json {
        eprintln!(
            "{}",
            json!({
                "event": "device_code",
                "user_code": auth.user_code,
                "verification_uri": auth.verification_uri,
                "verification_uri_complete": auth.verification_uri_complete,
                "expires_in": auth.expires_in,
                "interval": auth.interval,
            })
        );
    } else {
        eprintln!("To sign in to Silicon Calc as a Carbon, open\n  {link}\nand confirm the code {} (it expires in {minutes} minutes).", auth.user_code);
        eprintln!("(Silicons don't approve codes: calc login --silicon si:you --stk-stdin.)");
        eprintln!("Waiting for approval…");
    }
    if open && let Err(e) = open_browser(&link) {
        ctx.warn(format!("Couldn't open a browser ({e}); open the link yourself."));
    }
    let started = std::time::Instant::now();
    let mut interval = Duration::from_secs(auth.interval.max(1));
    let wait = async {
        loop {
            tokio::time::sleep(interval).await;
            match accounts.app_device_poll(APP_ID, auth.device_code.expose()).await {
                Ok(DevicePoll::Tokens(tokens)) => return Ok(*tokens),
                Ok(DevicePoll::Pending) => {}
                Ok(DevicePoll::SlowDown) => interval += Duration::from_secs(5),
                Ok(DevicePoll::Denied) => {
                    return Err(CliError::new(
                        "denied",
                        format!("The sign-in code {} was denied on the account site.", auth.user_code),
                        "Run calc login again if that was a mistake.",
                    ));
                }
                Ok(DevicePoll::Expired) => return Err(expired(&auth.user_code)),
                Ok(_) => {}
                Err(e) if e.is_transport() || e.status().is_some_and(|s| s >= 500 || s == 429) => {}
                Err(e) => return Err(e.into()),
            }
            if started.elapsed() + interval > Duration::from_secs(auth.expires_in.max(1)) {
                return Err(expired(&auth.user_code));
            }
        }
    };
    tokio::select! {
        r = wait => r,
        _ = tokio::signal::ctrl_c() => Err(CliError::interrupted(format!(
            "Sign-in stopped before the code {} was approved; the code stops working by itself.", auth.user_code
        ))),
    }
}

fn expired(code: &str) -> CliError {
    CliError::new(
        "expired",
        format!("The sign-in code {code} expired before it was approved."),
        "Run calc login again and approve the new code within 10 minutes.",
    )
}

fn open_browser(url: &str) -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// `login status`: with `--json` it always exits 0 and reports `authenticated`; without, signed out is exit 1.
/// Signed out (or `--offline`), it only reads the session file: no network, no writes.
pub async fn status(ctx: &Ctx, offline: bool) -> CliResult<()> {
    if let Some(problem) = &ctx.session_problem {
        return signed_out(ctx, Some((problem.code.clone(), problem.message.clone())));
    }
    if ctx.elsewhere.is_some() {
        return signed_out(ctx, Some(("other_service".into(), ctx.signed_out_error().message)));
    }
    let Some(session) = ctx.provider.session().await else { return signed_out(ctx, None) };
    if session.ended() {
        return signed_out(ctx, Some(("sign_in_ended".into(), format!("The sign-in as {} reached its end.", session.account.id))));
    }
    if offline {
        let value = session_json(&session, &ctx.api_url, Some(false));
        ctx.emit(&value, || readable_status(&session, ctx, false, None));
        return Ok(());
    }
    // Make sure the tokens still work: refresh when due, then ask Silicon Calc who this is.
    match ctx.provider.valid_token().await {
        Ok(_) => {}
        Err(e) if e.code == "signed_out" => return signed_out(ctx, Some((e.code, e.message))),
        Err(e) => {
            let mut value = session_json(&session, &ctx.api_url, Some(false));
            value["warning"] = json!(format!("Couldn't check the sign-in: {} Showing the stored one.", e.message));
            ctx.emit(&value, || readable_status(&session, ctx, false, Some(&e.message)));
            return Ok(());
        }
    }
    let session = ctx.provider.session().await.unwrap_or(session);
    match tokio::time::timeout(Duration::from_secs(15), ctx.client.me()).await {
        Ok(Ok(me)) => {
            let mut value = session_json(&session, &ctx.api_url, Some(true));
            if !me.id.is_empty() {
                value["id"] = json!(me.id);
            }
            if !me.display_name.is_empty() {
                value["display_name"] = json!(me.display_name);
            }
            ctx.emit(&value, || readable_status(&session, ctx, true, None));
            Ok(())
        }
        Ok(Err(e)) if e.is_signed_out() => signed_out(ctx, Some((e.code().to_owned(), e.message().to_owned()))),
        Ok(Err(e)) => {
            let mut value = session_json(&session, &ctx.api_url, Some(false));
            value["warning"] = json!(format!("Silicon Calc couldn't confirm the sign-in: {}", e.message()));
            ctx.emit(&value, || readable_status(&session, ctx, false, Some(e.message())));
            Ok(())
        }
        Err(_) => {
            let mut value = session_json(&session, &ctx.api_url, Some(false));
            value["warning"] = json!("Silicon Calc didn't answer within 15 seconds; showing the stored sign-in.");
            ctx.emit(&value, || readable_status(&session, ctx, false, Some("no answer within 15 seconds")));
            Ok(())
        }
    }
}

fn readable_status(session: &Session, ctx: &Ctx, verified: bool, problem: Option<&str>) -> String {
    let how = match session.method.as_str() {
        "device" => "a code approved in the browser",
        "silicon" => "si:id and STK",
        _ => "a short-lived token",
    };
    let mut out = format!(
        "Signed in to Silicon Calc as {} ({}, {}), with {how}.\n",
        session.account.id,
        session.account.display_name,
        kind_word(&session.account.kind)
    );
    out.push_str(&format!("access token  {} (refreshed by itself)\n", crate::output::when_ago(&rfc3339(session.expires_at))));
    if let Some(at) = session.refresh_expires_at {
        out.push_str(&format!("sign-in ends  {}\n", crate::output::when(&rfc3339(at))));
    }
    out.push_str(&format!("service       {} ({})\n", ctx.api_url, ctx.api_url_source));
    out.push_str(&format!("accounts      {} ({})\n", session.accounts_url, ctx.accounts_url_source));
    out.push_str(&format!("session file  {}\n", ctx.store.path().display()));
    match (verified, problem) {
        (true, _) => out.push_str("checked       just now with Silicon Calc\n"),
        (false, Some(p)) => out.push_str(&format!("checked       not now ({p})\n")),
        (false, None) => out.push_str("checked       not checked (--offline)\n"),
    }
    out
}

fn signed_out(ctx: &Ctx, reason: Option<(String, String)>) -> CliResult<()> {
    if ctx.json {
        let mut value = json!({"authenticated": false, "app_id": APP_ID});
        if let Some((code, message)) = &reason {
            value["reason"] = json!(code);
            value["message"] = json!(message);
        }
        ctx.emit(&value, String::new);
        return Ok(());
    }
    let message = match reason {
        Some((_, m)) => format!("You are not signed in to Silicon Calc: {m}"),
        None => "You are not signed in to Silicon Calc.".to_owned(),
    };
    Err(CliError::new("signed_out", message, SIGN_IN_HINT))
}

pub async fn logout(ctx: &Ctx) -> CliResult<()> {
    let _lock = ctx.store.lock().await?;
    let session = match ctx.store.load() {
        Ok(s) => s,
        Err(e) if e.code == "session_unreadable" => {
            ctx.store.delete()?;
            ctx.emit(&json!({"signed_out": true, "revoked": false, "note": "The stored sign-in couldn't be read, so it was deleted without being ended at Silicon Accounts."}), || {
                "Deleted the stored sign-in (it couldn't be read, so it wasn't ended at Silicon Accounts).".into()
            });
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let Some(session) = session else {
        ctx.emit(&json!({"signed_out": false, "reason": "not_signed_in"}), || "You weren't signed in.".into());
        return Ok(());
    };
    let token = session.refresh_token.clone().unwrap_or_else(|| session.access_token.clone());
    let revoked = revoke(&session.accounts_url, &session.app_id, &token).await;
    ctx.store.delete()?;
    let mut value = json!({"signed_out": true, "id": session.account.id, "uuid": session.account.uuid, "revoked": revoked.is_ok()});
    if let Err(e) = &revoked {
        value["warning"] = json!(e.message);
    }
    ctx.emit(&value, || match &revoked {
        Ok(()) => format!("Signed out {}, and ended the sign-in at Silicon Accounts.", session.account.id),
        Err(e) => format!("Signed out {} on this machine. {}", session.account.id, e.message),
    });
    Ok(())
}

pub async fn whoami(ctx: &Ctx) -> CliResult<()> {
    ctx.require_session()?;
    let me = ctx.client.me().await?;
    ctx.emit(&me, || {
        format!(
            "{} · {} · {}, signed in to Silicon Calc at {}",
            me.label(),
            me.display_name,
            kind_word(&me.kind).trim_start_matches("a "),
            ctx.api_url
        )
    });
    Ok(())
}

/// `accounts --json`: how Silicon Calc signs in. Offline: no network, nothing written, never fails.
pub fn accounts(api_url: Option<String>, accounts_url: Option<String>) -> Value {
    let stored = crate::session::home().ok().and_then(|(dir, _)| crate::session::Store::new(dir).load().ok().flatten());
    let pick = |flag: Option<String>, from: Option<String>, default: &str| {
        flag.map(|u| u.trim().trim_end_matches('/').to_owned())
            .filter(|u| !u.is_empty())
            .or(from.filter(|u| !u.is_empty()))
            .unwrap_or_else(|| default.to_owned())
    };
    let api = pick(api_url, stored.as_ref().map(|s| s.api_url.clone()), crate::ctx::DEFAULT_API_URL);
    let acc = pick(accounts_url, stored.as_ref().map(|s| s.accounts_url.clone()), crate::ctx::DEFAULT_ACCOUNTS_URL);
    json!({
        "app_id": APP_ID,
        "client_id": APP_ID,
        "name": "Silicon Calc",
        "version": env!("CARGO_PKG_VERSION"),
        "accounts_url": acc,
        "api_url": api,
        "web_url": WEB_URL,
        "device_flow": true,
        "public_client": true,
        "sign_in": {
            "carbon": "calc login",
            "silicon": "calc login --silicon si:you --stk-stdin",
            "silicon_with_token": "silicon-accounts login --app calc -q | calc login --slt-stdin",
        },
        "status": "calc login status --json",
        "client": "silicon-accounts-client",
        "rust_package": "silicon-calc-client",
        "install": "silicon-apps install calc",
        "guide_command": "calc docs",
        "telemetry": false,
    })
}

/// `accounts` without `--json`: the same, readable.
pub fn accounts_text(v: &Value) -> String {
    let s = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let sign_in = |key: &str| v.pointer(&format!("/sign_in/{key}")).and_then(Value::as_str).unwrap_or("").to_owned();
    let mut out = format!("{} (app id {}), calc {}\n", s("name"), s("app_id"), s("version"));
    let mut row = |k: &str, val: String| out.push_str(&format!("{k:<15}{val}\n"));
    row("service", s("api_url"));
    row("accounts", s("accounts_url"));
    row("web app", s("web_url"));
    row("Carbons", format!("{}  (approve a code in the browser)", sign_in("carbon")));
    row("Silicons", format!("{}  (si:id and STK)", sign_in("silicon")));
    row("", format!("{}  (a short-lived token)", sign_in("silicon_with_token")));
    row("signed in?", s("status"));
    row("install", s("install"));
    row("guide", s("guide_command"));
    row("telemetry", "none: calc records and sends nothing about how it is used".into());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_tokens_before_spending_them() {
        assert!(check_slt("slt_cO2a3ENrXHZBq36SkLVBtAuQMINeLP4k2xM93ur-2HE").is_ok());
        let e = check_slt("sar_abc").unwrap_err();
        assert_eq!(e.exit, 2);
        assert!(e.message.contains("refresh token"));
        assert!(check_slt("eyJhbGciOi").unwrap_err().message.contains("access token"));
        assert!(check_stk("stk-59e5f08f3bbe").is_ok());
        assert_eq!(check_stk("stk-xyz").unwrap_err().code, "invalid_stk");
        assert!(check_stk("59e5f08f3bbe").is_err());
    }

    #[test]
    fn accounts_json_names_the_app() {
        let v = accounts(Some("http://127.0.0.1:8730/".into()), Some("http://127.0.0.1:9989".into()));
        assert_eq!(v["app_id"], "calc");
        assert_eq!(v["api_url"], "http://127.0.0.1:8730");
        assert_eq!(v["accounts_url"], "http://127.0.0.1:9989");
        assert_eq!(v["public_client"], true);
        assert_eq!(v["telemetry"], false);
        let text = accounts_text(&v);
        assert!(text.starts_with("Silicon Calc (app id calc), calc "), "{text}");
        assert!(text.contains("service        http://127.0.0.1:8730\n"), "{text}");
        assert!(text.contains("calc login --silicon si:you --stk-stdin"), "{text}");
    }
}
