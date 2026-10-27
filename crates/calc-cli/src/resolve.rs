//! Turning what people type into ids: a sheet by its id, its link or its exact title; a copy by id or name; a
//! folder by id, path or name; a tab by name or id; a version by number or name.

use silicon_calc_client::{Folder, Line, ListSheets, Scope, Sheet, SheetCopy, Tab, a1};

use crate::ctx::Ctx;
use crate::error::{CliError, CliResult};

/// Sheet, folder, copy and transfer ids: 12 characters of `[a-z0-9]`.
pub fn looks_like_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The sheet id in a web link (`https://calc.teamofsilicons.com/s/k3j9xq2mpa7d…`).
pub fn id_from_link(s: &str) -> Option<String> {
    if !s.contains("://") {
        return None;
    }
    let url = url::Url::parse(s.trim()).ok()?;
    let segments: Vec<String> = url.path_segments()?.map(str::to_owned).collect();
    segments.into_iter().find(|seg| looks_like_id(seg))
}

/// Where to look for a sheet named by title.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Sheets the caller can see (theirs and shared with them).
    Visible,
    /// The caller's trash.
    Trash,
}

fn describe(s: &Sheet) -> String {
    let owner = s.owner.label();
    format!("{}  {}  (owner {})", s.id, s.title, if owner.is_empty() { "?" } else { owner })
}

/// A sheet by id, link or exact title.
pub async fn sheet(ctx: &Ctx, arg: &str, look: Look) -> CliResult<Sheet> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err(CliError::usage("The sheet is empty.", "Name a sheet by its id, its link or its exact title."));
    }
    let id = id_from_link(arg).or_else(|| looks_like_id(arg).then(|| arg.to_owned()));
    if let Some(id) = &id {
        match ctx.client.sheet(id).await {
            Ok(s) => return Ok(s),
            Err(e) if e.is_not_found() && !arg.contains("://") => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut found: Vec<Sheet> = Vec::new();
    let mut add = |list: Vec<Sheet>| {
        for s in list {
            if !found.iter().any(|f| f.id == s.id) {
                found.push(s);
            }
        }
    };
    let list = |scope: Scope| ListSheets { scope, q: Some(arg.to_owned()), limit: Some(100), ..Default::default() };
    match look {
        Look::Trash => add(ctx.client.list_all_sheets(&list(Scope::Trash), 1000).await?.sheets),
        Look::Visible => {
            add(ctx.client.list_all_sheets(&list(Scope::Mine), 1000).await?.sheets);
            add(ctx.client.list_all_sheets(&list(Scope::Shared), 1000).await?.sheets);
            // Search reaches sheets inside folders too; a service without it is fine.
            match ctx.client.search(arg, Some(100)).await {
                Ok(results) => add(results.into_iter().map(|r| r.sheet).collect()),
                Err(e) if e.is_signed_out() => return Err(e.into()),
                Err(_) => {}
            }
        }
    }
    let exact: Vec<&Sheet> = found.iter().filter(|s| s.title == arg).collect();
    let chosen = match exact.len() {
        1 => exact[0].clone(),
        0 => {
            let lower = arg.to_lowercase();
            let loose: Vec<&Sheet> = found.iter().filter(|s| s.title.to_lowercase() == lower).collect();
            match loose.len() {
                1 => loose[0].clone(),
                0 => {
                    let (what, hint) = match look {
                        Look::Trash => ("in your trash", "calc trash lists it; or pass the sheet's id."),
                        Look::Visible => (
                            "you can see",
                            "calc ls lists your sheets, calc ls --shared the ones shared with you, and calc search finds titles and cells; or pass the sheet's id.",
                        ),
                    };
                    return Err(CliError::not_found(format!("No sheet {what} is titled \"{arg}\"."), hint));
                }
                _ => return Err(ambiguous(arg, &loose)),
            }
        }
        _ => return Err(ambiguous(arg, &exact)),
    };
    // The listing has no tabs; the sheet itself does.
    Ok(ctx.client.sheet(&chosen.id).await?)
}

fn ambiguous(arg: &str, matches: &[&Sheet]) -> CliError {
    let list: Vec<String> = matches.iter().map(|s| format!("  {}", describe(s))).collect();
    CliError::usage_code(
        "ambiguous",
        format!("{} sheets are titled \"{arg}\":\n{}", matches.len(), list.join("\n")),
        "Pass the id of the one you mean.",
    )
    .with_details(serde_json::json!({"matches": matches.iter().map(|s| serde_json::json!({"id": s.id, "title": s.title, "owner": s.owner.id})).collect::<Vec<_>>()}))
}

/// A sheet, the line a command acts on (the main sheet or a copy), its tabs and its version.
pub struct Target {
    pub sheet: Sheet,
    pub line: Line,
    pub copy: Option<SheetCopy>,
    pub tabs: Vec<Tab>,
    pub version: i64,
}

impl Target {
    /// `"Q3 revenue"`, or `"Q3 revenue" (copy "Try weekly")`.
    pub fn place(&self) -> String {
        match &self.copy {
            Some(c) => format!("\"{}\" (copy \"{}\")", self.sheet.title, c.name),
            None => format!("\"{}\"", self.sheet.title),
        }
    }

    /// The `--copy` flag that names this line again, for suggested commands.
    pub fn copy_flag(&self) -> String {
        match &self.copy {
            Some(c) => format!(" --copy {}", c.id),
            None => String::new(),
        }
    }

    /// A tab by name or id (none: the first tab).
    pub fn tab(&self, name: Option<&str>) -> CliResult<&Tab> {
        find_tab(&self.tabs, name, &self.place())
    }
}

/// A tab in `tabs` by name (exact, then any case) or id; none is the first tab.
pub fn find_tab<'a>(tabs: &'a [Tab], name: Option<&str>, place: &str) -> CliResult<&'a Tab> {
    let mut sorted: Vec<&Tab> = tabs.iter().collect();
    sorted.sort_by_key(|t| t.index);
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return sorted.first().copied().ok_or_else(|| CliError::not_found(format!("{place} has no tabs."), "Add one with calc tabs add."));
    };
    if let Some(t) = sorted.iter().find(|t| t.name == name) {
        return Ok(t);
    }
    if let Ok(id) = name.parse::<u32>()
        && let Some(t) = sorted.iter().find(|t| t.id == id)
    {
        return Ok(t);
    }
    let lower = name.to_lowercase();
    let loose: Vec<&&Tab> = sorted.iter().filter(|t| t.name.to_lowercase() == lower).collect();
    if loose.len() == 1 {
        return Ok(loose[0]);
    }
    let names: Vec<String> = sorted.iter().map(|t| format!("\"{}\" (id {})", t.name, t.id)).collect();
    Err(CliError::not_found(
        format!("{place} has no tab named \"{name}\". Its tabs: {}.", names.join(", ")),
        "Name a tab exactly as listed, or by its id.",
    ))
}

