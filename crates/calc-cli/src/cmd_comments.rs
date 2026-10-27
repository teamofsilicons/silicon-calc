//! Comments: threads on a cell, a range, whole rows or columns, or a whole tab; replies, edits, reactions,
//! resolving and reopening.

use std::io::{IsTerminal, Read as _};

use serde_json::json;
use silicon_calc_client::{NewThread, TabRef, Thread, ThreadFilter, ThreadStatus, a1};

use crate::cmd_cells::range_or_tab;
use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{ago, one_line, truncate};
use crate::resolve;

/// The longest comment the service takes.
const MAX_BODY: usize = 10_000;

/// A comment's text: the words given (joined with spaces), or stdin for `-`.
pub fn body_text(parts: &[String]) -> CliResult<String> {
    let text = if parts.len() == 1 && parts[0] == "-" {
        if std::io::stdin().is_terminal() {
            return Err(CliError::usage("The comment is read from stdin (-), but stdin is a terminal.", "Pipe the text in, or write it after the command."));
        }
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).map_err(|e| CliError::file(format!("Can't read stdin: {e}.")))?;
        s.trim_end_matches(['\n', '\r']).to_owned()
    } else {
        parts.join(" ")
    };
    if text.trim().is_empty() {
        return Err(CliError::usage("The comment is empty.", "Write what you want to say, for example: \"@si:analyst why did March drop?\""));
    }
    if text.chars().count() > MAX_BODY {
        return Err(CliError::usage(
            format!("The comment is {} characters long; a comment holds at most {MAX_BODY}.", text.chars().count()),
            "Shorten it, or split it into a comment and replies.",
        ));
    }
    Ok(text)
}

/// The `@c:…` / `@si:…` mentions in a text, as written.
pub fn mentions_in(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for (i, _) in text.match_indices('@') {
        // A mention starts a word: not inside an email address.
        if text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_') {
            continue;
        }
        let rest = &text[i + 1..];
        let lower = rest.to_ascii_lowercase();
        let prefix = if lower.starts_with("si:") {
            3
        } else if lower.starts_with("c:") {
            2
        } else {
            continue;
        };
        let name: String =
            rest[prefix..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')).collect();
        let name = name.trim_end_matches(['.', '-']);
        if name.is_empty() {
            continue;
        }
        let id = format!("{}{}", &lower[..prefix], name.to_ascii_lowercase());
        if !found.contains(&id) {
            found.push(id);
        }
    }
    found
}

fn status_word(t: &Thread) -> String {
    let mut s = match t.status {
        ThreadStatus::Open => "open".to_owned(),
        ThreadStatus::Resolved => match &t.resolved_by {
            Some(by) => format!("resolved by {}", by.label()),
            None => "resolved".to_owned(),
        },
    };
    if t.detached {
        s.push_str(", its cells were deleted");
    }
    s
}

/// One thread, readable. `ids` shows the id of every comment (to edit, delete or react to them).
pub fn render_thread(t: &Thread, ids: bool) -> String {
    let mut out = format!("{}  {}  {}\n", t.id, t.place(), status_word(t));
    if let Some(q) = t.quote.as_deref().filter(|q| !q.trim().is_empty()) {
        out.push_str(&format!("  on \"{}\"\n", truncate(&one_line(q), 100)));
    }
    if let (Some(orig), Some(now)) = (&t.original_range, &t.range)
        && orig != now
    {
        out.push_str(&format!("  made on {orig}; it moved with its cells\n"));
    }
    for (i, c) in t.comments.iter().enumerate() {
        let lead = if i == 0 { "  " } else { "  ↳ " };
        let edited = if c.edited_at.is_some() { " (edited)" } else { "" };
        let id = if ids { format!("  [{}]", c.id) } else { String::new() };
        let mut lines = c.body.lines();
        let first = lines.next().unwrap_or("");
        out.push_str(&format!("{lead}{} · {}{edited}: {first}{id}\n", c.author.label(), ago(&c.created_at)));
        let indent = " ".repeat(lead.chars().count() + 2);
        for line in lines {
            out.push_str(&format!("{indent}{line}\n"));
        }
        if !c.reactions.is_empty() {
            let r: Vec<String> = c
                .reactions
                .iter()
                .map(|r| format!("{} {}", r.emoji, r.accounts.iter().map(|a| a.label().to_owned()).collect::<Vec<_>>().join(", ")))
                .collect();
            out.push_str(&format!("{indent}{}\n", r.join(" · ")));
        }
    }
    out
}

