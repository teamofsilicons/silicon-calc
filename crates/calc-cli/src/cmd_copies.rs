//! Copies: a duplicate of a sheet kept inside it, to try something without touching the main sheet. List them
//! (as a tree), make one (of the main sheet or of another copy), rename, delete, compare with where it came from,
//! and merge back in whole or in part.

use serde_json::json;
use silicon_calc_client::a1::{self, RangeKind, RangeRef};
use silicon_calc_client::{Conflict, CopyStatus, MergeRange, MergeRequest, Resolution, Sheet, SheetCopy, Side, TabRef};

use crate::cmd_versions::render_diff;
use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{ago, plural};
use crate::resolve::{self, Look};

fn copy_line(c: &SheetCopy) -> String {
    let status = match c.status {
        CopyStatus::Open => format!("open · version {}", c.version),
        CopyStatus::Merged => match (&c.merged_by, &c.merged_at) {
            (Some(by), Some(at)) => format!("merged by {} {}", by.label(), ago(at)),
            _ => "merged".to_owned(),
        },
        CopyStatus::Deleted => "deleted (its history stays readable)".to_owned(),
    };
    format!(
        "{}  \"{}\"  {status} · made from version {} by {} {}",
        c.id,
        c.name,
        c.forked_at,
        c.created_by.label(),
        ago(&c.created_at)
    )
}

/// The copies as a tree under the main sheet.
pub fn render_tree(sheet: &Sheet, copies: &[SheetCopy]) -> String {
    let mut out = format!("\"{}\"  main sheet · version {}\n", sheet.title, sheet.version);
    fn walk(copies: &[SheetCopy], parent: Option<&str>, prefix: &str, out: &mut String, depth: usize) {
        let mut children: Vec<&SheetCopy> = copies.iter().filter(|c| c.parent_id.as_deref() == parent).collect();
        children.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        for (i, c) in children.iter().enumerate() {
            let last = i + 1 == children.len();
            out.push_str(&format!("{prefix}{}{}\n", if last { "└─ " } else { "├─ " }, copy_line(c)));
            if depth < 64 {
                let next = format!("{prefix}{}", if last { "   " } else { "│  " });
                walk(copies, Some(&c.id), &next, out, depth + 1);
            }
        }
    }
    walk(copies, None, "", &mut out, 0);
    // Copies whose parent isn't listed (closed parents with --all off) still show, at the top level.
    let orphans: Vec<&SheetCopy> = copies
        .iter()
        .filter(|c| c.parent_id.as_deref().is_some_and(|p| !copies.iter().any(|x| x.id == p)))
        .collect();
    for c in orphans {
        out.push_str(&format!("·  {} (made from the copy {})\n", copy_line(c), c.parent_id.as_deref().unwrap_or("")));
    }
    out
}

pub async fn ls(ctx: &Ctx, sheet: &str, all: bool) -> CliResult<()> {
    let s = resolve::sheet(ctx, sheet, Look::Visible).await?;
    let copies = ctx.client.copies(&s.id, all).await?;
    ctx.emit(&json!({"sheet": {"id": s.id, "title": s.title, "version": s.version}, "copies": copies}), || {
        if copies.is_empty() {
            let more = if all { "" } else { " open" };
            return format!(
                "\"{}\" has no{more} copies. Make one with calc copies new {} \"What to try\".",
                s.title, s.id
            );
        }
        render_tree(&s, &copies)
    });
    Ok(())
}

pub async fn new(ctx: &Ctx, sheet: &str, from: Option<&str>, name: &str) -> CliResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::usage("The copy's name is empty.", "Say what the copy is for, like \"Try weekly buckets\"."));
    }
    let target = resolve::target(ctx, sheet, from).await?;
    let c = ctx.client.create_copy(&target.line, name).await?;
    ctx.emit(&c, || {
        format!(
            "Made the copy \"{}\" ({}) of {} at version {}. Work in it with --copy {}, for example calc read {} --copy {}.",
            c.name, c.id, target.place(), c.forked_at, c.id, target.sheet.id, c.id
        )
    });
    Ok(())
}

pub async fn rename(ctx: &Ctx, sheet: &str, copy: &str, name: &str) -> CliResult<()> {
    let s = resolve::sheet(ctx, sheet, Look::Visible).await?;
    let c = resolve::copy(ctx, &s, copy).await?;
    let renamed = ctx.client.rename_copy(&s.id, &c.id, name.trim()).await?;
    let value = renamed.map(|r| serde_json::to_value(r).unwrap_or_default()).unwrap_or_else(|| json!({"id": c.id, "name": name.trim()}));
    ctx.emit(&value, || format!("Renamed the copy \"{}\" to \"{}\".", c.name, name.trim()));
    Ok(())
}

