//! Versions: every change is kept with who made it and when. List them, show what one changed, read the sheet as
//! it was, name and unname versions, compare two, restore (all, one tab or one range), undo everything one
//! account changed since a version, and make a new sheet from a version.

use std::collections::BTreeMap;

use serde_json::json;
use silicon_calc_client::{CellDiff, CompareQuery, RestoreRequest, TabRef, Version, WorkbookDiff, a1};

use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{Table, ago, plural, when};
use crate::resolve::{self, Target};

/// What a diff found, tab by tab, with the changed cells.
pub fn render_diff(diff: &WorkbookDiff) -> String {
    let mut out = String::new();
    let mut by_tab: BTreeMap<u32, Vec<&CellDiff>> = BTreeMap::new();
    for c in &diff.cells {
        by_tab.entry(c.tab).or_default().push(c);
    }
    let mut tabs: Vec<(u32, String)> = diff.tabs.iter().map(|t| (t.tab, t.name.clone())).collect();
    for id in by_tab.keys() {
        if !tabs.iter().any(|(t, _)| t == id) {
            tabs.push((*id, format!("tab {id}")));
        }
    }
    for (id, name) in tabs {
        let td = diff.tabs.iter().find(|t| t.tab == id);
        let mut parts: Vec<String> = Vec::new();
        if let Some(t) = td {
            if t.added {
                parts.push("added".into());
            }
            if t.removed {
                parts.push("removed".into());
            }
            if let Some(old) = &t.renamed_from {
                parts.push(format!("renamed from \"{old}\""));
            }
            if t.reordered {
                parts.push("moved among the tabs".into());
            }
            if t.cells_changed > 0 {
                parts.push(format!("{} changed", plural(t.cells_changed, "cell", "cells")));
            }
            for (n, one, many, verb) in [
                (t.rows_added, "row", "rows", "added"),
                (t.rows_removed, "row", "rows", "removed"),
                (t.cols_added, "column", "columns", "added"),
                (t.cols_removed, "column", "columns", "removed"),
            ] {
                if n > 0 {
                    parts.push(format!("{} {verb}", plural(u64::from(n), one, many)));
                }
            }
        }
        let cells = by_tab.get(&id);
        if parts.is_empty() && cells.is_none() {
            continue;
        }
        out.push_str(&format!("{}: {}\n", name, if parts.is_empty() { "changed".to_owned() } else { parts.join(", ") }));
        if let Some(cells) = cells {
            let mut t = Table::new(&["cell", "before", "after", "by"]);
            for c in cells {
                let by = c.by.as_ref().map(|b| b.label(&diff.accounts)).unwrap_or_default();
                t.row(vec![c.address.clone(), c.before.to_string(), c.after.to_string(), by]);
            }
            for line in t.render().lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    if diff.names_changed {
        out.push_str("Defined names changed.\n");
    }
    if diff.charts_changed {
        out.push_str("Charts changed.\n");
    }
    if diff.truncated {
        let listed = diff.cells.len() as u64;
        out.push_str(&format!(
            "… listing {} of {}; --limit lists more.\n",
            plural(listed, "cell", "cells"),
            plural(diff.cells_changed, "changed cell", "changed cells")
        ));
    }
    if out.is_empty() {
        out.push_str("No differences.\n");
    }
    out
}

fn render_versions(versions: &[Version]) -> String {
    let mut t = Table::new(&["version", "when", "who", "kind", "name", "what"]).right(0);
    for v in versions {
        let span = if v.from_version < v.version { format!("{}–{}", v.from_version, v.version) } else { v.version.to_string() };
        let at = if v.ended_at.is_empty() { &v.started_at } else { &v.ended_at };
        t.row(vec![span, ago(at), v.by.label().to_owned(), v.kind.as_str().to_owned(), v.name.clone().unwrap_or_default(), v.summary.clone()]);
    }
    t.render()
}

pub async fn ls(ctx: &Ctx, sheet: &str, copy: Option<&str>, limit: Option<u32>, all: bool) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let mut versions: Vec<Version> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut next: Option<String>;
    loop {
        let page = ctx.client.versions(&target.line, cursor.as_deref(), Some(limit.unwrap_or(50).clamp(1, 500))).await?;
        versions.extend(page.versions);
        next = page.next_cursor;
        match &next {
            Some(c) if all && Some(c) != cursor.as_ref() => cursor = Some(c.clone()),
            _ => break,
        }
    }
    ctx.emit(&json!({"versions": versions, "next_cursor": next}), || {
        if versions.is_empty() {
            return format!("{} has no versions yet.", target.place());
        }
        format!("{} · latest version {}\n{}", target.place(), target.version, render_versions(&versions))
    });
    if next.is_some() && !all {
        ctx.note("There are older versions: add --all to list them all.");
    }
    Ok(())
}

/// The version entry holding operation `v`.
async fn find_version(ctx: &Ctx, target: &Target, v: i64) -> CliResult<Option<Version>> {
    let mut cursor: Option<String> = None;
    for _ in 0..100 {
        let page = ctx.client.versions(&target.line, cursor.as_deref(), Some(200)).await?;
        if let Some(found) = page.versions.iter().find(|x| x.from_version <= v && v <= x.version) {
            return Ok(Some(found.clone()));
        }
        // Newest first: once a page reaches below v, it isn't there.
        if page.versions.last().is_some_and(|x| x.version < v) {
            return Ok(None);
        }
        match page.next_cursor {
            Some(c) if Some(&c) != cursor.as_ref() => cursor = Some(c),
            _ => break,
        }
    }
    Ok(None)
}

pub async fn show(ctx: &Ctx, sheet: &str, copy: Option<&str>, version: &str, limit: Option<u32>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, version, target.version).await?;
    let entry = find_version(ctx, &target, v).await?;
    let (from, to) = match &entry {
        Some(e) => ((e.from_version - 1).max(0), e.version),
        None => ((v - 1).max(0), v),
    };
    let diff = ctx.client.compare(&target.line, &CompareQuery { from, to: Some(to), limit, tab: None, range: None }).await?;
    ctx.emit(&json!({"version": entry, "from": from, "to": to, "diff": diff}), || {
        let mut out = String::new();
        match &entry {
            Some(e) => {
                let name = e.name.as_deref().map(|n| format!(" \"{n}\"")).unwrap_or_default();
                let span = if e.from_version < e.version { format!("versions {}–{}", e.from_version, e.version) } else { format!("version {}", e.version) };
                out.push_str(&format!("{span}{name} of {}: {}\n", target.place(), e.summary));
                out.push_str(&format!("by {} ({}), {} to {} ({})\n\n", e.by.label(), e.by.display_name, when(&e.started_at), when(&e.ended_at), e.kind.as_str()));
            }
            None => out.push_str(&format!("Version {v} of {}\n\n", target.place())),
        }
        out.push_str(&render_diff(&diff));
        out
    });
    Ok(())
}

