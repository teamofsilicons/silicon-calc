//! Sheets, folders, the trash, transfers, sharing, search, and the service itself.

use serde_json::json;
use silicon_calc_client::{
    Folder, FolderPatch, ImportSheet, Item, ListSheets, NewFolder, NewSheet, PublicAccess, PublicSetting, Role, Scope,
    ShareRequest, Sheet, SheetList, SheetPatch, TransferAction, Upload, Via,
};

use crate::ctx::{Ctx, ask};
use crate::error::{CliError, CliResult};
use crate::output::{Table, ago, plural, when_ago};
use crate::resolve::{self, Look};

pub struct LsArgs {
    pub shared: bool,
    pub trash: bool,
    pub folder: Option<String>,
    pub q: Option<String>,
    pub limit: Option<u32>,
    pub all: bool,
}

fn owner_label(s: &Sheet) -> String {
    s.owner.label().to_owned()
}

/// The readable listing of folders and sheets.
pub fn render_list(list: &SheetList, trash: bool) -> String {
    let mut t = Table::new(&["name", "kind", "id", "owner", "role", if trash { "trashed" } else { "updated" }]);
    for f in &list.folders {
        let when = if trash { f.trashed_at.as_deref().map(ago).unwrap_or_default() } else { String::new() };
        t.row(vec![format!("{}/", f.name), "folder".into(), f.id.clone(), f.owner.label().to_owned(), f.role.to_string(), when]);
    }
    for s in &list.sheets {
        let when = if trash { s.trashed_at.as_deref().map(ago).unwrap_or_default() } else { ago(&s.updated_at) };
        t.row(vec![s.title.clone(), "sheet".into(), s.id.clone(), owner_label(s), s.role.to_string(), when]);
    }
    t.render()
}

pub async fn ls(ctx: &Ctx, args: LsArgs) -> CliResult<()> {
    ctx.require_session()?;
    let scope = if args.trash {
        Scope::Trash
    } else if args.shared {
        Scope::Shared
    } else {
        Scope::Mine
    };
    let folder = match &args.folder {
        Some(f) => Some(resolve::folder(ctx, f).await?),
        None => None,
    };
    let params = ListSheets {
        scope,
        folder: folder.as_ref().map(|f| f.id.clone()),
        q: args.q.clone(),
        cursor: None,
        limit: args.limit,
    };
    let list = if args.all { ctx.client.list_all_sheets(&params, usize::MAX).await? } else { ctx.client.list_sheets(&params).await? };
    ctx.emit(&list, || {
        if list.sheets.is_empty() && list.folders.is_empty() {
            return match (scope, &folder, &args.q) {
                (_, _, Some(q)) => format!("Nothing matches \"{q}\". calc search \"{q}\" also looks inside cells."),
                (_, Some(f), _) => format!("\"{}\" is empty.", f.name),
                (Scope::Trash, ..) => "The trash is empty.".into(),
                (Scope::Shared, ..) => "Nothing is shared with you yet.".into(),
                (Scope::Mine, ..) => "No sheets yet. Make one with calc new \"Title\", or open a file with calc import data.csv.".into(),
            };
        }
        render_list(&list, scope == Scope::Trash)
    });
    if list.next_cursor.is_some() && !args.all {
        ctx.note("There are more: add --all to list everything.");
    }
    Ok(())
}

pub async fn new(ctx: &Ctx, title: Option<String>, folder: Option<String>) -> CliResult<()> {
    ctx.require_session()?;
    let folder_id = match &folder {
        Some(f) => Some(resolve::folder(ctx, f).await?.id),
        None => None,
    };
    let sheet = ctx.client.create_sheet(&NewSheet { title, folder_id }).await?;
    ctx.emit(&sheet, || format!("Made \"{}\" ({}).", sheet.title, sheet.id));
    ctx.note(format!("Next: calc write {} A1 name amount, or calc tabs import {} data.csv.", sheet.id, sheet.id));
    Ok(())
}