/// The sheet `sheet_arg` names, on the line `copy_arg` names (none: the main sheet), with its tabs.
pub async fn target(ctx: &Ctx, sheet_arg: &str, copy_arg: Option<&str>) -> CliResult<Target> {
    let sheet = self::sheet(ctx, sheet_arg, Look::Visible).await?;
    target_of(ctx, sheet, copy_arg).await
}

/// The same for a sheet already looked up.
pub async fn target_of(ctx: &Ctx, sheet: Sheet, copy_arg: Option<&str>) -> CliResult<Target> {
    let copy = match copy_arg.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => Some(self::copy(ctx, &sheet, c).await?),
        None => None,
    };
    let line = match &copy {
        Some(c) => Line::copy(&sheet.id, &c.id),
        None => Line::main(&sheet.id),
    };
    let list = ctx.client.tabs(&line).await?;
    Ok(Target { sheet, line, copy, tabs: list.tabs, version: list.version })
}

/// A copy of `sheet` by id or exact name.
pub async fn copy(ctx: &Ctx, sheet: &Sheet, arg: &str) -> CliResult<SheetCopy> {
    let copies = ctx.client.copies(&sheet.id, true).await?;
    if let Some(c) = copies.iter().find(|c| c.id == arg) {
        return Ok(c.clone());
    }
    let named: Vec<&SheetCopy> = copies.iter().filter(|c| c.name == arg).collect();
    let open: Vec<&SheetCopy> = named.iter().copied().filter(|c| c.status == silicon_calc_client::CopyStatus::Open).collect();
    let pick = match (open.len(), named.len()) {
        (1, _) => Some(open[0]),
        (0, 1) => Some(named[0]),
        _ => None,
    };
    if let Some(c) = pick {
        return Ok(c.clone());
    }
    if named.len() > 1 {
        let list: Vec<String> = named.iter().map(|c| format!("  {}  {}  ({})", c.id, c.name, c.status.as_str())).collect();
        return Err(CliError::usage_code(
            "ambiguous",
            format!("{} copies of \"{}\" are named \"{arg}\":\n{}", named.len(), sheet.title, list.join("\n")),
            "Pass the id of the one you mean.",
        ));
    }
    let lower = arg.to_lowercase();
    let loose: Vec<&SheetCopy> = copies.iter().filter(|c| c.name.to_lowercase() == lower).collect();
    if loose.len() == 1 {
        return Ok(loose[0].clone());
    }
    Err(CliError::not_found(
        format!("\"{}\" has no copy with the id or name \"{arg}\".", sheet.title),
        format!("calc copies ls {} lists its copies.", sheet.id),
    ))
}

