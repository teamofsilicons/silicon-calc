//! Live: `watch` streams a sheet's events as they happen (changes, comments, copies, who is here), as readable
//! lines or JSON lines, until told to stop: after a number of events, after a time, or on Ctrl-C. `presence`
//! shows the caller on a tab or range to everyone viewing the sheet.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value as Json, json};
use silicon_calc_client::{
    Change, ChangeEvent, CommentEvent, CopyEvent, CopyStatus, Event, EventOptions, PresenceEvent, PresenceUpdate, TabRef,
    Thread, ThreadFilter, ThreadStatus, a1,
};

use crate::cmd_cells::client_id;
use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{one_line, truncate};
use crate::resolve::{self, Target};

/// The kinds of events `--only` picks from.
pub const KINDS: [&str; 5] = ["change", "comment", "copy", "presence", "reset"];

pub struct WatchArgs {
    pub sheet: String,
    pub copy: Option<String>,
    pub since: Option<String>,
    pub only: Vec<String>,
    pub mentions: bool,
    pub count: Option<u64>,
    pub timeout: Option<u64>,
    pub no_reconnect: bool,
    pub here: Option<String>,
    pub pings: bool,
}

/// `--only change,comment` as a set of event names.
pub fn parse_only(only: &[String]) -> CliResult<HashSet<String>> {
    let mut set = HashSet::new();
    for word in only.iter().flat_map(|o| o.split(',')).map(|w| w.trim().to_ascii_lowercase()).filter(|w| !w.is_empty()) {
        let word = match word.as_str() {
            "changes" | "edit" | "edits" => "change".to_owned(),
            "comments" => "comment".to_owned(),
            "copies" => "copy".to_owned(),
            "here" | "viewers" => "presence".to_owned(),
            _ => word,
        };
        if !KINDS.contains(&word.as_str()) {
            return Err(CliError::usage(format!("\"{word}\" is not a kind of event."), format!("Pick from {}.", KINDS.join(", "))));
        }
        set.insert(word);
    }
    Ok(set)
}

/// What a comment event was, worked out from what was seen before.
#[derive(Default)]
struct ThreadsSeen {
    /// Each thread's status and comment ids.
    threads: HashMap<String, (ThreadStatus, Vec<String>)>,
    /// Each comment's text.
    bodies: HashMap<String, String>,
}

impl ThreadsSeen {
    fn remember(&mut self, t: &Thread) {
        self.threads.insert(t.id.clone(), (t.status, t.comments.iter().map(|c| c.id.clone()).collect()));
        for c in &t.comments {
            self.bodies.insert(c.id.clone(), c.body.clone());
        }
    }

    /// (`created` | `replied` | `edited` | `deleted` | `resolved` | `reopened` | `updated`, the new comments).
    fn classify(&mut self, t: &Thread) -> (&'static str, Vec<usize>) {
        let new: Vec<usize> = t.comments.iter().enumerate().filter(|(_, c)| !self.bodies.contains_key(&c.id)).map(|(i, _)| i).collect();
        let what = match self.threads.get(&t.id) {
            None => "created",
            Some(_) if !new.is_empty() => "replied",
            Some((old, _)) if *old != t.status => {
                if t.status == ThreadStatus::Resolved {
                    "resolved"
                } else {
                    "reopened"
                }
            }
            Some(_) if t.comments.iter().any(|c| self.bodies.get(&c.id).is_some_and(|b| *b != c.body)) => "edited",
            Some((_, ids)) if ids.iter().any(|id| !t.comments.iter().any(|c| c.id == *id)) => "deleted",
            Some(_) => "updated",
        };
        self.remember(t);
        (what, new)
    }
}

struct Watcher {
    tab_names: HashMap<u32, String>,
    threads: ThreadsSeen,
    me: Option<(String, String)>,
    viewers: Option<String>,
}

impl Watcher {
    fn tab_name(&self, id: u32) -> String {
        self.tab_names.get(&id).cloned().unwrap_or_else(|| format!("tab {id}"))
    }

    fn describe_changes(&self, changes: &[Change]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for c in changes {
            let part = match (c.tab, &c.range) {
                (Some(t), Some(r)) => a1::with_tab(&self.tab_name(t), r),
                (Some(t), None) if c.reload => format!("all of {}", a1::quote_tab(&self.tab_name(t))),
                _ if c.tabs => "the tabs".to_owned(),
                _ if c.meta => "names, charts or protections".to_owned(),
                _ => continue,
            };
            if !parts.contains(&part) {
                parts.push(part);
            }
        }
        if parts.len() > 6 {
            let more = parts.len() - 6;
            parts.truncate(6);
            parts.push(format!("{more} more"));
        }
        parts.join(", ")
    }