/// An upload from a path, or from stdin (`-`) named by `format`.
pub fn upload_from(path: &str, format: Option<&str>) -> CliResult<Upload> {
    if path == "-" {
        let Some(format) = format else {
            return Err(CliError::usage(
                "Reading a file from stdin needs its format.",
                "Add --format csv (or tsv, xlsx, xls, ods, numbers, json, parquet).",
            ));
        };
        let f = silicon_calc_client::Format::parse(format).filter(|f| f.can_import()).ok_or_else(|| {
            CliError::usage(format!("\"{format}\" is not a format Silicon Calc opens."), "Use csv, tsv, xlsx, xls, ods, numbers, json or parquet.")
        })?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf).map_err(|e| CliError::file(format!("Can't read stdin: {e}.")))?;
        return Ok(Upload::from_bytes(format!("stdin.{}", f.ext()), buf));
    }
    let upload = Upload::from_path(path).map_err(|e| CliError::file(e.message().to_owned()))?;
    let named = match format {
        Some(fmt) => {
            let f = silicon_calc_client::Format::parse(fmt)
                .filter(|f| f.can_import())
                .ok_or_else(|| CliError::usage(format!("\"{fmt}\" is not a format Silicon Calc opens."), "Use csv, tsv, xlsx, xls, ods, numbers, json or parquet."))?;
            let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            upload.named(format!("{stem}.{}", f.ext()))
        }
        None => upload,
    };
    match silicon_calc_client::Format::from_file_name(&named.file_name) {
        Some(f) if f.can_import() => Ok(named),
        Some(_) => Err(CliError::usage("PDF files can't be opened; Silicon Calc only writes them.", "Open a .csv, .tsv, .xlsx, .xls, .ods, .numbers, .json or .parquet file.")),
        None => Err(CliError::usage(
            format!("Silicon Calc can't tell the format of {} from its name.", named.file_name),
            "Name it with its extension, or add --format csv (or tsv, xlsx, xls, ods, numbers, json, parquet).",
        )),
    }
}

pub async fn import(ctx: &Ctx, file: &str, title: Option<String>, folder: Option<String>, format: Option<&str>) -> CliResult<()> {
    ctx.require_session()?;
    let upload = upload_from(file, format)?;
    let folder_id = match &folder {
        Some(f) => Some(resolve::folder(ctx, f).await?.id),
        None => None,
    };
    let sheet = ctx.client.import_sheet(&upload, &ImportSheet { title, folder_id }).await?;
    ctx.emit(&sheet, || {
        let tabs = sheet.tabs.as_ref().map(|t| t.iter().map(|t| format!("\"{}\"", t.name)).collect::<Vec<_>>().join(", "));
        match tabs {
            Some(t) if !t.is_empty() => format!("Opened {} as \"{}\" ({}), tabs {t}.", upload.file_name, sheet.title, sheet.id),
            _ => format!("Opened {} as \"{}\" ({}).", upload.file_name, sheet.title, sheet.id),
        }
    });
    ctx.note(format!("Next: calc read {} reads it; calc run sql {} -c 'select …' asks it questions.", sheet.id, sheet.id));
    Ok(())
}

pub async fn info(ctx: &Ctx, sheet_arg: &str, copy: Option<&str>) -> CliResult<()> {
    let target = resolve::target(ctx, sheet_arg, copy).await?;
    let s = &target.sheet;
    let folder_name = match &s.folder_id {
        Some(f) => match ctx.client.folder(f).await {
            Ok(v) => format!("{} ({})", v.folder.name, f),
            Err(_) => f.clone(),
        },
        None => "top level".into(),
    };
    let mut value = serde_json::to_value(s).unwrap_or_default();
    value["tabs"] = serde_json::to_value(&target.tabs).unwrap_or_default();
    value["line_version"] = json!(target.version);
    if let Some(c) = &target.copy {
        value["copy"] = serde_json::to_value(c).unwrap_or_default();
    }
    ctx.emit(&value, || {
        let mut out = format!("{}\n", s.title);
        let mut row = |k: &str, v: String| out.push_str(&format!("{k:<10}{v}\n"));
        row("id", s.id.clone());
        row("owner", format!("{} ({})", s.owner.label(), s.owner.display_name));
        row("your role", s.role.to_string());
        row("folder", folder_name.clone());
        row("public", if s.public.enabled { format!("on, anyone with the link can {}", s.public.access.as_str()) } else { "off".into() });
        row("version", s.version.to_string());
        row("created", format!("{} by {}", when_ago(&s.created_at), s.created_by.label()));
        row("updated", format!("{} by {}", when_ago(&s.updated_at), s.updated_by.label()));
        row("copies", format!("{} open", s.copies));
        if let Some(t) = &s.trashed_at {
            row("trashed", when_ago(t));
        }
        if let Some(c) = &target.copy {
            row("copy", format!("\"{}\" ({}), {}, version {}", c.name, c.id, c.status.as_str(), target.version));
        }
        out.push('\n');
        out.push_str(&render_tabs(&target.tabs));
        out
    });
    Ok(())
}

