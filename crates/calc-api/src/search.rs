//! Search: `GET /v1/search`, over the titles and cell text of every sheet the caller can read (owned, or shared
//! with them directly or through a folder; not public sheets they merely have the link to, and nothing in the
//! trash). See docs/API.md, Search.
//!
//! Cell text comes from `sheets.search_text`, which the lines module refreshes with each snapshot through
//! `set_search_text(…, &search_text_of(&workbook))`. Its format is one line per distinct text in a tab:
//! `{tab id}\t{tab name}\t{A1 address}\t{shown text}`. A line without that shape is matched as plain text.

use std::{collections::HashSet, sync::Arc};

use axum::{Json, Router, extract::State, routing::get};
use calc_engine::{CellPos, Value as CellValue, Workbook};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    auth::Caller,
    db,
    error::{ApiError, ApiResult, QueryParams},
    perms::{SHEET_COLUMNS, SheetRow},
    sheets,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/search", get(search))
}

/// The most text one sheet keeps for search.
pub const SEARCH_TEXT_MAX_BYTES: usize = 1 << 20;
/// The longest piece of one cell's text that is kept.
const CELL_TEXT_MAX_CHARS: usize = 200;
/// How many cells are looked at, at most, per sheet (the first rows of every tab come first).
const CELLS_VISITED_MAX: usize = 5_000_000;
/// Matches shown per sheet.
const MATCHES_PER_SHEET: i64 = 20;

fn one_line(s: &str, max: usize) -> String {
    s.chars().take(max).map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

/// The search text of a workbook: for each tab, each distinct piece of text a cell holds (or a formula shows), with
/// where it first appears, row by row, up to `SEARCH_TEXT_MAX_BYTES`.
pub fn search_text_of(wb: &Workbook) -> String {
    let mut out = String::new();
    let mut visited = 0usize;
    'tabs: for tab in &wb.tabs {
        let name = one_line(&tab.name, 100);
        let mut seen: HashSet<Arc<str>> = HashSet::new();
        let (rows, cols) = (tab.rows.len(), tab.cols.len());
        for r in 0..rows {
            for c in 0..cols {
                visited += 1;
                if visited > CELLS_VISITED_MAX {
                    break 'tabs;
                }
                let CellValue::Text(text) = tab.value_at(r, c) else { continue };
                if text.trim().is_empty() || !seen.insert(text.clone()) {
                    continue;
                }
                let line = format!("{}\t{name}\t{}\t{}\n", tab.id, CellPos::new(r, c), one_line(&text, CELL_TEXT_MAX_CHARS));
                if out.len() + line.len() > SEARCH_TEXT_MAX_BYTES {
                    break 'tabs;
                }
                out.push_str(&line);
            }
        }
    }
    out
}

/// Stores a sheet's search text (see `search_text_of`). Contract: called by `lines` when it snapshots a main line.
pub async fn set_search_text(state: &AppState, sheet_id: &str, text: &str) -> ApiResult<()> {
    let mut end = text.len().min(SEARCH_TEXT_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    sqlx::query("update sheets set search_text = $2 where id = $1 and deleted_at is null")
        .bind(sheet_id)
        .bind(&text[..end])
        .execute(&state.db)
        .await?;
    Ok(())
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
    limit: Option<i64>,
}

async fn search(State(state): State<AppState>, caller: Caller, QueryParams(q): QueryParams<SearchQuery>) -> ApiResult<Json<Value>> {
    let Some(text) = q.q.as_deref().map(str::trim).filter(|t| !t.is_empty()) else {
        return Err(ApiError::invalid_field("q", "Give something to search for, like ?q=revenue."));
    };
    if text.chars().count() > 200 {
        return Err(ApiError::invalid_field("q", "Search for at most 200 characters at a time."));
    }
    let limit = q.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(ApiError::invalid_field("limit", "limit must be between 1 and 100."));
    }
    let ids = crate::perms::readable_sheet_ids(&state, caller.uuid).await?;
    if ids.is_empty() {
        return Ok(Json(json!({"results": []})));
    }
    let pattern = db::like_anywhere(text);
    // Candidates: the title matches, or the search text has the words somewhere (checked per cell below).
    let cols = sheets::prefixed(SHEET_COLUMNS, "s");
    let candidates: Vec<SheetRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "select {cols} from sheets s where s.id = any($1) and (s.title ilike $2 or s.search_text ilike $2)
         order by (s.title ilike $2) desc, s.updated_at desc, s.id limit 500"
    )))
    .bind(&ids)
    .bind(&pattern)
    .fetch_all(&state.db)
    .await?;
    let candidate_ids: Vec<String> = candidates.iter().map(|s| s.id.clone()).collect();
    let lines: Vec<(String, String)> = sqlx::query_as(
        "select s.id, m.line from sheets s
         cross join lateral (
             select line from regexp_split_to_table(s.search_text, E'\\n') as line
             where line <> ''
               and (case when line like E'%\\t%\\t%\\t%' then split_part(line, E'\\t', 4) else line end) ilike $2
             limit $3
         ) m
         where s.id = any($1)",
    )
    .bind(&candidate_ids)
    .bind(&pattern)
    .bind(MATCHES_PER_SHEET)
    .fetch_all(&state.db)
    .await?;
    let lower = text.to_lowercase();
    let mut chosen: Vec<(SheetRow, Vec<Value>)> = Vec::new();
    for sheet in candidates {
        let matches: Vec<Value> = lines.iter().filter(|(id, _)| *id == sheet.id).map(|(_, line)| match_json(line)).collect();
        let title_hit = sheet.title.to_lowercase().contains(&lower);
        if !title_hit && matches.is_empty() {
            continue;
        }
        chosen.push((sheet, matches));
        if chosen.len() as i64 >= limit {
            break;
        }
    }
    let rows: Vec<SheetRow> = chosen.iter().map(|(s, _)| s.clone()).collect();
    let sheets_json = sheets::sheet_list_json(&state, Some(caller.uuid), &rows).await?;
    let mut by_id: std::collections::HashMap<String, Value> =
        sheets_json.into_iter().filter_map(|v| v["id"].as_str().map(|id| (id.to_owned(), v.clone()))).collect();
    let results: Vec<Value> = chosen
        .into_iter()
        .filter_map(|(s, matches)| by_id.remove(&s.id).map(|sheet| json!({"sheet": sheet, "matches": matches})))
        .collect();
    Ok(Json(json!({"results": results})))
}

fn match_json(line: &str) -> Value {
    let parts: Vec<&str> = line.splitn(4, '\t').collect();
    if let [tab, tab_name, a, text] = parts[..]
        && let Ok(tab) = tab.parse::<u32>()
    {
        return json!({"tab": tab, "tab_name": tab_name, "a": a, "text": text});
    }
    json!({"tab": null, "tab_name": null, "a": null, "text": line})
}
