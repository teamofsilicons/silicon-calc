//! File uploads are spooled to private files before parsing; downloads use the same workbook contract as code.
use crate::{
    auth::{Caller, MaybeCaller},
    code::{self, LinePath},
    error::{ApiError, ApiResult, QueryParams},
    lines::{self, ApplyRequest, OpKind},
    perms, sheets,
    state::AppState,
};
use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use calc_engine::{
    Workbook,
    formats::{ExportOptions, Format, ImportOptions, Imported},
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

pub fn router() -> Router<AppState> {
    let r = Router::new().route("/sheets/import", post(import_sheet));
    let r = code::both(r, "/tabs/import", post(import_tabs));
    code::both(r, "/download", get(download))
}

#[derive(Deserialize)]
struct Upload {
    filename: String,
    title: Option<String>,
    folder_id: Option<String>,
    mode: Option<String>,
    tab: Option<String>,
}
struct Uploaded(std::path::PathBuf);
impl Drop for Uploaded {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn receive(state: &AppState, body: Body) -> ApiResult<Uploaded> {
    let path = std::env::temp_dir().join(format!("calc-upload-{}", uuid::Uuid::new_v4().simple()));
    let guard = Uploaded(path.clone());
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path).await.map_err(ApiError::internal)?;
    let mut stream = body.into_data_stream();
    let mut length = 0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            ApiError::bad_request(
                format!("The upload was interrupted: {e}"),
                "Send the file again.",
            )
        })?;
        length = length.saturating_add(chunk.len());
        if length > state.cfg.max_upload_bytes {
            return Err(ApiError::too_large(
                "The file is larger than the upload limit.",
                "Split it into smaller files.",
            ));
        }
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
    }
    file.flush().await.map_err(ApiError::internal)?;
    Ok(guard)
}

async fn parse(state: &AppState, body: Body, name: String) -> ApiResult<Imported> {
    let upload = receive(state, body).await?;
    let bytes = tokio::fs::read(&upload.0).await.map_err(ApiError::internal)?;
    parse_bytes(state, bytes, name).await
}

pub(crate) async fn parse_bytes(state: &AppState, bytes: Vec<u8>, name: String) -> ApiResult<Imported> {
    let runner = code::runner(state);
    tokio::task::spawn_blocking(move || {
        let format = Format::detect(&name, &bytes).ok_or_else(|| {
            ApiError::unprocessable(
                "unknown_format",
                "The file format could not be recognized.",
                "Choose CSV, TSV, XLSX, XLS, ODS, Numbers, JSON or Parquet.",
            )
        })?;
        if !format.can_open() {
            return Err(ApiError::unprocessable(
                "unsupported_format",
                "PDF and HTML can only be downloaded.",
                "Open a spreadsheet or data file instead.",
            ));
        }
        let mut imported = runner
            .import_file(format, &bytes, &ImportOptions::default())
            .map_err(code::runner_error)?;
        for note in &imported.notes {
            if let Some((_, content)) = imported.tabs.get_mut(note.tab) {
                content.rows = content.rows.max(note.row.saturating_add(1));
                content.cols = content.cols.max(note.col.saturating_add(1));
            }
        }
        if imported.tabs.is_empty() {
            return Err(ApiError::unprocessable(
                "empty_file",
                "This file has no tabs.",
                "Choose a file containing a table.",
            ));
        }
        if matches!(format, Format::Csv | Format::Tsv | Format::Parquet) && imported.tabs.len() == 1
        {
            if let Some(stem) = std::path::Path::new(&name)
                .file_stem()
                .and_then(|s| s.to_str())
            {
                imported.tabs[0].0 = stem.to_owned();
            }
        }
        Ok(imported)
    })
    .await
    .map_err(ApiError::internal)?
}