pub fn render_tabs(tabs: &[silicon_calc_client::Tab]) -> String {
    let mut sorted: Vec<&silicon_calc_client::Tab> = tabs.iter().collect();
    sorted.sort_by_key(|t| t.index);
    let mut t = Table::new(&["#", "tab", "id", "used", "frozen", "made by code"]).right(0).right(2);
    for tab in sorted {
        let frozen = match (tab.frozen_rows, tab.frozen_cols) {
            (0, 0) => String::new(),
            (r, 0) => plural(u64::from(r), "row", "rows"),
            (0, c) => plural(u64::from(c), "column", "columns"),
            (r, c) => format!("{} and {}", plural(u64::from(r), "row", "rows"), plural(u64::from(c), "column", "columns")),
        };
        let code = tab
            .code
            .as_ref()
            .map(|c| {
                format!("{} by {}{}", c.language.as_str(), c.run_by.label(), if c.keep_updated { ", keeps updated" } else { "" })
            })
            .unwrap_or_default();
        t.row(vec![(tab.index + 1).to_string(), tab.name.clone(), tab.id.to_string(), tab.used.clone().unwrap_or_else(|| "empty".into()), frozen, code]);
    }
    t.render()
}

pub async fn rename(ctx: &Ctx, sheet_arg: &str, title: &str) -> CliResult<()> {
    let sheet = resolve::sheet(ctx, sheet_arg, Look::Visible).await?;
    let updated = ctx.client.update_sheet(&sheet.id, &SheetPatch { title: Some(title.to_owned()), folder_id: None }).await?;
    ctx.emit(&updated, || format!("Renamed \"{}\" to \"{}\".", sheet.title, updated.title));
    Ok(())
}

/// `/` (or empty) is the top level.
async fn folder_or_top(ctx: &Ctx, arg: &str) -> CliResult<Option<Folder>> {
    let a = arg.trim();
    if a.is_empty() || a == "/" || a == "~" {
        Ok(None)
    } else {
        Ok(Some(resolve::folder(ctx, a).await?))
    }
}

pub async fn mv(ctx: &Ctx, sheet_arg: &str, folder_arg: &str) -> CliResult<()> {
    let sheet = resolve::sheet(ctx, sheet_arg, Look::Visible).await?;
    let folder = folder_or_top(ctx, folder_arg).await?;
    let patch = SheetPatch { title: None, folder_id: Some(folder.as_ref().map(|f| f.id.clone())) };
    let updated = ctx.client.update_sheet(&sheet.id, &patch).await?;
    ctx.emit(&updated, || match &folder {
        Some(f) => format!("Moved \"{}\" into {}.", sheet.title, f.name),
        None => format!("Moved \"{}\" to the top level.", sheet.title),
    });
    Ok(())
}

pub async fn rm(ctx: &Ctx, sheet_arg: &str) -> CliResult<()> {
    let sheet = resolve::sheet(ctx, sheet_arg, Look::Visible).await?;
    ctx.client.trash_sheet(&sheet.id).await?;
    ctx.emit(&json!({"trashed": true, "id": sheet.id, "title": sheet.title}), || {
        format!("Moved \"{}\" to the trash. Bring it back with calc restore {}.", sheet.title, sheet.id)
    });
    Ok(())
}