pub async fn rm(ctx: &Ctx, sheet: &str, copy: &str) -> CliResult<()> {
    let s = resolve::sheet(ctx, sheet, Look::Visible).await?;
    let c = resolve::copy(ctx, &s, copy).await?;
    ctx.client.delete_copy(&s.id, &c.id).await?;
    ctx.emit(&json!({"deleted": true, "id": c.id, "name": c.name}), || {
        format!("Deleted the copy \"{}\" ({}). Its history stays readable: calc versions ls {} --copy {}.", c.name, c.id, s.id, c.id)
    });
    Ok(())
}

fn side_word(side: Side) -> &'static str {
    match side {
        Side::Copy => "the copy's",
        Side::Sheet => "the sheet's",
    }
}

pub fn render_conflicts(conflicts: &[Conflict], verb: &str) -> String {
    let mut out = format!("{} (changed both in the copy and where it came from):\n", plural(conflicts.len() as u64, "conflict", "conflicts"));
    for c in conflicts {
        out.push_str(&format!(
            "  {:<12} was {}; sheet has {}; copy has {} → {verb} {}\n",
            a1::with_tab(&c.tab_name, &c.a),
            c.base,
            c.sheet,
            c.copy,
            side_word(c.taken)
        ));
    }
    out
}

pub async fn compare(ctx: &Ctx, sheet: &str, copy: &str, limit: Option<u32>) -> CliResult<()> {
    let s = resolve::sheet(ctx, sheet, Look::Visible).await?;
    let c = resolve::copy(ctx, &s, copy).await?;
    let answer = ctx.client.compare_copy(&s.id, &c.id, limit).await?;
    ctx.emit(&answer, || {
        let mut out = format!("The copy \"{}\" against {} as it was when the copy was made (version {}):\n", c.name, parent_name(&s, &c), c.forked_at);
        if answer.diff.is_empty() {
            out.push_str("Nothing changed in the copy.\n");
        } else {
            out.push_str(&render_diff(&answer.diff));
        }
        if answer.parent_changed {
            out.push_str(&format!("{} changed too since then.\n", capitalise(&parent_name(&s, &c))));
        }
        if !answer.conflicts.is_empty() {
            out.push('\n');
            out.push_str(&render_conflicts(&answer.conflicts, "merging takes"));
            out.push_str("Choose per cell with calc copies merge … --resolve Sales!B2=sheet, or for all with --take sheet.\n");
        }
        out
    });
    Ok(())
}