/// Tells (on stderr) about mentions that reached nobody: the account can't open the sheet, so it wasn't told.
fn warn_unreached(ctx: &Ctx, without: &[silicon_calc_client::Account], sheet: Option<&str>) {
    if without.is_empty() {
        return;
    }
    let ids: Vec<&str> = without.iter().map(|a| a.label()).collect();
    let share = match sheet {
        Some(s) => format!("calc share add {s} {} --role commenter", ids.join(" ")),
        None => format!("calc share add SHEET {} --role commenter", ids.join(" ")),
    };
    ctx.warn(format!(
        "{} can't open this sheet, so {} not told about the mention. Share it with them first ({share}), then mention them again.",
        ids.join(", "),
        if ids.len() == 1 { "was" } else { "were" }
    ));
}

/// Several threads, a blank line between them.
pub fn render_threads(threads: &[Thread], ids: bool) -> String {
    threads.iter().map(|t| render_thread(t, ids)).collect::<Vec<_>>().join("\n")
}

pub async fn ls(ctx: &Ctx, sheet: &str, copy: Option<&str>, filter: ThreadFilter, tab: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let tab_ref = match tab {
        Some(t) => Some(TabRef::Id(target.tab(Some(t))?.id)),
        None => None,
    };
    let threads = ctx.client.threads(&target.line, filter, tab_ref.as_ref()).await?;
    ctx.emit(&json!({"threads": threads}), || {
        if threads.is_empty() {
            let which = match filter {
                ThreadFilter::Open => "open ",
                ThreadFilter::Resolved => "resolved ",
                ThreadFilter::All => "",
            };
            let hint = if filter == ThreadFilter::Open { " (--all includes resolved ones)" } else { "" };
            return format!("{} has no {which}comments{hint}.", target.place());
        }
        render_threads(&threads, true)
    });
    Ok(())
}

pub async fn show(ctx: &Ctx, thread: &str) -> CliResult<()> {
    let t = ctx.client.thread(thread.trim()).await.map_err(|e| {
        let mut err = CliError::from(e);
        if err.code == "not_found" {
            err.hint = Some("Thread ids come from calc comments ls SHEET (or from the ting that told you about it).".into());
        }
        err
    })?;
    ctx.emit(&t, || render_thread(&t, true));
    Ok(())
}

pub async fn add(ctx: &Ctx, sheet: &str, copy: Option<&str>, place: Option<&str>, tab_flag: Option<&str>, body: &[String]) -> CliResult<()> {
    let body = body_text(body)?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, range) = range_or_tab(&target, place, tab_flag)?;
    let tab = target.tab(tab_arg.as_deref())?.clone();
    let thread = ctx
        .client
        .create_thread(&target.line, &NewThread { tab: TabRef::Id(tab.id), range: range.clone(), body: body.clone() })
        .await?;
    let mentioned: Vec<String> = mentions_in(&body)
        .into_iter()
        .filter(|m| !thread.mentioned_without_access.iter().any(|a| a.id.eq_ignore_ascii_case(m)))
        .collect();
    warn_unreached(ctx, &thread.mentioned_without_access, Some(&target.sheet.id));
    ctx.emit(&thread, || {
        let place = match &range {
            Some(r) => a1::with_tab(&tab.name, r),
            None => format!("the tab {}", a1::quote_tab(&tab.name)),
        };
        let mut s = format!("Commented on {place} in {} (thread {}).", target.place(), thread.id);
        if !mentioned.is_empty() {
            s.push_str(&format!(" {} told through Silicon Ting.", mentioned.join(", ")));
        }
        s
    });
    Ok(())
}

pub async fn reply(ctx: &Ctx, thread: &str, body: &[String]) -> CliResult<()> {
    let body = body_text(body)?;
    let c = ctx.client.reply(thread.trim(), &body).await?;
    warn_unreached(ctx, &c.mentioned_without_access, None);
    ctx.emit(&c, || format!("Replied in thread {} (comment {}).", thread.trim(), c.id));
    Ok(())
}

pub async fn edit(ctx: &Ctx, comment: &str, body: &[String]) -> CliResult<()> {
    let body = body_text(body)?;
    let c = ctx.client.edit_comment(comment.trim(), &body).await?;
    if let Some(c) = &c {
        warn_unreached(ctx, &c.mentioned_without_access, None);
    }
    let value = c.map(|c| serde_json::to_value(c).unwrap_or_default()).unwrap_or_else(|| json!({"id": comment.trim(), "body": body}));
    ctx.emit(&value, || format!("Edited comment {}.", comment.trim()));
    Ok(())
}