pub async fn restore(ctx: &Ctx, sheet_arg: &str) -> CliResult<()> {
    let sheet = resolve::sheet(ctx, sheet_arg, Look::Trash).await?;
    let restored = ctx.client.restore_sheet(&sheet.id).await?;
    ctx.emit(&restored, || format!("Restored \"{}\" from the trash.", restored.title));
    Ok(())
}

pub async fn trash(ctx: &Ctx, all: bool) -> CliResult<()> {
    ls(ctx, LsArgs { shared: false, trash: true, folder: None, q: None, limit: None, all }).await
}

/// Asks for the title (or name) before deleting for good, unless `--confirm` said so already.
fn confirm_forever(ctx: &Ctx, what: &str, name: &str, confirmed: bool) -> CliResult<()> {
    if confirmed {
        return Ok(());
    }
    if !ctx.interactive() {
        return Err(CliError::usage_code(
            "confirm_needed",
            format!("Deleting {what} \"{name}\" forever can't be undone, so it needs confirming."),
            "Add --confirm to delete it without being asked.",
        ));
    }
    eprintln!("This deletes {what} \"{name}\" forever, with its history. It can't be undone.");
    let typed = ask(&format!("Type its {} to confirm:", if what == "the folder" { "name" } else { "title" }))?;
    if typed == name {
        Ok(())
    } else {
        Err(CliError::usage_code("not_confirmed", "That didn't match, so nothing was deleted.", "Type it exactly, or pass --confirm."))
    }
}

pub async fn delete_forever(ctx: &Ctx, sheet_arg: &str, confirmed: bool) -> CliResult<()> {
    let sheet = resolve::sheet(ctx, sheet_arg, Look::Trash).await?;
    if sheet.trashed_at.is_none() {
        return Err(CliError::new(
            "conflict",
            format!("\"{}\" isn't in the trash; only sheets in the trash can be deleted forever.", sheet.title),
            format!("Move it to the trash first: calc rm {}.", sheet.id),
        ));
    }
    confirm_forever(ctx, "the sheet", &sheet.title, confirmed)?;
    ctx.client.delete_sheet_forever(&sheet.id, &sheet.title).await?;
    ctx.emit(&json!({"deleted": true, "id": sheet.id, "title": sheet.title}), || format!("Deleted \"{}\" forever.", sheet.title));
    Ok(())
}

pub async fn transfer(ctx: &Ctx, item: Item, label: &str, to: &str) -> CliResult<()> {
    let to = resolve::account_id(to)?;
    let t = ctx.client.transfer(&item, &to).await?;
    ctx.emit(&t, || {
        format!("Offered {label} to {}. It moves to them when they accept (calc transfers accept {}).", t.to.label(), t.id)
    });
    Ok(())
}

pub async fn transfers(ctx: &Ctx) -> CliResult<()> {
    ctx.require_session()?;
    let list = ctx.client.transfers().await?;
    ctx.emit(&list, || {
        if list.incoming.is_empty() && list.outgoing.is_empty() {
            return "No transfers.".into();
        }
        let mut t = Table::new(&["id", "way", "kind", "title", "from", "to", "status", "when"]);
        for (way, items) in [("incoming", &list.incoming), ("outgoing", &list.outgoing)] {
            for x in items {
                t.row(vec![
                    x.id.clone(),
                    way.into(),
                    x.kind.clone(),
                    x.title.clone(),
                    x.from.label().to_owned(),
                    x.to.label().to_owned(),
                    x.status.as_str().into(),
                    ago(&x.created_at),
                ]);
            }
        }
        t.render()
    });
    Ok(())
}

