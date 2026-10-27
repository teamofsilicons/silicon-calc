//! Tabs (list, show, add, rename, move, delete, colour, import a file into), defined names, charts
//! and protected ranges.

use serde_json::json;
use silicon_calc_client::a1::{self, RangeKind, RangeRef};
use silicon_calc_client::{
    ChartSpec, Edit, ImportMode, NameDef, ProtectionSpec, Tab, TabList, TabRef,
};

use crate::cmd_cells::{parse_color, range_or_tab, send};
use crate::cmd_library::{render_tabs, upload_from};
use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};
use crate::output::{Table, when_ago};
use crate::resolve::{self, Target};

/// A 1-based position (as `calc tabs ls` numbers tabs) as the zero-based index requests take.
pub fn position(pos: usize, what: &str) -> CliResult<usize> {
    if pos == 0 {
        return Err(CliError::usage(
            format!("{what} 0 doesn't exist; tabs are numbered from 1."),
            "calc tabs ls SHEET shows their numbers.",
        ));
    }
    Ok(pos - 1)
}

pub async fn ls(ctx: &Ctx, sheet: &str, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let list = TabList {
        version: target.version,
        tabs: target.tabs.clone(),
        names: None,
    };
    ctx.emit(&list, || {
        format!(
            "{} · version {}\n{}",
            target.place(),
            target.version,
            render_tabs(&target.tabs)
        )
    });
    Ok(())
}

pub fn render_tab(tab: &Tab) -> String {
    let mut out = format!(
        "{} (tab id {}, number {})\n",
        tab.name,
        tab.id,
        tab.index + 1
    );
    let mut row = |k: &str, v: String| out.push_str(&format!("{k:<13}{v}\n"));
    row(
        "used",
        tab.used.clone().unwrap_or_else(|| "nothing yet".into()),
    );
    row("grid", format!("{} rows × {} columns", tab.rows, tab.cols));
    if tab.frozen_rows > 0 || tab.frozen_cols > 0 {
        row(
            "frozen",
            format!("{} rows, {} columns", tab.frozen_rows, tab.frozen_cols),
        );
    }
    if let Some(c) = &tab.color {
        row("colour", c.clone());
    }
    if let Some(code) = &tab.code {
        row(
            "made by",
            format!(
                "{} code run by {}, {}",
                code.language.as_str(),
                code.run_by.label(),
                when_ago(&code.run_at)
            ),
        );
        row("execution", "JavaScript in the browser".into());
        if !code.inputs.is_empty() {
            row(
                "reads tabs",
                code.inputs
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        out.push_str("code\n");
        for line in code.code.lines() {
            out.push_str(&format!("  {line}\n"));
        }
    }
    out
}

pub async fn show(ctx: &Ctx, sheet: &str, copy: Option<&str>, tab: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    ctx.emit(&t, || {
        let mut out = render_tab(&t);
        if let Some(code) = &t.code
            && !code.inputs.is_empty()
        {
            let names: Vec<String> = code
                .inputs
                .iter()
                .map(|id| {
                    target
                        .tabs
                        .iter()
                        .find(|x| x.id == *id)
                        .map(|x| format!("{} ({id})", x.name))
                        .unwrap_or_else(|| id.to_string())
                })
                .collect();
            out = out.replace(
                &format!(
                    "reads tabs   {}\n",
                    code.inputs
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                &format!("reads tabs   {}\n", names.join(", ")),
            );
        }
        out
    });
    Ok(())
}

/// The tabs again, after a change to them.
async fn tabs_now(ctx: &Ctx, target: &Target) -> CliResult<TabList> {
    Ok(ctx.client.tabs(&target.line).await?)
}

pub async fn add(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    name: &str,
    at: Option<usize>,
) -> CliResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CliError::usage(
            "The tab's name is empty.",
            "Name it, for example: calc tabs add SHEET Notes.",
        ));
    }
    let index = at.map(|p| position(p, "Position")).transpose()?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let answer = send(
        ctx,
        &target,
        vec![Edit::AddTab {
            name: name.to_owned(),
            index,
        }],
        None,
    )
    .await?;
    let list = tabs_now(ctx, &target).await?;
    let tab = answer
        .added_tab()
        .and_then(|id| list.tabs.iter().find(|t| t.id == id))
        .or_else(|| list.tabs.iter().find(|t| t.name == name))
        .cloned();
    ctx.emit(
        &json!({"version": answer.version, "tab": tab, "tabs": list.tabs}),
        || match &tab {
            Some(t) => format!(
                "Added the tab \"{}\" (id {}, number {}) to {} (version {}).",
                t.name,
                t.id,
                t.index + 1,
                target.place(),
                answer.version
            ),
            None => format!(
                "Added the tab \"{name}\" to {} (version {}).",
                target.place(),
                answer.version
            ),
        },
    );
    Ok(())
}