/// The tab named in front of a range (`Sales!A1:B9`), the `--tab` flag, or neither; and the range itself.
pub fn split_range(arg: Option<&str>, tab_flag: Option<&str>) -> CliResult<(Option<String>, Option<String>)> {
    let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok((tab_flag.map(str::to_owned), None));
    };
    let (tab, rest) = a1::split_tab(arg);
    if let (Some(t), Some(f)) = (&tab, tab_flag)
        && t != f
    {
        return Err(CliError::usage(
            format!("The range names the tab \"{t}\" but --tab names \"{f}\"."),
            "Name the tab once: either Tab!A1:B9 or --tab.",
        ));
    }
    let rest = rest.trim();
    let range = (!rest.is_empty()).then(|| rest.to_owned());
    if let Some(r) = &range
        && a1::RangeRef::parse(r).is_none()
    {
        return Err(CliError::usage(
            format!("\"{r}\" is not a range."),
            "Ranges are written like B7, A1:C9, A:C (whole columns), 3:5 (whole rows), or Sales!A1:C9 with the tab.",
        ));
    }
    Ok((tab.or_else(|| tab_flag.map(str::to_owned)), range))
}

/// A folder by id, path (`Finance/Q3`) or name.
pub async fn folder(ctx: &Ctx, arg: &str) -> CliResult<Folder> {
    if let Some(id) = id_from_link(arg) {
        return Ok(ctx.client.folder(&id).await?.folder);
    }
    let arg = arg.trim().trim_matches('/');
    if arg.is_empty() {
        return Err(CliError::usage("The folder is empty.", "Name a folder by its id, its path (Finance/Q3) or its name."));
    }
    if looks_like_id(arg) {
        match ctx.client.folder(arg).await {
            Ok(v) => return Ok(v.folder),
            Err(e) if e.is_not_found() => {}
            Err(e) => return Err(e.into()),
        }
    }
    let top = ctx.client.list_all_sheets(&ListSheets { scope: Scope::Mine, ..Default::default() }, 10_000).await?.folders;
    let shared = ctx.client.list_all_sheets(&ListSheets { scope: Scope::Shared, ..Default::default() }, 10_000).await?.folders;
    let mut roots = top.clone();
    roots.extend(shared.into_iter().filter(|f| !top.iter().any(|t| t.id == f.id)));
    if arg.contains('/') {
        let parts: Vec<&str> = arg.split('/').map(str::trim).filter(|p| !p.is_empty()).collect();
        let mut level = roots;
        let mut current: Option<Folder> = None;
        for (i, part) in parts.iter().enumerate() {
            let next = pick_folder(&level, part, &parts[..=i].join("/"))?;
            if i + 1 < parts.len() {
                level = ctx.client.folder(&next.id).await?.folders;
            }
            current = Some(next);
        }
        return current.ok_or_else(|| CliError::usage("The folder path is empty.", "Write it like Finance/Q3."));
    }
    // A plain name: the top level first, then everything below (breadth first).
    let mut queue: std::collections::VecDeque<Folder> = roots.into_iter().collect();
    let mut matches: Vec<Folder> = Vec::new();
    let mut visited = 0;
    let lower = arg.to_lowercase();
    while let Some(f) = queue.pop_front() {
        if f.name == arg || f.name.to_lowercase() == lower {
            if !matches.iter().any(|m| m.id == f.id) {
                matches.push(f.clone());
            }
            continue;
        }
        visited += 1;
        if visited > 300 {
            break;
        }
        if let Ok(view) = ctx.client.folder(&f.id).await {
            queue.extend(view.folders);
        }
    }
    let exact: Vec<&Folder> = matches.iter().filter(|f| f.name == arg).collect();
    match (exact.len(), matches.len()) {
        (1, _) => Ok(exact[0].clone()),
        (0, 1) => Ok(matches[0].clone()),
        (0, 0) => Err(CliError::not_found(
            format!("No folder you can see is named \"{arg}\"."),
            "calc folders ls lists your folders; or pass the folder's id or path (Finance/Q3).",
        )),
        _ => {
            let list: Vec<String> = matches.iter().map(|f| format!("  {}  {}", f.id, f.name)).collect();
            Err(CliError::usage_code(
                "ambiguous",
                format!("{} folders are named \"{arg}\":\n{}", matches.len(), list.join("\n")),
                "Pass the folder's id or its path (Finance/Q3).",
            ))
        }
    }
}

fn pick_folder(level: &[Folder], name: &str, path: &str) -> CliResult<Folder> {
    if let Some(f) = level.iter().find(|f| f.name == name) {
        return Ok(f.clone());
    }
    let lower = name.to_lowercase();
    let loose: Vec<&Folder> = level.iter().filter(|f| f.name.to_lowercase() == lower).collect();
    if loose.len() == 1 {
        return Ok(loose[0].clone());
    }
    Err(CliError::not_found(format!("There is no folder at \"{path}\"."), "calc folders ls shows your folders."))
}