pub async fn transfer_action(ctx: &Ctx, id: &str, action: TransferAction) -> CliResult<()> {
    ctx.require_session()?;
    let t = ctx.client.transfer_action(id, action).await?;
    ctx.emit(&t, || match action {
        TransferAction::Accept => format!("Accepted: {} \"{}\" is yours now.", t.kind, t.title),
        TransferAction::Decline => format!("Declined {} \"{}\"; it stays with {}.", t.kind, t.title, t.from.label()),
        TransferAction::Cancel => format!("Cancelled the transfer of \"{}\" to {}.", t.title, t.to.label()),
    });
    Ok(())
}

// ── Folders ─────────────────────────────────────────────────────────────────────────────────────────────

pub async fn folder_new(ctx: &Ctx, name: &str, parent: Option<String>) -> CliResult<()> {
    ctx.require_session()?;
    let parent = match &parent {
        Some(p) => folder_or_top(ctx, p).await?,
        None => None,
    };
    let f = ctx.client.create_folder(&NewFolder { name: name.to_owned(), parent_id: parent.as_ref().map(|p| p.id.clone()) }).await?;
    ctx.emit(&f, || match &parent {
        Some(p) => format!("Made the folder \"{}\" ({}) in {}.", f.name, f.id, p.name),
        None => format!("Made the folder \"{}\" ({}).", f.name, f.id),
    });
    Ok(())
}

pub async fn folder_ls(ctx: &Ctx, folder: Option<String>, all: bool) -> CliResult<()> {
    ctx.require_session()?;
    let Some(arg) = folder else {
        let list = if all {
            ctx.client.list_all_sheets(&ListSheets::default(), usize::MAX).await?
        } else {
            ctx.client.list_sheets(&ListSheets::default()).await?
        };
        let only = SheetList { sheets: Vec::new(), folders: list.folders.clone(), next_cursor: None };
        ctx.emit(&only.folders, || if only.folders.is_empty() { "No folders yet. Make one with calc folders new Finance.".into() } else { render_list(&only, false) });
        return Ok(());
    };
    let f = resolve::folder(ctx, &arg).await?;
    let view = ctx.client.folder(&f.id).await?;
    ctx.emit(&view, || {
        let mut path: Vec<String> = view.path.iter().map(|p| p.name.clone()).collect();
        path.push(view.folder.name.clone());
        let mut out = format!("{}/  ({}, owner {}, your role {})\n", path.join("/"), view.folder.id, view.folder.owner.label(), view.folder.role);
        let list = SheetList { sheets: view.sheets.clone(), folders: view.folders.clone(), next_cursor: None };
        if list.sheets.is_empty() && list.folders.is_empty() {
            out.push_str("It is empty.\n");
        } else {
            out.push_str(&render_list(&list, false));
        }
        out
    });
    Ok(())
}

pub async fn folder_rename(ctx: &Ctx, folder: &str, name: &str) -> CliResult<()> {
    let f = resolve::folder(ctx, folder).await?;
    let updated = ctx.client.update_folder(&f.id, &FolderPatch { name: Some(name.to_owned()), parent_id: None }).await?;
    let value = updated.map(|u| serde_json::to_value(u).unwrap_or_default()).unwrap_or_else(|| json!({"id": f.id, "name": name}));
    ctx.emit(&value, || format!("Renamed the folder \"{}\" to \"{name}\".", f.name));
    Ok(())
}

pub async fn folder_mv(ctx: &Ctx, folder: &str, parent: &str) -> CliResult<()> {
    let f = resolve::folder(ctx, folder).await?;
    let parent = folder_or_top(ctx, parent).await?;
    if parent.as_ref().is_some_and(|p| p.id == f.id) {
        return Err(CliError::usage("A folder can't go inside itself.", "Pick another folder, or / for the top level."));
    }
    let patch = FolderPatch { name: None, parent_id: Some(parent.as_ref().map(|p| p.id.clone())) };
    let updated = ctx.client.update_folder(&f.id, &patch).await?;
    let value = updated.map(|u| serde_json::to_value(u).unwrap_or_default()).unwrap_or_else(|| json!({"id": f.id, "parent_id": parent.as_ref().map(|p| p.id.clone())}));
    ctx.emit(&value, || match &parent {
        Some(p) => format!("Moved the folder \"{}\" into {}.", f.name, p.name),
        None => format!("Moved the folder \"{}\" to the top level.", f.name),
    });
    Ok(())
}