pub async fn rename(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    tab: &str,
    name: &str,
) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    let answer = send(
        ctx,
        &target,
        vec![Edit::RenameTab {
            tab: TabRef::Id(t.id),
            name: name.trim().to_owned(),
        }],
        None,
    )
    .await?;
    ctx.emit(&answer, || {
        format!(
            "Renamed the tab \"{}\" to \"{}\" (version {}).",
            t.name,
            name.trim(),
            answer.version
        )
    });
    ctx.note("Formulas that pointed at the old name now use the new one.");
    Ok(())
}

pub async fn move_tab(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    tab: &str,
    to: usize,
) -> CliResult<()> {
    let index = position(to, "Position")?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    let answer = send(
        ctx,
        &target,
        vec![Edit::MoveTab {
            tab: TabRef::Id(t.id),
            index,
        }],
        None,
    )
    .await?;
    ctx.emit(&answer, || {
        format!(
            "Moved the tab \"{}\" to number {to} (version {}).",
            t.name, answer.version
        )
    });
    Ok(())
}

pub async fn rm(ctx: &Ctx, sheet: &str, copy: Option<&str>, tab: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    if target.tabs.len() == 1 {
        return Err(CliError::new(
            "last_tab",
            format!(
                "\"{}\" is the only tab of {}; a sheet needs at least one tab.",
                t.name,
                target.place()
            ),
            "Add another tab first (calc tabs add), or clear this one (calc clear SHEET A:XFD --all).",
        ));
    }
    let answer = send(
        ctx,
        &target,
        vec![Edit::DeleteTab {
            tab: TabRef::Id(t.id),
        }],
        None,
    )
    .await?;
    ctx.emit(&answer, || {
        format!(
            "Deleted the tab \"{}\" (version {}). It stays in the history; calc undo {}{} brings it back.",
            t.name,
            answer.version,
            target.sheet.id,
            target.copy_flag()
        )
    });
    Ok(())
}

pub async fn color(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    tab: &str,
    color: &str,
) -> CliResult<()> {
    let c = parse_color(color)?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let t = target.tab(Some(tab))?.clone();
    let edit = Edit::TabProps {
        tab: TabRef::Id(t.id),
        frozen_rows: None,
        frozen_cols: None,
        color: Some(c.clone()),
    };
    let answer = send(ctx, &target, vec![edit], None).await?;
    ctx.emit(&answer, || match &c {
        Some(c) => format!(
            "The tab \"{}\" is {c} now (version {}).",
            t.name, answer.version
        ),
        None => format!(
            "The tab \"{}\" has no colour now (version {}).",
            t.name, answer.version
        ),
    });
    Ok(())
}