    fn change_line(&self, c: &ChangeEvent) -> String {
        let mut s = format!("v{}  {} changed {}", c.version, c.by.label(), self.describe_changes(&c.changes));
        if let Some(cells) = &c.cells
            && !cells.is_empty()
        {
            let shown: Vec<String> =
                cells.iter().take(3).map(|cell| format!("{} = {}", cell.a, truncate(&one_line(cell.f.as_deref().unwrap_or(&cell.t)), 30))).collect();
            s.push_str(&format!("  ({}{})", shown.join("; "), if cells.len() > 3 { format!("; {} more", cells.len() - 3) } else { String::new() }));
        }
        s
    }

    fn comment_line(&self, t: &Thread, what: &str, new: &[usize]) -> String {
        let place = t.place();
        let last = new.last().and_then(|i| t.comments.get(*i)).or_else(|| t.comments.last());
        let quote = |c: &silicon_calc_client::Comment| format!("\"{}\"", truncate(&one_line(&c.body), 120));
        match (what, last) {
            ("created", Some(c)) => format!("comment  {} on {place}: {}  (thread {})", c.author.label(), quote(c), t.id),
            ("replied", Some(c)) => format!("reply    {} on {place}: {}  (thread {})", c.author.label(), quote(c), t.id),
            ("edited", Some(c)) => format!("edited   a comment on {place}: {}  (thread {})", quote(c), t.id),
            ("resolved", _) => format!(
                "resolved {} resolved the thread on {place}  (thread {})",
                t.resolved_by.as_ref().map(|a| a.label().to_owned()).unwrap_or_else(|| "someone".into()),
                t.id
            ),
            ("reopened", _) => format!("reopened the thread on {place}  (thread {})", t.id),
            ("deleted", _) => format!("deleted  a comment on {place}  (thread {})", t.id),
            _ => format!("comment  the thread on {place} changed  (thread {})", t.id),
        }
    }

    fn copy_line(c: &CopyEvent) -> String {
        let copy = &c.copy;
        match copy.status {
            CopyStatus::Open => format!("copy     {} made the copy \"{}\" ({}) at version {}", copy.created_by.label(), copy.name, copy.id, copy.forked_at),
            CopyStatus::Merged => format!(
                "copy     {} merged the copy \"{}\" ({})",
                copy.merged_by.as_ref().map(|a| a.label().to_owned()).unwrap_or_else(|| "someone".into()),
                copy.name,
                copy.id
            ),
            CopyStatus::Deleted => format!("copy     the copy \"{}\" ({}) was deleted", copy.name, copy.id),
        }
    }

    fn presence_line(&self, p: &PresenceEvent) -> String {
        if p.viewers.is_empty() {
            return "here     nobody else is viewing".into();
        }
        let mut seen: Vec<String> = Vec::new();
        for v in &p.viewers {
            let place = match (v.tab, &v.range) {
                (Some(t), Some(r)) => format!(" on {}", a1::with_tab(&self.tab_name(t), r)),
                (Some(t), None) => format!(" on {}", a1::quote_tab(&self.tab_name(t))),
                _ => String::new(),
            };
            let entry = format!("{}{place}", v.account.label());
            if !seen.contains(&entry) {
                seen.push(entry);
            }
        }
        format!("here     {}", seen.join(", "))
    }

    fn mentions_me(&self, t: &Thread, new: &[usize]) -> bool {
        let Some((uuid, id)) = &self.me else { return false };
        new.iter().filter_map(|i| t.comments.get(*i)).any(|c| {
            c.mentions.iter().any(|m| m.uuid == *uuid || (!id.is_empty() && m.id.eq_ignore_ascii_case(id)))
                || (!id.is_empty() && c.body.to_ascii_lowercase().contains(&format!("@{}", id.to_ascii_lowercase())))
        })
    }
}

fn clock() -> String {
    let t = time::OffsetDateTime::now_utc();
    format!("{:02}:{:02}:{:02}", t.hour(), t.minute(), t.second())
}

async fn tab_names(ctx: &Ctx, target: &Target) -> HashMap<u32, String> {
    match ctx.client.tabs(&target.line).await {
        Ok(list) => list.tabs.into_iter().map(|t| (t.id, t.name)).collect(),
        Err(_) => target.tabs.iter().map(|t| (t.id, t.name.clone())).collect(),
    }
}

/// The tab and range `--here` / `presence` name: `Sales!B2`, `Sales`, `B2` (the first tab) or nothing.
fn presence_place(target: &Target, place: Option<&str>) -> CliResult<PresenceUpdate> {
    let (tab, range) = crate::cmd_cells::range_or_tab(target, place, None)?;
    let t = target.tab(tab.as_deref())?;
    Ok(PresenceUpdate { tab: TabRef::Id(t.id), range, client_id: client_id() })
}