pub async fn folder_rm(ctx: &Ctx, folder: &str) -> CliResult<()> {
    let f = resolve::folder(ctx, folder).await?;
    ctx.client.trash_folder(&f.id).await?;
    ctx.emit(&json!({"trashed": true, "id": f.id, "name": f.name}), || {
        format!("Moved the folder \"{}\" and everything in it to the trash. Bring it back with calc folders restore {}.", f.name, f.id)
    });
    Ok(())
}

/// A trashed folder by id or name.
async fn trashed_folder(ctx: &Ctx, arg: &str) -> CliResult<Folder> {
    if resolve::looks_like_id(arg) {
        return Ok(ctx.client.folder(arg).await?.folder);
    }
    let trash = ctx.client.list_all_sheets(&ListSheets { scope: Scope::Trash, ..Default::default() }, 10_000).await?;
    let named: Vec<&Folder> = trash.folders.iter().filter(|f| f.name == arg).collect();
    match named.len() {
        1 => Ok(named[0].clone()),
        0 => Err(CliError::not_found(format!("No folder in your trash is named \"{arg}\"."), "calc trash lists it; or pass the folder's id.")),
        n => Err(CliError::usage_code("ambiguous", format!("{n} folders in your trash are named \"{arg}\"."), "Pass the folder's id (calc trash lists them).")),
    }
}

pub async fn folder_restore(ctx: &Ctx, folder: &str) -> CliResult<()> {
    let f = trashed_folder(ctx, folder).await?;
    let restored = ctx.client.restore_folder(&f.id).await?;
    let value = restored.map(|r| serde_json::to_value(r).unwrap_or_default()).unwrap_or_else(|| json!({"id": f.id, "name": f.name, "restored": true}));
    ctx.emit(&value, || format!("Restored the folder \"{}\" from the trash.", f.name));
    Ok(())
}

pub async fn folder_delete_forever(ctx: &Ctx, folder: &str, confirmed: bool) -> CliResult<()> {
    let f = trashed_folder(ctx, folder).await?;
    confirm_forever(ctx, "the folder", &f.name, confirmed)?;
    ctx.client.delete_folder_forever(&f.id, &f.name).await?;
    ctx.emit(&json!({"deleted": true, "id": f.id, "name": f.name}), || format!("Deleted the folder \"{}\" forever.", f.name));
    Ok(())
}

// ── Sharing ─────────────────────────────────────────────────────────────────────────────────────────────

/// A sheet or folder: `--folder` says which; otherwise a sheet, then a folder.
pub async fn item(ctx: &Ctx, arg: &str, folder: bool) -> CliResult<(Item, String)> {
    if folder {
        let f = resolve::folder(ctx, arg).await?;
        return Ok((Item::Folder(f.id.clone()), format!("the folder \"{}\"", f.name)));
    }
    match resolve::sheet(ctx, arg, Look::Visible).await {
        Ok(s) => Ok((Item::Sheet(s.id.clone()), format!("\"{}\"", s.title))),
        Err(e) if e.code == "not_found" => match resolve::folder(ctx, arg).await {
            Ok(f) => Ok((Item::Folder(f.id.clone()), format!("the folder \"{}\"", f.name))),
            Err(_) => Err(e),
        },
        Err(e) => Err(e),
    }
}

fn render_sharing(s: &silicon_calc_client::Sharing, label: &str) -> String {
    let mut out = format!("{label} belongs to {} ({}).\n", s.owner.label(), s.owner.display_name);
    if s.members.is_empty() && s.invites.is_empty() {
        out.push_str("It isn't shared with anyone.\n");
    } else {
        let mut t = Table::new(&["who", "name", "role", "how"]);
        for m in &s.members {
            let how = match &m.via {
                Via::Direct => "invited".to_owned(),
                Via::Folder(f) => format!("through the folder {}", f.name),
            };
            t.row(vec![m.account.label().to_owned(), m.account.display_name.clone(), m.role.to_string(), how]);
        }
        for i in &s.invites {
            t.row(vec![i.email.clone(), String::new(), i.role.to_string(), format!("invited by {}, no account yet", i.invited_by.label())]);
        }
        out.push_str(&t.render());
    }
    if s.public.enabled {
        out.push_str(&format!("Public link: on, anyone with it can {}.\n", s.public.access.as_str()));
    }
    out
}