pub async fn name(ctx: &Ctx, sheet: &str, copy: Option<&str>, version: &str, name: &str) -> CliResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::usage("The name is empty.", "Name it for what it is, like \"Sent to the client\"."));
    }
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, version, target.version).await?;
    let answer = ctx.client.name_version(&target.line, v, name).await?;
    let value = answer.map(|a| serde_json::to_value(a).unwrap_or_default()).unwrap_or_else(|| json!({"version": v, "name": name}));
    ctx.emit(&value, || format!("Named version {v} of {} \"{name}\".", target.place()));
    Ok(())
}

pub async fn unname(ctx: &Ctx, sheet: &str, copy: Option<&str>, version: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, version, target.version).await?;
    ctx.client.unname_version(&target.line, v).await?;
    ctx.emit(&json!({"version": v, "name": null}), || format!("Version {v} of {} has no name now.", target.place()));
    Ok(())
}

pub struct CompareArgs {
    pub sheet: String,
    pub copy: Option<String>,
    pub from: String,
    pub to: Option<String>,
    pub tab: Option<String>,
    pub range: Option<String>,
    pub limit: Option<u32>,
}

pub async fn compare(ctx: &Ctx, a: CompareArgs) -> CliResult<()> {
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let from = resolve::version(ctx, &target.line, &a.from, target.version).await?;
    let to = match &a.to {
        Some(t) => Some(resolve::version(ctx, &target.line, t, target.version).await?),
        None => None,
    };
    let (tab_arg, range) = match &a.range {
        Some(r) => resolve::split_range(Some(r), a.tab.as_deref())?,
        None => (a.tab.clone(), None),
    };
    let tab = match &tab_arg {
        Some(t) => Some(TabRef::Id(target.tab(Some(t))?.id)),
        None if range.is_some() => Some(TabRef::Id(target.tab(None)?.id)),
        None => None,
    };
    let diff = ctx.client.compare(&target.line, &CompareQuery { from, to, limit: a.limit, tab, range }).await?;
    ctx.emit(&diff, || {
        let to_text = to.map(|t| format!("version {t}")).unwrap_or_else(|| format!("now (version {})", target.version));
        format!("{} from version {from} to {to_text}:\n{}", target.place(), render_diff(&diff))
    });
    Ok(())
}