async fn import_sheet(
    State(state): State<AppState>,
    caller: Caller,
    QueryParams(query): QueryParams<Upload>,
    body: Body,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // Validate folder access before accepting a potentially large upload.
    sheets::placement(&state, &caller, query.folder_id.as_deref()).await?;
    let imported = parse(&state, body, query.filename.clone()).await?;
    let warnings = imported.warnings.clone();
    let notes = imported.notes.clone();
    let default_title = std::path::Path::new(&query.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported sheet");
    let access = sheets::create_sheet(
        &state,
        &caller,
        Some(query.title.as_deref().unwrap_or(default_title)),
        query.folder_id.as_deref(),
        &Workbook::empty(),
    )
    .await?;
    let line = state
        .lines
        .open(&state, &crate::ids::main_line(&access.sheet.id))
        .await?;
    line.apply(ApplyRequest {
        author: caller.uuid,
        kind: OpKind::Import,
        summary: Some(format!("opened {}", query.filename)),
        base_version: Some(0),
        client_id: None,
        build: Box::new(move |wb| Ok(Some(imported.plan(wb, None).op))),
    })
    .await?;
    let ids = line
        .read(|wb, _| wb.tabs.iter().map(|t| t.id).collect::<Vec<_>>())
        .await?;
    save_notes(&state, &line, caller.uuid, notes, ids).await?;
    // The initial empty version has no search text. Publish the imported index before replying.
    line.shared.snapshot().await?;
    let mut answer = sheets::sheet_json_full(&state, &access).await?;
    if !warnings.is_empty() {
        answer["warnings"] = json!(warnings);
    }
    Ok((StatusCode::CREATED, Json(answer)))
}

async fn import_tabs(
    State(state): State<AppState>,
    caller: Caller,
    Path(path): Path<LinePath>,
    QueryParams(query): QueryParams<Upload>,
    body: Body,
) -> ApiResult<Json<Value>> {
    let line = code::handle(&state, Some(&caller), &path, true).await?;
    let mode = query.mode.as_deref().unwrap_or("new_tab");
    if !matches!(mode, "new_tab" | "replace") {
        return Err(ApiError::invalid_field(
            "mode",
            "Choose new_tab or replace.",
        ));
    }
    let replace = mode == "replace";
    let imported = parse(&state, body, query.filename.clone()).await?;
    // Conversion may take a minute; sharing, trash and copy status may have changed.
    code::handle(&state, Some(&caller), &path, true).await?;
    let warnings = imported.warnings.clone();
    let notes = imported.notes.clone();
    let created = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = created.clone();
    let outcome = line
        .apply(ApplyRequest {
            author: caller.uuid,
            kind: OpKind::Import,
            summary: Some(format!("opened {}", query.filename)),
            base_version: None,
            client_id: None,
            build: Box::new(move |wb| {
                if replace {
                    let name = query.tab.as_deref().ok_or_else(|| {
                        ApiError::invalid_field("tab", "Choose the tab to replace.")
                    })?;
                    let id = code::tab_id(wb, &json!(name))?;
                    *capture.lock().unwrap() = vec![id];
                    return Ok(imported.plan_replace(wb, id));
                }
                let plan = imported.plan(wb, None);
                *capture.lock().unwrap() = plan.tabs;
                Ok(Some(plan.op))
            }),
        })
        .await?;
    let ids = created.lock().unwrap().clone();
    save_notes(&state, &line, caller.uuid, notes, ids).await?;
    // The initial empty version has no search text. Publish the imported index before replying.
    line.shared.snapshot().await?;
    let mut tabs = line
        .read(move |wb, _| {
            Value::Array(
                wb.tabs
                    .iter()
                    .enumerate()
                    .map(|(i, t)| lines::tab_json(wb, t, i))
                    .collect(),
            )
        })
        .await?;
    lines::resolve_tab_accounts(&state, &mut tabs).await;
    Ok(Json(
        json!({"version":outcome.version,"tabs":tabs,"warnings":warnings}),
    ))
}

#[derive(Deserialize)]
struct Download {
    format: String,
    tab: Option<String>,
    version: Option<i64>,
    #[serde(default)]
    formulas: bool,
}
async fn download(
    State(state): State<AppState>,
    MaybeCaller(caller): MaybeCaller,
    Path(path): Path<LinePath>,
    QueryParams(query): QueryParams<Download>,
) -> ApiResult<Response> {
    let access = perms::sheet_access(&state, caller.as_ref(), &path.sheet).await?;
    let line = code::handle(&state, caller.as_ref(), &path, false).await?;
    let wb = if let Some(version) = query.version {
        lines::workbook_at(&state, &line.line_id, version).await?
    } else {
        line.read(|wb, _| wb.clone()).await?
    };
    let format = Format::from_extension(&query.format).ok_or_else(|| {
        ApiError::invalid_field(
            "format",
            "Choose xlsx, ods, pdf, html, csv, tsv, xls, numbers, json or parquet.",
        )
    })?;
    let tab = query
        .tab
        .as_ref()
        .map(|t| code::tab_id(&wb, &json!(t)))
        .transpose()?;
    let options = ExportOptions {
        tab,
        values_only: format == Format::Json && !query.formulas,
    };
    let runner = code::runner(&state);
    let bytes = tokio::task::spawn_blocking(move || runner.export_file(&wb, format, &options))
        .await
        .map_err(ApiError::internal)?
        .map_err(code::runner_error)?;
    let title: String = access
        .sheet
        .title
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '/'))
        .take(150)
        .collect();
    let encoded =
        url::form_urlencoded::byte_serialize(format!("{title}.{}", format.extension()).as_bytes())
            .collect::<String>();
    let ascii: String = title
        .chars()
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();
    Ok((
        [
            (header::CONTENT_TYPE, format.media_type().to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"{ascii}.{}\"; filename*=UTF-8''{encoded}",
                    format.extension()
                ),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// File authors are preserved in the body; the authenticated importer owns the resulting comments.
async fn save_notes(
    state: &AppState,
    line: &lines::LineHandle,
    author: uuid::Uuid,
    notes: Vec<calc_engine::formats::Note>,
    ids: Vec<calc_engine::TabId>,
) -> ApiResult<()> {
    if notes.is_empty() {
        return Ok(());
    }
    let placed = line.read(move |wb, _| place_notes(wb, notes, &ids)).await?;
    let mut tx = state.db.begin().await?;
    insert_notes(&mut tx, &line.sheet_id, &line.line_id, author, placed).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) type PlacedNote = (calc_engine::formats::Note, calc_engine::TabId, u32, u32, String, String, String);

pub(crate) fn place_notes(wb: &Workbook, notes: Vec<calc_engine::formats::Note>, ids: &[calc_engine::TabId]) -> Vec<PlacedNote> {
    notes.into_iter().filter_map(|note| {
        let tab = *ids.get(note.tab)?;
        let t = wb.tab(tab)?;
        let row = t.rows.id_at(note.row)?;
        let col = t.cols.id_at(note.col)?;
        let address = calc_engine::CellPos::new(note.row, note.col).to_string();
        let value = calc_runner::values::answer_json(&t.value_at(note.row, note.col), None);
        let quote = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
        Some((note, tab, row, col, address, t.name.clone(), quote))
    }).collect()
}

pub(crate) async fn insert_notes(conn: &mut sqlx::PgConnection, sheet: &str, line: &str, author: uuid::Uuid, placed: Vec<PlacedNote>) -> ApiResult<()> {
    for (note, tab, row, col, address, name, quote) in placed {
        let thread = uuid::Uuid::new_v4();
        sqlx::query("insert into threads (id,sheet_id,line_id,tab_id,r1,c1,r2,c2,row_ids,col_ids,original_range,quote,tab_name,created_by) values ($1,$2,$3,$4,$5,$6,$5,$6,$7,$8,$9,$10,$11,$12)")
            .bind(thread).bind(sheet).bind(line).bind(tab as i32).bind(row as i32).bind(col as i32)
            .bind(vec![row as i32]).bind(vec![col as i32]).bind(address).bind(quote).bind(name).bind(author).execute(&mut *conn).await?;
        for (file_author, text) in std::iter::once((note.author, note.text)).chain(note.replies) {
            let body = if file_author.is_empty() {
                text
            } else {
                format!("{file_author}: {text}")
            };
            sqlx::query("insert into comments (id,thread_id,author,body) values ($1,$2,$3,$4)")
                .bind(uuid::Uuid::new_v4())
                .bind(thread)
                .bind(author)
                .bind(body)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}