pub async fn share_ls(ctx: &Ctx, arg: &str, folder: bool) -> CliResult<()> {
    let (item, label) = item(ctx, arg, folder).await?;
    let s = ctx.client.sharing(&item).await?;
    ctx.emit(&s, || render_sharing(&s, &label));
    Ok(())
}

pub fn parse_role(s: &str) -> CliResult<Role> {
    match Role::parse(s) {
        Some(Role::Owner) => Err(CliError::usage(
            "Owner isn't a role to give; every sheet and folder has exactly one owner.",
            "To give it away, use calc transfer.",
        )),
        Some(r) => Ok(r),
        None => Err(CliError::usage(format!("\"{s}\" is not a role."), "Roles are editor, commenter and viewer.")),
    }
}

pub async fn share_add(ctx: &Ctx, arg: &str, who: &[String], role: &str, notify: bool, folder: bool) -> CliResult<()> {
    let role = parse_role(role)?;
    let mut invite = Vec::new();
    for w in who {
        let w = w.trim();
        if w.contains('@') && !w.starts_with('@') {
            invite.push(w.to_owned());
        } else {
            invite.push(resolve::account_id(w)?);
        }
    }
    if invite.is_empty() {
        return Err(CliError::usage("Nobody to share with.", "Name them: c:ada, si:analyst or an email address."));
    }
    let (item, label) = item(ctx, arg, folder).await?;
    let s = ctx.client.share(&item, &ShareRequest { invite: invite.clone(), role, notify }).await?;
    ctx.emit(&s, || format!("Shared {label} with {} as {role}.\n\n{}", invite.join(", "), render_sharing(&s, &label)));
    Ok(())
}

/// The uuid of a member named by id or uuid.
async fn member_uuid(ctx: &Ctx, item: &Item, who: &str) -> CliResult<(String, String)> {
    let s = ctx.client.sharing(item).await?;
    let w = who.trim().trim_start_matches('@');
    let found = s
        .members
        .iter()
        .find(|m| m.account.uuid == w || m.account.id.eq_ignore_ascii_case(w))
        .map(|m| (m.account.uuid.clone(), m.account.label().to_owned()));
    found.ok_or_else(|| {
        CliError::not_found(
            format!("{w} isn't among the people it is shared with."),
            "calc share ls lists who has it; someone with it through a folder is changed on that folder.",
        )
    })
}

pub async fn share_role(ctx: &Ctx, arg: &str, who: &str, role: &str, folder: bool) -> CliResult<()> {
    let role = parse_role(role)?;
    let (item, label) = item(ctx, arg, folder).await?;
    let (uuid, id) = member_uuid(ctx, &item, who).await?;
    let answer = ctx.client.set_member_role(&item, &uuid, role).await?;
    let value = answer.map(|a| serde_json::to_value(a).unwrap_or_default()).unwrap_or_else(|| json!({"account": id, "role": role}));
    ctx.emit(&value, || format!("{id} is now a {role} on {label}."));
    Ok(())
}

pub async fn share_rm(ctx: &Ctx, arg: &str, who: &str, folder: bool) -> CliResult<()> {
    let (item, label) = item(ctx, arg, folder).await?;
    let w = who.trim();
    if w.contains('@') && !w.starts_with('@') {
        ctx.client.remove_invite(&item, w).await?;
        ctx.emit(&json!({"removed_invite": w}), || format!("Removed the invite for {w} to {label}."));
        return Ok(());
    }
    let (uuid, id) = member_uuid(ctx, &item, w).await?;
    ctx.client.remove_member(&item, &uuid).await?;
    ctx.emit(&json!({"removed": id, "uuid": uuid}), || format!("{id} no longer has {label}."));
    Ok(())
}