pub async fn watch(ctx: &Ctx, a: WatchArgs) -> CliResult<()> {
    let only = parse_only(&a.only)?;
    if a.mentions && !only.is_empty() && !only.contains("comment") {
        return Err(CliError::usage("--mentions picks comments, but --only leaves comments out.", "Add comment to --only, or drop --only."));
    }
    if a.count == Some(0) {
        return Err(CliError::usage("--count 0 would stop before anything happens.", "Give --count 1 or more."));
    }
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let since = match &a.since {
        Some(v) => resolve::version(ctx, &target.line, v, target.version).await?,
        None => target.version,
    };
    let me = if a.mentions {
        ctx.require_session()?;
        let me = ctx.client.me().await?;
        Some((me.uuid, me.id))
    } else {
        None
    };
    let mut threads = ThreadsSeen::default();
    if let Ok(existing) = ctx.client.threads(&target.line, ThreadFilter::All, None).await {
        for t in &existing {
            threads.remember(t);
        }
    }
    let mut w = Watcher { tab_names: tab_names(ctx, &target).await, threads, me, viewers: None };
    let mut presence = match &a.here {
        Some(place) => Some(presence_place(&target, Some(place))?),
        None => None,
    };
    if let Some(p) = &presence {
        ctx.client.presence(&target.line, p).await?;
    }
    let options = EventOptions { since: Some(since), reconnect: !a.no_reconnect, ..EventOptions::default() };
    let mut stream = ctx.client.events(&target.line, options);
    ctx.note(format!(
        "Watching {} from version {since}{}. Ctrl-C stops.",
        target.place(),
        match (a.count, a.timeout) {
            (Some(n), Some(t)) => format!(", until {n} events or {t} seconds"),
            (Some(n), None) => format!(", until {n} events"),
            (None, Some(t)) => format!(", for {t} seconds"),
            (None, None) => String::new(),
        }
    ));
    let deadline = a.timeout.map(|t| tokio::time::Instant::now() + Duration::from_secs(t));
    let mut matched: u64 = 0;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.tick().await;
    loop {
        let sleep_until = async {
            match deadline {
                Some(d) => tokio::time::sleep_until(d).await,
                None => std::future::pending::<()>().await,
            }
        };
        let next = tokio::select! {
            next = stream.next() => next,
            _ = sleep_until => {
                return match a.count {
                    Some(n) if matched < n => Err(CliError::new(
                        "timed_out",
                        format!(
                            "{} of {n} events arrived within {} seconds.",
                            matched,
                            a.timeout.unwrap_or_default()
                        ),
                        "Watch longer with a bigger --timeout, or drop --count to watch without a goal.",
                    ).with_details(json!({"events": matched, "wanted": n}))),
                    _ => Ok(()),
                };
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = heartbeat.tick(), if presence.is_some() => {
                if let Some(p) = &presence
                    && let Err(e) = ctx.client.presence(&target.line, p).await
                {
                    ctx.warn(format!("Couldn't keep showing you as here: {}", e.message()));
                    presence = None;
                }
                continue;
            }
        };
        let Some(event) = next else { return Ok(()) };
        let event = event.map_err(CliError::from)?;
        let (name, mut line_json, text): (&str, Json, Option<String>) = match &event {
            Event::Ping => {
                if a.pings {
                    ("ping", event.to_json(), Some("ping".to_owned()))
                } else {
                    continue;
                }
            }
            Event::Reconnecting { reason, attempt, after } => {
                ctx.warn(format!("The live connection dropped ({reason}); reconnecting in {} s (try {attempt}) from the last version seen.", after.as_secs()));
                continue;
            }
            Event::Change(c) => {
                if c.changes.iter().any(|ch| ch.tabs) {
                    w.tab_names = tab_names(ctx, &target).await;
                }
                ("change", event.to_json(), Some(w.change_line(c)))
            }
            Event::Comment(CommentEvent { thread }) => {
                let (what, new) = w.threads.classify(thread);
                let mine = w.mentions_me(thread, &new);
                if a.mentions && !mine {
                    continue;
                }
                let mut j = event.to_json();
                j["what"] = json!(what);
                if w.me.is_some() {
                    j["mentions_me"] = json!(mine);
                }
                ("comment", j, Some(w.comment_line(thread, what, &new)))
            }
            Event::Copy(c) => ("copy", event.to_json(), Some(Watcher::copy_line(c))),
            Event::Presence(p) => {
                let text = w.presence_line(p);
                if w.viewers.as_deref() == Some(text.as_str()) {
                    continue;
                }
                w.viewers = Some(text.clone());
                ("presence", event.to_json(), Some(text))
            }
            Event::Reset => ("reset", event.to_json(), Some("reset    too much changed at once to describe; read the sheet again".to_owned())),
            Event::Other { event: name, .. } => {
                ("other", event.to_json(), Some(format!("{name}  (an event this calc doesn't know; update calc to read it)")))
            }
        };
        if a.mentions && name != "comment" {
            continue;
        }
        if !only.is_empty() && !only.contains(name) && name != "ping" {
            continue;
        }
        if ctx.json {
            if let Json::Object(m) = &mut line_json {
                m.insert("sheet".into(), json!(target.sheet.id));
                if let Some(c) = &target.copy {
                    m.insert("copy".into(), json!(c.id));
                }
            }
            ctx.line(&line_json);
        } else if let Some(text) = text {
            println!("{}  {text}", clock());
        }
        if name != "ping" {
            matched += 1;
            if a.count.is_some_and(|n| matched >= n) {
                return Ok(());
            }
        }
    }
}

pub async fn presence(ctx: &Ctx, sheet: &str, copy: Option<&str>, place: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let update = presence_place(&target, place)?;
    ctx.client.presence(&target.line, &update).await?;
    let where_ = {
        let tab = target.tabs.iter().find(|t| TabRef::Id(t.id) == update.tab).map(|t| t.name.clone()).unwrap_or_default();
        match &update.range {
            Some(r) => a1::with_tab(&tab, r),
            None => a1::quote_tab(&tab),
        }
    };
    ctx.emit(&json!({"here": true, "tab": update.tab, "range": update.range, "client_id": update.client_id, "expires_in": 45}), || {
        format!(
            "Everyone viewing {} sees you on {where_} for the next 45 seconds (calc watch … --here keeps it up).",
            target.place()
        )
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_calc_client::{Account, Comment};

    #[test]
    fn only_takes_kinds() {
        let s = parse_only(&["change,comments".into(), "copy".into()]).unwrap();
        assert!(s.contains("change") && s.contains("comment") && s.contains("copy"));
        assert!(parse_only(&["nonsense".into()]).is_err());
        assert!(parse_only(&[]).unwrap().is_empty());
    }

    fn thread(comments: &[(&str, &str)], status: ThreadStatus) -> Thread {
        Thread {
            id: "t1".into(),
            tab_name: "Sales".into(),
            range: Some("B7".into()),
            status,
            comments: comments
                .iter()
                .map(|(id, body)| Comment {
                    id: (*id).into(),
                    body: (*body).into(),
                    author: Account { id: "c:ada".into(), ..Default::default() },
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn comment_events_say_what_happened() {
        let mut seen = ThreadsSeen::default();
        assert_eq!(seen.classify(&thread(&[("c1", "why?")], ThreadStatus::Open)).0, "created");
        let (what, new) = seen.classify(&thread(&[("c1", "why?"), ("c2", "because")], ThreadStatus::Open));
        assert_eq!((what, new), ("replied", vec![1]));
        assert_eq!(seen.classify(&thread(&[("c1", "why?!"), ("c2", "because")], ThreadStatus::Open)).0, "edited");
        assert_eq!(seen.classify(&thread(&[("c1", "why?!"), ("c2", "because")], ThreadStatus::Resolved)).0, "resolved");
        assert_eq!(seen.classify(&thread(&[("c1", "why?!"), ("c2", "because")], ThreadStatus::Open)).0, "reopened");
        assert_eq!(seen.classify(&thread(&[("c1", "why?!")], ThreadStatus::Open)).0, "deleted");
        assert_eq!(seen.classify(&thread(&[("c1", "why?!")], ThreadStatus::Open)).0, "updated");
    }

    #[test]
    fn mentions_are_noticed() {
        let w = Watcher {
            tab_names: HashMap::new(),
            threads: ThreadsSeen::default(),
            me: Some(("u-analyst".into(), "si:analyst".into())),
            viewers: None,
        };
        let t = thread(&[("c1", "@si:analyst why did March drop?")], ThreadStatus::Open);
        assert!(w.mentions_me(&t, &[0]));
        let t = thread(&[("c1", "no one here")], ThreadStatus::Open);
        assert!(!w.mentions_me(&t, &[0]));
        let line = w.comment_line(&thread(&[("c1", "why?")], ThreadStatus::Open), "created", &[0]);
        assert_eq!(line, "comment  c:ada on Sales!B7: \"why?\"  (thread t1)");
    }

    #[test]
    fn changes_name_their_places() {
        let w = Watcher {
            tab_names: [(1, "Sales".to_owned()), (2, "By region".to_owned())].into_iter().collect(),
            threads: ThreadsSeen::default(),
            me: None,
            viewers: None,
        };
        let changes = vec![
            Change { tab: Some(1), range: Some("B2:B9".into()), ..Default::default() },
            Change { tab: Some(2), reload: true, ..Default::default() },
            Change { tabs: true, ..Default::default() },
        ];
        assert_eq!(w.describe_changes(&changes), "Sales!B2:B9, all of 'By region', the tabs");
    }
}