pub async fn rm(ctx: &Ctx, comment: &str) -> CliResult<()> {
    ctx.client.delete_comment(comment.trim()).await?;
    ctx.emit(&json!({"deleted": true, "id": comment.trim()}), || {
        format!("Deleted comment {}. It stays in the sheet's history.", comment.trim())
    });
    Ok(())
}

pub async fn resolve_thread(ctx: &Ctx, thread: &str, reopen: bool) -> CliResult<()> {
    let id = thread.trim();
    let t = if reopen { ctx.client.reopen_thread(id).await? } else { ctx.client.resolve_thread(id).await? };
    ctx.emit(&t, || {
        if reopen {
            format!("Reopened the thread on {} ({}).", t.place(), t.id)
        } else {
            format!("Resolved the thread on {} ({}). calc comments reopen {} brings it back.", t.place(), t.id, t.id)
        }
    });
    Ok(())
}

pub async fn react(ctx: &Ctx, comment: &str, emoji: &str, remove: bool) -> CliResult<()> {
    let emoji = emoji.trim();
    if emoji.is_empty() || emoji.chars().count() > 16 {
        return Err(CliError::usage(format!("\"{emoji}\" is not a reaction."), "React with one emoji, like 👍."));
    }
    let id = comment.trim();
    if remove {
        ctx.client.unreact(id, emoji).await?;
        ctx.emit(&json!({"comment": id, "emoji": emoji, "reacted": false}), || format!("Took back your {emoji} on comment {id}."));
    } else {
        let answer = ctx.client.react(id, emoji).await?;
        let value = answer.unwrap_or_else(|| json!({"comment": id, "emoji": emoji, "reacted": true}));
        ctx.emit(&value, || format!("Reacted {emoji} to comment {id}."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_calc_client::{Account, Comment, Reaction};

    #[test]
    fn finds_mentions() {
        assert_eq!(mentions_in("@si:analyst why did March drop? cc @c:Ada."), vec!["si:analyst", "c:ada"]);
        assert_eq!(mentions_in("mail ada@c:x.com"), Vec::<String>::new());
        assert_eq!(mentions_in("@si:analyst and @si:analyst"), vec!["si:analyst"]);
        assert!(mentions_in("@carbon @si: nothing").is_empty());
    }

    #[test]
    fn bodies() {
        assert_eq!(body_text(&["why".into(), "March?".into()]).unwrap(), "why March?");
        assert_eq!(body_text(&["  ".into()]).unwrap_err().exit, 2);
        assert_eq!(body_text(&["x".repeat(MAX_BODY + 1)]).unwrap_err().exit, 2);
    }

    #[test]
    fn threads_read_well() {
        let ada = Account { id: "c:ada".into(), display_name: "Ada".into(), ..Default::default() };
        let analyst = Account { id: "si:analyst".into(), ..Default::default() };
        let t = Thread {
            id: "t1".into(),
            tab_name: "Sales".into(),
            range: Some("B2:B9".into()),
            original_range: Some("B2:B9".into()),
            quote: Some("1,200 | 900".into()),
            comments: vec![
                Comment { id: "c1".into(), author: ada.clone(), body: "@si:analyst why did March drop?".into(), ..Default::default() },
                Comment {
                    id: "c2".into(),
                    author: analyst,
                    body: "Three fewer working days.\nSee Notes!A1.".into(),
                    reactions: vec![Reaction { emoji: "👍".into(), accounts: vec![ada] }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let out = render_thread(&t, true);
        assert!(out.starts_with("t1  Sales!B2:B9  open\n"), "{out}");
        assert!(out.contains("  on \"1,200 | 900\"\n"), "{out}");
        assert!(out.contains("c:ada · "), "{out}");
        assert!(out.contains(": @si:analyst why did March drop?  [c1]\n"), "{out}");
        assert!(out.contains("  ↳ si:analyst · "), "{out}");
        assert!(out.contains("      See Notes!A1.\n"), "{out}");
        assert!(out.contains("👍 c:ada"), "{out}");
        let whole_tab = Thread { tab_name: "By region".into(), range: None, ..t };
        assert!(render_thread(&whole_tab, false).starts_with("t1  'By region'  open"));
    }
}