pub async fn import(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    file: &str,
    replace: Option<&str>,
    format: Option<&str>,
) -> CliResult<()> {
    let upload = upload_from(file, format)?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let mode = match replace {
        Some(tab) => ImportMode::Replace(TabRef::Id(target.tab(Some(tab))?.id)),
        None => ImportMode::NewTab,
    };
    let answer = ctx.client.import_tabs(&target.line, &upload, &mode).await?;
    ctx.emit(&answer, || {
        let names: Vec<String> = answer
            .tabs
            .iter()
            .map(|t| format!("\"{}\"", t.name))
            .collect();
        match replace {
            Some(tab) => format!(
                "Replaced the contents of \"{tab}\" with {} (version {}).",
                upload.file_name, answer.version
            ),
            None => format!(
                "Opened {} into {} as {} (version {}).",
                upload.file_name,
                target.place(),
                names.join(", "),
                answer.version
            ),
        }
    });
    Ok(())
}

// ── Names ───────────────────────────────────────────────────────────────────────────────────────────────

/// `Revenue=Sales!$B$2:$B$100` as a name and what it refers to.
pub fn parse_name_def(s: &str) -> CliResult<NameDef> {
    let Some((name, reference)) = s.split_once('=') else {
        return Err(CliError::usage(
            format!("\"{s}\" is not NAME=REFERENCE."),
            "Write names like Revenue='Sales'!$B$2:$B$100.",
        ));
    };
    let name = name.trim();
    let reference = reference.trim().trim_start_matches('=');
    check_name(name)?;
    if reference.is_empty() {
        return Err(CliError::usage(
            format!("The name {name} refers to nothing."),
            "Write what it refers to after =, like Revenue=Sales!$B$2:$B$100.",
        ));
    }
    Ok(NameDef {
        name: name.to_owned(),
        reference: reference.to_owned(),
    })
}

fn check_name(name: &str) -> CliResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '\\'))
        && a1::CellRef::parse(name).is_none()
        && !matches!(
            name.to_ascii_uppercase().as_str(),
            "R" | "C" | "TRUE" | "FALSE"
        );
    if ok {
        Ok(())
    } else {
        Err(CliError::usage(
            format!("\"{name}\" can't be a name."),
            "Names start with a letter or _, hold letters, digits, _ and ., and don't look like a cell (A1) or TRUE/FALSE.",
        ))
    }
}

async fn names_of(ctx: &Ctx, target: &Target) -> CliResult<Vec<NameDef>> {
    let list = ctx.client.tabs(&target.line).await?;
    list.names.ok_or_else(|| {
        CliError::new(
            "not_supported",
            "This Silicon Calc service doesn't list a sheet's defined names, so they can't be read or changed one by one.",
            "Replace them all at once with calc names set SHEET NAME=REF … (it sets exactly the names given).",
        )
    })
}

fn render_names(names: &[NameDef]) -> String {
    if names.is_empty() {
        return "No defined names.".into();
    }
    let mut t = Table::new(&["name", "refers to"]);
    for n in names {
        t.row(vec![n.name.clone(), n.reference.clone()]);
    }
    t.render()
}

pub async fn names_ls(ctx: &Ctx, sheet: &str, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let names = names_of(ctx, &target).await?;
    ctx.emit(&json!({"names": names}), || render_names(&names));
    Ok(())
}

async fn put_names(ctx: &Ctx, target: &Target, names: Vec<NameDef>, said: String) -> CliResult<()> {
    let answer = send(
        ctx,
        target,
        vec![Edit::Names {
            names: names.clone(),
        }],
        None,
    )
    .await?;
    ctx.emit(&json!({"version": answer.version, "names": names}), || {
        format!("{said} (version {}).", answer.version)
    });
    Ok(())
}