pub async fn share_leave(ctx: &Ctx, arg: &str, folder: bool) -> CliResult<()> {
    ctx.require_session()?;
    let (item, label) = item(ctx, arg, folder).await?;
    let me = ctx.client.me().await?;
    ctx.client.remove_member(&item, &me.uuid).await?;
    ctx.emit(&json!({"left": item.id()}), || format!("You left {label}."));
    Ok(())
}

pub async fn share_public(ctx: &Ctx, arg: &str, state: &str, access: Option<&str>) -> CliResult<()> {
    let enabled = match state.trim().to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" | "enable" => true,
        "off" | "no" | "false" | "disable" => false,
        other => return Err(CliError::usage(format!("\"{other}\" is neither on nor off."), "Write calc share public SHEET on (or off).")),
    };
    let access = match access {
        Some(a) => PublicAccess::parse(a).ok_or_else(|| CliError::usage(format!("\"{a}\" is not a link access."), "Use view, comment or edit."))?,
        None => PublicAccess::View,
    };
    let sheet = resolve::sheet(ctx, arg, Look::Visible).await?;
    let link = ctx.client.set_public(&sheet.id, &PublicSetting { enabled, access }).await?;
    ctx.emit(&link, || {
        if link.enabled {
            let who = match link.access {
                PublicAccess::View => "anyone with it can view",
                PublicAccess::Comment => "anyone with it can view, and signed-in accounts can comment",
                PublicAccess::Edit => "anyone with it can view, and signed-in accounts can edit",
            };
            format!("The public link of \"{}\" is on: {who}.\n{}", sheet.title, link.url)
        } else {
            format!("The public link of \"{}\" is off.", sheet.title)
        }
    });
    Ok(())
}

// ── Search and the service ──────────────────────────────────────────────────────────────────────────────

pub async fn search(ctx: &Ctx, text: &str, limit: Option<u32>) -> CliResult<()> {
    ctx.require_session()?;
    let results = ctx.client.search(text, limit).await?;
    ctx.emit(&json!({"results": results}), || {
        if results.is_empty() {
            return format!("Nothing you can read matches \"{text}\".");
        }
        let mut out = String::new();
        for r in &results {
            out.push_str(&format!("{}  ({}, owner {})\n", r.sheet.title, r.sheet.id, r.sheet.owner.label()));
            for m in r.matches.iter().take(10) {
                out.push_str(&format!("  {:<14} {}\n", silicon_calc_client::a1::with_tab(&m.tab_name, &m.a), crate::output::truncate(&crate::output::one_line(&m.text), 80)));
            }
            if r.matches.len() > 10 {
                out.push_str(&format!("  … and {} more cells\n", r.matches.len() - 10));
            }
        }
        out
    });
    Ok(())
}

pub async fn meta(ctx: &Ctx) -> CliResult<()> {
    let m = ctx.client.meta().await?;
    ctx.emit(&m, || {
        format!(
            "Silicon Calc {} at {}\napp id        {}\naccounts      {}\nmost rows     {}\nmost columns  {}\nlargest file  {} MB\ncells per edit {}\ncode runs for {} seconds at most\n",
            m.version,
            ctx.api_url,
            m.app_id,
            m.accounts_url,
            crate::output::thousands(m.limits.max_rows),
            crate::output::thousands(m.limits.max_cols),
            m.limits.max_upload_bytes / (1024 * 1024),
            crate::output::thousands(m.limits.max_cells_per_edit),
            m.limits.code_timeout_seconds
        )
    });
    Ok(())
}

pub async fn lookup(ctx: &Ctx, id: &str) -> CliResult<()> {
    ctx.require_session()?;
    let id = resolve::account_id(id)?;
    let a = ctx.client.lookup_account(&id).await?;
    ctx.emit(&a, || format!("{} · {} · {}", a.label(), a.display_name, if a.is_silicon() { "Silicon" } else { "Carbon" }));
    Ok(())
}