pub async fn restore(ctx: &Ctx, sheet: &str, copy: Option<&str>, version: &str, tab: Option<&str>, range: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, version, target.version).await?;
    let (tab_arg, range) = match range {
        Some(r) => resolve::split_range(Some(r), tab)?,
        None => (tab.map(str::to_owned), None),
    };
    // The tab is named as it was at that version (it may have been renamed or deleted since).
    let tab_ref = match &tab_arg {
        Some(name) => {
            let then = ctx.client.tabs_at(&target.line, v).await?;
            let t = resolve::find_tab(&then.tabs, Some(name), &format!("{} at version {v}", target.place()))?;
            Some(TabRef::Id(t.id))
        }
        None if range.is_some() => Some(TabRef::Id(target.tab(None)?.id)),
        None => None,
    };
    let answer = ctx.client.restore_version(&target.line, v, &RestoreRequest { tab: tab_ref, range: range.clone() }).await?;
    ctx.emit(&answer, || {
        let what = match (&tab_arg, &range) {
            (Some(t), Some(r)) => a1::with_tab(t, r),
            (Some(t), None) => format!("the tab \"{t}\""),
            (None, Some(r)) => r.clone(),
            (None, None) => target.place(),
        };
        format!(
            "Restored {what} as it was at version {v}. That is itself a new version ({}), so nothing was lost.",
            answer.version
        )
    });
    Ok(())
}

pub async fn undo_account(ctx: &Ctx, sheet: &str, copy: Option<&str>, account: &str, since: &str) -> CliResult<()> {
    let id = resolve::account_id(account)?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, since, target.version).await?;
    let answer = ctx.client.undo_account(&target.line, v, &id).await?;
    ctx.emit(&answer, || {
        if answer.cells_changed == 0 {
            format!("{id} changed nothing in {} after version {v} that is still there to undo.", target.place())
        } else {
            format!(
                "Undid everything {id} changed in {} after version {v}: {} put back, everyone else's changes kept (version {}).",
                target.place(),
                plural(answer.cells_changed, "cell", "cells"),
                answer.version
            )
        }
    });
    Ok(())
}

pub async fn new_sheet(ctx: &Ctx, sheet: &str, copy: Option<&str>, version: &str, title: Option<String>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let v = resolve::version(ctx, &target.line, version, target.version).await?;
    let s = ctx.client.version_to_new_sheet(&target.line, v, title.as_deref()).await?;
    ctx.emit(&s, || format!("Made the sheet \"{}\" ({}) from version {v} of {}.", s.title, s.id, target.place()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_calc_client::{Account, AccountRef, CellSummary, TabDiff, Value, VersionKind};

    #[test]
    fn diffs_read_well() {
        let mut accounts = BTreeMap::new();
        accounts.insert("u1".to_owned(), Account { uuid: "u1".into(), id: "si:analyst".into(), ..Default::default() });
        let diff = WorkbookDiff {
            tabs: vec![
                TabDiff { tab: 1, name: "Sales".into(), cells_changed: 2, rows_added: 1, ..Default::default() },
                TabDiff { tab: 4, name: "Notes".into(), added: true, ..Default::default() },
                TabDiff { tab: 5, name: "Same".into(), ..Default::default() },
            ],
            cells: vec![
                CellDiff {
                    tab: 1,
                    address: "B2".into(),
                    before: CellSummary { value: Value::Number(1200.0), ..Default::default() },
                    after: CellSummary { value: Value::Number(1300.0), ..Default::default() },
                    by: Some(AccountRef::Uuid("u1".into())),
                    ..Default::default()
                },
                CellDiff {
                    tab: 1,
                    address: "C7".into(),
                    after: CellSummary { formula: Some("=SUM(C2:C6)".into()), ..Default::default() },
                    ..Default::default()
                },
            ],
            cells_changed: 5,
            truncated: true,
            accounts,
            ..Default::default()
        };
        let out = render_diff(&diff);
        assert!(out.starts_with("Sales: 2 cells changed, 1 row added\n"), "{out}");
        assert!(out.contains("  B2    1200     1300         si:analyst\n"), "{out}");
        assert!(out.contains("  C7    (empty)  =SUM(C2:C6)\n"), "{out}");
        assert!(out.contains("Notes: added\n"), "{out}");
        assert!(!out.contains("Same"), "{out}");
        assert!(out.contains("… listing 2 cells of 5 changed cells"), "{out}");
        assert_eq!(render_diff(&WorkbookDiff::default()), "No differences.\n");
    }

    #[test]
    fn versions_read_well() {
        let v = Version {
            version: 42,
            from_version: 37,
            by: Account { id: "si:analyst".into(), ..Default::default() },
            started_at: "2026-10-17T08:00:00Z".into(),
            ended_at: "2026-10-17T08:03:00Z".into(),
            summary: "si:analyst changed 340 cells in Sales".into(),
            name: Some("Sent to the client".into()),
            kind: VersionKind::Edit,
        };
        let out = render_versions(&[v]);
        assert!(out.lines().nth(1).unwrap().trim_start().starts_with("37–42 "), "{out}");
        assert!(out.contains("Sent to the client"), "{out}");
    }
}