pub async fn names_set(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    defs: &[String],
    clear: bool,
) -> CliResult<()> {
    if defs.is_empty() && !clear {
        return Err(CliError::usage(
            "No names given.",
            "Give NAME=REF pairs, or --clear to remove every name.",
        ));
    }
    let names = defs
        .iter()
        .map(|d| parse_name_def(d))
        .collect::<CliResult<Vec<_>>>()?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let said = if names.is_empty() {
        "Removed every defined name".to_owned()
    } else {
        format!(
            "{} now has exactly these names: {}",
            target.place(),
            names
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    put_names(ctx, &target, names, said).await
}

pub async fn names_add(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    name: &str,
    reference: &str,
) -> CliResult<()> {
    let def = parse_name_def(&format!("{name}={reference}"))?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let mut names = names_of(ctx, &target).await?;
    let said = match names
        .iter_mut()
        .find(|n| n.name.eq_ignore_ascii_case(&def.name))
    {
        Some(existing) => {
            existing.reference = def.reference.clone();
            format!("{} now refers to {}", def.name, def.reference)
        }
        None => {
            names.push(def.clone());
            format!("Defined {} as {}", def.name, def.reference)
        }
    };
    put_names(ctx, &target, names, said).await
}

pub async fn names_rm(ctx: &Ctx, sheet: &str, copy: Option<&str>, name: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let mut names = names_of(ctx, &target).await?;
    let before = names.len();
    names.retain(|n| !n.name.eq_ignore_ascii_case(name.trim()));
    if names.len() == before {
        return Err(CliError::not_found(
            format!("{} has no name {}.", target.place(), name.trim()),
            "calc names ls SHEET lists them.",
        ));
    }
    let said = format!(
        "Removed the name {}; formulas using it now show #NAME?",
        name.trim()
    );
    put_names(ctx, &target, names, said).await
}

// ── Charts ──────────────────────────────────────────────────────────────────────────────────────────────

pub const CHART_KINDS: [&str; 6] = ["line", "bar", "column", "area", "pie", "scatter"];

/// The charts and protections of each tab named (every tab when none is).
async fn tab_extras(
    ctx: &Ctx,
    target: &Target,
    tab: Option<&str>,
) -> CliResult<Vec<(Tab, silicon_calc_client::Cells)>> {
    let tabs: Vec<Tab> = match tab {
        Some(t) => vec![target.tab(Some(t))?.clone()],
        None => {
            let mut all = target.tabs.clone();
            all.sort_by_key(|t| t.index);
            all
        }
    };
    let mut out = Vec::new();
    for t in tabs {
        let cells = ctx
            .client
            .cells(&target.line, &TabRef::Id(t.id), Some("A1"), None)
            .await?;
        out.push((t, cells));
    }
    Ok(out)
}

pub async fn chart_ls(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    tab: Option<&str>,
) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let extras = tab_extras(ctx, &target, tab).await?;
    let mut charts = Vec::new();
    for (t, cells) in &extras {
        for c in &cells.charts {
            let mut v = serde_json::to_value(c).unwrap_or_default();
            v["tab"] = json!(t.id);
            v["tab_name"] = json!(t.name);
            charts.push((t.name.clone(), c.clone(), v));
        }
    }
    let values: Vec<serde_json::Value> = charts.iter().map(|c| c.2.clone()).collect();
    ctx.emit(&json!({"charts": values}), || {
        if charts.is_empty() {
            return format!(
                "{} has no charts. Add one with calc chart add SHEET A1:C20 --kind line.",
                target.place()
            );
        }
        let mut t = Table::new(&["id", "tab", "kind", "title", "data", "at", "size"]);
        for (tab, c, _) in &charts {
            t.row(vec![
                c.id.clone(),
                tab.clone(),
                c.kind.clone(),
                c.title.clone(),
                c.range.clone(),
                c.anchor_a1(),
                format!("{}×{}", c.width, c.height),
            ]);
        }
        t.render()
    });
    Ok(())
}

pub struct ChartArgs {
    pub sheet: String,
    pub copy: Option<String>,
    pub range: String,
    pub tab: Option<String>,
    pub kind: String,
    pub title: Option<String>,
    pub at: Option<String>,
    pub width: u32,
    pub height: u32,
    pub id: Option<String>,
}

pub async fn chart_add(ctx: &Ctx, a: ChartArgs) -> CliResult<()> {
    let kind = a.kind.trim().to_ascii_lowercase();
    if !CHART_KINDS.contains(&kind.as_str()) {
        return Err(CliError::usage(
            format!("\"{}\" is not a kind of chart.", a.kind),
            format!("Use one of {}.", CHART_KINDS.join(", ")),
        ));
    }
    if !(50..=4000).contains(&a.width) || !(50..=4000).contains(&a.height) {
        return Err(CliError::usage(
            "A chart is 50 to 4000 pixels wide and high.",
            "Pass --width and --height in that span.",
        ));
    }
    let target = resolve::target(ctx, &a.sheet, a.copy.as_deref()).await?;
    let (tab_arg, range) = range_or_tab(&target, Some(&a.range), a.tab.as_deref())?;
    let range = range.ok_or_else(|| {
        CliError::usage(
            "Which cells does the chart show?",
            "Name the data range, like A1:C20.",
        )
    })?;
    let data = RangeRef::parse(&range).ok_or_else(|| {
        CliError::usage(
            format!("\"{range}\" is not a range."),
            "Write it like A1:C20.",
        )
    })?;
    let t = target.tab(tab_arg.as_deref())?.clone();
    let anchor = match &a.at {
        Some(at) => {
            let r = RangeRef::parse(at)
                .filter(|r| r.kind == RangeKind::Cells)
                .ok_or_else(|| {
                    CliError::usage(
                        format!("\"{at}\" is not a cell to put the chart at."),
                        "Name its top-left cell, like E2.",
                    )
                })?;
            r.start().to_string()
        }
        // To the right of the data, at its top.
        None => a1::CellRef::new(
            if data.kind == RangeKind::Cells {
                data.r1
            } else {
                0
            },
            (data.c2 + 2).min(a1::MAX_COLS - 1),
        )
        .to_string(),
    };
    let title = a.title.clone().unwrap_or_default();
    let spec = ChartSpec {
        id: a.id.clone(),
        tab: TabRef::Id(t.id),
        kind: kind.clone(),
        title: title.clone(),
        range: range.clone(),
        anchor: anchor.clone(),
        width: a.width,
        height: a.height,
    };
    let answer = send(ctx, &target, vec![Edit::Chart { chart: spec }], None).await?;
    ctx.emit(&answer, || {
        format!(
            "Added a {kind} chart of {} at {anchor} (version {}).",
            a1::with_tab(&t.name, &range),
            answer.version
        )
    });
    Ok(())
}

pub async fn chart_rm(ctx: &Ctx, sheet: &str, copy: Option<&str>, id: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let answer = send(
        ctx,
        &target,
        vec![Edit::DeleteChart {
            id: id.trim().to_owned(),
        }],
        None,
    )
    .await?;
    ctx.emit(&answer, || {
        format!(
            "Deleted the chart {} (version {}).",
            id.trim(),
            answer.version
        )
    });
    Ok(())
}

// ── Protections ─────────────────────────────────────────────────────────────────────────────────────────

pub async fn protections_ls(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    tab: Option<&str>,
) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let extras = tab_extras(ctx, &target, tab).await?;
    let mut rows = Vec::new();
    for (t, cells) in &extras {
        for p in &cells.protections {
            let editors: Vec<String> = p.editors.iter().map(|e| e.label(&cells.accounts)).collect();
            let mut v = serde_json::to_value(p).unwrap_or_default();
            v["tab"] = json!(t.id);
            v["tab_name"] = json!(t.name);
            rows.push((t.name.clone(), p.clone(), editors, v));
        }
    }
    let values: Vec<serde_json::Value> = rows.iter().map(|r| r.3.clone()).collect();
    ctx.emit(&json!({"protections": values}), || {
        if rows.is_empty() {
            return format!("Nothing in {} is protected.", target.place());
        }
        let mut t = Table::new(&["id", "where", "who may change it", "what for"]);
        for (tab, p, editors, _) in &rows {
            let place = match &p.range {
                Some(r) => a1::with_tab(tab, r),
                None => format!("all of {}", a1::quote_tab(tab)),
            };
            let who = if editors.is_empty() {
                "the owner".to_owned()
            } else {
                format!("the owner, {}", editors.join(", "))
            };
            t.row(vec![p.id.clone(), place, who, p.description.clone()]);
        }
        t.render()
    });
    Ok(())
}