fn parent_name(s: &Sheet, c: &SheetCopy) -> String {
    match &c.parent_id {
        Some(p) => format!("the copy {p}"),
        None => format!("\"{}\"", s.title),
    }
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub struct MergeArgs {
    pub sheet: String,
    pub copy: String,
    pub take: Option<String>,
    pub tabs: Vec<String>,
    pub range: Option<String>,
    pub resolve: Vec<String>,
    pub dry_run: bool,
    pub close: bool,
}

pub fn parse_side(s: &str) -> CliResult<Side> {
    Side::parse(s).ok_or_else(|| CliError::usage(format!("\"{s}\" is neither copy nor sheet."), "Take copy (what the copy has) or sheet (what it came from has)."))
}

/// `B2=sheet` or `Sales!B2=copy`.
pub fn parse_resolution(s: &str) -> CliResult<(Option<String>, String, Side)> {
    let Some((place, side)) = s.rsplit_once('=') else {
        return Err(CliError::usage(format!("\"{s}\" is not CELL=SIDE."), "Write resolutions like B2=sheet or 'Sales'!B2=copy."));
    };
    let side = parse_side(side)?;
    let (tab, cell) = a1::split_tab(place);
    let r = RangeRef::parse(cell).filter(RangeRef::is_cell).ok_or_else(|| {
        CliError::usage(format!("\"{cell}\" is not one cell."), "A resolution names one cell, like B2=sheet.")
    })?;
    Ok((tab, r.start().to_string(), side))
}

pub async fn merge(ctx: &Ctx, a: MergeArgs) -> CliResult<()> {
    let default = a.take.as_deref().map(parse_side).transpose()?;
    let resolutions_in = a.resolve.iter().map(|r| parse_resolution(r)).collect::<CliResult<Vec<_>>>()?;
    let s = resolve::sheet(ctx, &a.sheet, Look::Visible).await?;
    let c = resolve::copy(ctx, &s, &a.copy).await?;
    if c.status != CopyStatus::Open {
        return Err(CliError::new(
            "conflict",
            format!("The copy \"{}\" is {}, so it can't be merged.", c.name, c.status.as_str()),
            format!("calc copies ls {} --all shows every copy and where it stands.", s.id),
        ));
    }
    let copy_target = resolve::target_of(ctx, s.clone(), Some(&c.id)).await?;
    let tabs = if a.tabs.is_empty() {
        None
    } else {
        Some(a.tabs.iter().map(|t| copy_target.tab(Some(t)).map(|t| TabRef::Id(t.id))).collect::<CliResult<Vec<_>>>()?)
    };
    let range = match &a.range {
        Some(r) => {
            let (tab, rest) = a1::split_tab(r);
            let rr = RangeRef::parse(rest).ok_or_else(|| CliError::usage(format!("\"{r}\" is not a range."), "Write it like 'Sales'!A1:D50."))?;
            let only_tab = (tabs.as_ref().map(Vec::len) == Some(1)).then(|| a.tabs[0].clone());
            let t = copy_target.tab(tab.as_deref().or(only_tab.as_deref()))?;
            Some(MergeRange { tab: TabRef::Id(t.id), range: if rr.kind == RangeKind::Cells { rr.to_string() } else { rest.trim().to_owned() } })
        }
        None => None,
    };
    let fallback_tab = range.as_ref().map(|r| r.tab.clone()).or_else(|| tabs.as_ref().filter(|t| t.len() == 1).map(|t| t[0].clone()));
    let mut resolutions = Vec::new();
    for (tab, cell, side) in resolutions_in {
        let t = match (&tab, &fallback_tab) {
            (Some(name), _) => TabRef::Id(copy_target.tab(Some(name))?.id),
            (None, Some(t)) => t.clone(),
            (None, None) => TabRef::Id(copy_target.tab(None)?.id),
        };
        resolutions.push(Resolution { tab: t, a: cell, take: side });
    }
    let request = MergeRequest { tabs, range, resolutions, default, dry_run: a.dry_run, close: a.close };
    let answer = ctx.client.merge_copy(&s.id, &c.id, &request).await?;
    ctx.emit(&answer, || {
        let into = parent_name(&s, &c);
        let mut out = if a.dry_run {
            format!("Merging \"{}\" into {into} would take {} from the copy", c.name, plural(answer.cells_taken, "cell", "cells"))
        } else {
            format!("Merged \"{}\" into {into}: took {} from the copy", c.name, plural(answer.cells_taken, "cell", "cells"))
        };
        let mut extra = Vec::new();
        if answer.tabs_added.count() > 0 {
            extra.push(format!("{} added", plural(answer.tabs_added.count(), "tab", "tabs")));
        }
        if answer.tabs_removed.count() > 0 {
            extra.push(format!("{} removed", plural(answer.tabs_removed.count(), "tab", "tabs")));
        }
        if answer.rows_added > 0 {
            extra.push(format!("{} added", plural(answer.rows_added, "row", "rows")));
        }
        if answer.rows_removed > 0 {
            extra.push(format!("{} removed", plural(answer.rows_removed, "row", "rows")));
        }
        if !extra.is_empty() {
            out.push_str(&format!(", {}", extra.join(", ")));
        }
        match (a.dry_run, answer.version) {
            (true, _) => out.push_str(". Nothing was changed (--dry-run).\n"),
            (false, Some(v)) => out.push_str(&format!(" (version {v}).\n")),
            (false, None) => out.push_str(".\n"),
        }
        if !answer.conflicts.is_empty() {
            out.push_str(&render_conflicts(&answer.conflicts, if a.dry_run { "would take" } else { "took" }));
        }
        if !a.dry_run && !a.close {
            out.push_str(&format!("The copy stays open; calc copies rm {} {} deletes it when it isn't needed.\n", s.id, c.id));
        }
        out
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use silicon_calc_client::Account;

    fn copy(id: &str, parent: Option<&str>, at: &str) -> SheetCopy {
        SheetCopy {
            id: id.into(),
            name: format!("idea {id}"),
            parent_id: parent.map(str::to_owned),
            forked_at: 4,
            version: 2,
            created_by: Account { id: "si:analyst".into(), ..Default::default() },
            created_at: at.into(),
            ..Default::default()
        }
    }

    #[test]
    fn copies_draw_as_a_tree() {
        let sheet = Sheet { title: "Q3".into(), version: 9, ..Default::default() };
        let copies = vec![
            copy("c2", None, "2026-10-17T02:00:00Z"),
            copy("c1", None, "2026-10-17T01:00:00Z"),
            copy("c3", Some("c1"), "2026-10-17T03:00:00Z"),
        ];
        let out = render_tree(&sheet, &copies);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "\"Q3\"  main sheet · version 9");
        assert!(lines[1].starts_with("├─ c1  \"idea c1\"  open · version 2 · made from version 4 by si:analyst"), "{out}");
        assert!(lines[2].starts_with("│  └─ c3  "), "{out}");
        assert!(lines[3].starts_with("└─ c2  "), "{out}");
    }

    #[test]
    fn resolutions() {
        assert_eq!(parse_resolution("B2=sheet").unwrap(), (None, "B2".into(), Side::Sheet));
        assert_eq!(parse_resolution("'By region'!$c$3=copy").unwrap(), (Some("By region".into()), "C3".into(), Side::Copy));
        assert!(parse_resolution("B2").is_err());
        assert!(parse_resolution("B2:C3=copy").is_err());
        assert!(parse_resolution("B2=mine").is_err());
        assert_eq!(parse_side("SHEET").unwrap(), Side::Sheet);
    }
}