/// A version by number, or by name (`"Sent to the client"`), or `head` for the latest.
pub async fn version(ctx: &Ctx, line: &Line, arg: &str, head: i64) -> CliResult<i64> {
    let arg = arg.trim().trim_start_matches(['v', 'V']);
    if let Ok(v) = arg.parse::<i64>() {
        if v < 0 {
            return Err(CliError::usage(format!("Version {v} is not a version number."), "Versions count up from 0."));
        }
        return Ok(v);
    }
    if arg.eq_ignore_ascii_case("head") || arg.eq_ignore_ascii_case("latest") {
        return Ok(head);
    }
    let mut cursor: Option<String> = None;
    for _ in 0..20 {
        let page = ctx.client.versions(line, cursor.as_deref(), Some(200)).await?;
        if let Some(v) = page.versions.iter().find(|v| v.name.as_deref() == Some(arg)) {
            return Ok(v.version);
        }
        match page.next_cursor {
            Some(c) if Some(&c) != cursor.as_ref() => cursor = Some(c),
            _ => break,
        }
    }
    Err(CliError::not_found(
        format!("No version is numbered or named \"{arg}\"."),
        "calc versions ls lists versions with their numbers and names.",
    ))
}

/// Checks a `c:`/`si:` account id.
pub fn account_id(arg: &str) -> CliResult<String> {
    let a = arg.trim().trim_start_matches('@').to_ascii_lowercase();
    if (a.starts_with("c:") && a.len() > 2) || (a.starts_with("si:") && a.len() > 3) {
        Ok(a)
    } else {
        Err(CliError::usage(
            format!("\"{arg}\" is not an account id."),
            "Accounts are named by their id: c:ada for a Carbon, si:analyst for a Silicon.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_links() {
        assert!(looks_like_id("k3j9xq2mpa7d"));
        assert!(!looks_like_id("Q3 revenue"));
        assert!(!looks_like_id("K3J9XQ2MPA7D"));
        assert_eq!(id_from_link("https://calc.teamofsilicons.com/s/k3j9xq2mpa7d?tab=1").as_deref(), Some("k3j9xq2mpa7d"));
        assert_eq!(id_from_link("https://calc.teamofsilicons.com/sheets/k3j9xq2mpa7d/copies/c2abcdefghij").as_deref(), Some("k3j9xq2mpa7d"));
        assert_eq!(id_from_link("k3j9xq2mpa7d"), None);
    }

    fn tabs() -> Vec<Tab> {
        vec![
            Tab { id: 1, name: "Sales".into(), index: 0, ..Default::default() },
            Tab { id: 7, name: "2024".into(), index: 2, ..Default::default() },
            Tab { id: 3, name: "By region".into(), index: 1, ..Default::default() },
        ]
    }

    #[test]
    fn tabs_by_name_or_id() {
        let t = tabs();
        assert_eq!(find_tab(&t, None, "x").unwrap().id, 1);
        assert_eq!(find_tab(&t, Some("By region"), "x").unwrap().id, 3);
        assert_eq!(find_tab(&t, Some("by REGION"), "x").unwrap().id, 3);
        // A name wins over an id.
        assert_eq!(find_tab(&t, Some("2024"), "x").unwrap().id, 7);
        assert_eq!(find_tab(&t, Some("3"), "x").unwrap().id, 3);
        let e = find_tab(&t, Some("Nope"), "\"Q3\"").unwrap_err();
        assert!(e.message.contains("\"Sales\" (id 1), \"By region\" (id 3), \"2024\" (id 7)"), "{}", e.message);
    }

    #[test]
    fn ranges_with_tabs() {
        assert_eq!(split_range(Some("Sales!A1:B2"), None).unwrap(), (Some("Sales".into()), Some("A1:B2".into())));
        assert_eq!(split_range(Some("'By region'!B:B"), None).unwrap(), (Some("By region".into()), Some("B:B".into())));
        assert_eq!(split_range(Some("A1"), Some("Sales")).unwrap(), (Some("Sales".into()), Some("A1".into())));
        assert_eq!(split_range(Some("Sales!"), None).unwrap(), (Some("Sales".into()), None));
        assert_eq!(split_range(None, Some("Sales")).unwrap(), (Some("Sales".into()), None));
        assert_eq!(split_range(Some("Sales!A1"), Some("Other")).unwrap_err().exit, 2);
        assert_eq!(split_range(Some("hello world"), None).unwrap_err().exit, 2);
    }

    #[test]
    fn account_ids() {
        assert_eq!(account_id("@si:Analyst").unwrap(), "si:analyst");
        assert_eq!(account_id("c:ada").unwrap(), "c:ada");
        assert!(account_id("ada").is_err());
        assert!(account_id("c:").is_err());
    }
}