pub async fn protect(
    ctx: &Ctx,
    sheet: &str,
    copy: Option<&str>,
    range: Option<&str>,
    tab: Option<&str>,
    editors: &[String],
    description: Option<String>,
) -> CliResult<()> {
    let editors = editors
        .iter()
        .flat_map(|e| e.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(resolve::account_id)
        .collect::<CliResult<Vec<_>>>()?;
    let target = resolve::target(ctx, sheet, copy).await?;
    let (tab_arg, range) = range_or_tab(&target, range, tab)?;
    let t = target.tab(tab_arg.as_deref())?.clone();
    let spec = ProtectionSpec {
        id: None,
        tab: TabRef::Id(t.id),
        range: range.clone(),
        editors: editors.clone(),
        description: description.unwrap_or_default(),
    };
    let answer = send(ctx, &target, vec![Edit::Protect { protection: spec }], None).await?;
    ctx.emit(&answer, || {
        let place = match &range {
            Some(r) => a1::with_tab(&t.name, r),
            None => format!("the whole tab {}", a1::quote_tab(&t.name)),
        };
        let who = if editors.is_empty() {
            "only the owner".to_owned()
        } else {
            format!("only the owner and {}", editors.join(", "))
        };
        format!(
            "Protected {place}: {who} may change it now (version {}).",
            answer.version
        )
    });
    Ok(())
}

pub async fn unprotect(ctx: &Ctx, sheet: &str, copy: Option<&str>, id: &str) -> CliResult<()> {
    let target = resolve::target(ctx, sheet, copy).await?;
    let answer = send(
        ctx,
        &target,
        vec![Edit::Unprotect {
            id: id.trim().to_owned(),
        }],
        None,
    )
    .await?;
    ctx.emit(&answer, || {
        format!(
            "Removed the protection {}; every editor may change those cells again (version {}).",
            id.trim(),
            answer.version
        )
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_from_one() {
        assert_eq!(position(1, "Position").unwrap(), 0);
        assert_eq!(position(3, "Position").unwrap(), 2);
        assert_eq!(position(0, "Position").unwrap_err().exit, 2);
    }

    #[test]
    fn name_definitions() {
        let n = parse_name_def("Revenue=Sales!$B$2:$B$100").unwrap();
        assert_eq!(
            (n.name.as_str(), n.reference.as_str()),
            ("Revenue", "Sales!$B$2:$B$100")
        );
        let n = parse_name_def("tax_rate = =0.2").unwrap();
        assert_eq!(n.reference, "0.2");
        assert!(parse_name_def("A1=Sales!B2").is_err());
        assert!(parse_name_def("1st=B2").is_err());
        assert!(parse_name_def("Revenue").is_err());
        assert!(parse_name_def("Revenue=").is_err());
        assert!(parse_name_def("TRUE=1").is_err());
    }

    #[test]
    fn tabs_describe_their_code() {
        let tab = Tab {
            id: 3,
            name: "By region".into(),
            index: 1,
            rows: 1000,
            cols: 26,
            used: Some("A1:B5".into()),
            code: Some(silicon_calc_client::TabCode {
                language: "javascript".into(),
                code: "const rows = await calc.tab(\"Sales\");\nreturn rows;".into(),
                inputs: vec![1],
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = render_tab(&tab);
        assert!(out.starts_with("By region (tab id 3, number 2)\n"), "{out}");
        assert!(out.contains("JavaScript in the browser"), "{out}");
        assert!(out.contains("  return rows;\n"), "{out}");
    }
}
