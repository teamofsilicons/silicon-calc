//! Materialize results from JavaScript executed in the browser; never evaluate submitted code.
use crate::{
    auth::Caller,
    error::{ApiError, ApiResult, JsonBody},
    lines::{self, ApplyRequest, LineHandle, OpKind},
    perms,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    routing::post,
};
use calc_engine::{CellPos, Op, TabId, Workbook, model::CodeSource};
use calc_runner::{OutColumn, RunError, RunOutput, Runner, RunnerConfig, SandboxMode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Mutex, time::Duration};

#[derive(Clone, Deserialize)]
pub(crate) struct LinePath {
    pub sheet: String,
    pub copy: Option<String>,
}

pub(crate) fn both(
    router: Router<AppState>,
    suffix: &str,
    method: axum::routing::MethodRouter<AppState>,
) -> Router<AppState> {
    router
        .route(&format!("/sheets/{{sheet}}{suffix}"), method.clone())
        .route(
            &format!("/sheets/{{sheet}}/copies/{{copy}}{suffix}"),
            method,
        )
}

pub fn router() -> Router<AppState> {
    both(Router::new(), "/run", post(run))
}

pub(crate) fn runner(state: &AppState) -> Runner {
    let python = if state.cfg.runner_python == "python3" {
        let local = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.dev/runner-venv/bin/python");
        if local.exists() {
            local.to_string_lossy().into_owned()
        } else {
            state.cfg.runner_python.clone()
        }
    } else {
        state.cfg.runner_python.clone()
    };
    let mut config = RunnerConfig::new(
        python,
        SandboxMode::parse(&state.cfg.code_sandbox).unwrap_or(SandboxMode::Auto),
    );
    config.limits.timeout = state.cfg.code_timeout.min(Duration::from_secs(60));
    Runner::new(config)
}

pub(crate) fn runner_error(error: RunError) -> ApiError {
    ApiError::unprocessable("code_failed", error.message, error.hint)
        .with_details(json!({"stderr":error.stderr,"stdout":error.stdout,"line":error.line}))
}

pub(crate) async fn handle(
    state: &AppState,
    caller: Option<&Caller>,
    path: &LinePath,
    edit: bool,
) -> ApiResult<LineHandle> {
    let access = perms::sheet_access(state, caller, &path.sheet).await?;
    if edit {
        access.require_edit()?;
    }
    let row = lines::line_for(state, &path.sheet, path.copy.as_deref()).await?;
    if edit && row.status != "open" {
        return Err(ApiError::conflict(
            "This copy is closed.",
            "Make a new copy to keep editing.",
        ));
    }
    state.lines.open(state, &row.id).await
}

pub(crate) fn tab_id(wb: &Workbook, value: &Value) -> ApiResult<TabId> {
    let id = value
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()));
    id.filter(|id| wb.tab(*id).is_some())
        .or_else(|| value.as_str().and_then(|s| wb.tab_by_name(s).map(|t| t.id)))
        .ok_or_else(|| ApiError::not_found("That tab"))
}

/// Browser JavaScript is never evaluated by this service. This request carries only
/// its materialized result and the exact sheet version the browser read.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    code: String,
    base_version: i64,
    inputs: Vec<TabId>,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    into: Destination,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Destination {
    new_tab: Option<String>,
    tab: Option<Value>,
    at: Option<String>,
    #[serde(default)]
    replace: bool,
}

const MAX_RESULT_BYTES: usize = 16 << 20;

fn validate(request: &Request) -> ApiResult<()> {
    if request.base_version < 0 {
        return Err(ApiError::invalid_field(
            "base_version",
            "Use the version of the input snapshot.",
        ));
    }
    if request.code.len() > 1 << 20 {
        return Err(ApiError::too_large(
            "The JavaScript source is over 1 MB.",
            "Use a smaller command.",
        ));
    }
    let width = request.columns.len();
    if width == 0 || width > calc_engine::addr::MAX_COLS as usize {
        return Err(ApiError::invalid_field(
            "columns",
            "Provide one or more result column names within the grid width.",
        ));
    }
    let cells = width
        .checked_mul(request.rows.len().saturating_add(1))
        .unwrap_or(usize::MAX);
    if cells > crate::config::MAX_CELLS_PER_EDIT {
        return Err(ApiError::too_large(
            format!(
                "This result covers {cells} cells; one result can contain at most {} including its header.",
                crate::config::MAX_CELLS_PER_EDIT
            ),
            "Return fewer rows or columns.",
        ));
    }
    if request.inputs.len() > 10_000 {
        return Err(ApiError::invalid_field("inputs", "Too many input tabs."));
    }
    let mut bytes = request.columns.iter().map(String::len).sum::<usize>();
    for row in &request.rows {
        if row.len() != width {
            return Err(ApiError::invalid_field(
                "rows",
                "Every result row must match the columns width.",
            ));
        }
        for value in row {
            bytes = bytes.saturating_add(match value {
                Value::Null | Value::Bool(_) => 8,
                Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => 24,
                Value::String(s) => s.len(),
                _ => return Err(ApiError::invalid_field("rows", "Result cells must be null, booleans, finite numbers or strings. Dates must be ISO strings.")),
            });
        }
    }
    if bytes > MAX_RESULT_BYTES {
        return Err(ApiError::too_large(
            "The result data is over 16 MB.",
            "Return smaller strings or fewer rows.",
        ));
    }
    let into = &request.into;
    if into.new_tab.is_some() == into.tab.is_some()
        || (into.new_tab.is_some() && (into.at.is_some() || into.replace))
        || (into.replace && into.at.as_deref().is_some_and(|s| s != "A1"))
    {
        return Err(ApiError::invalid_field(
            "into",
            "Choose new_tab, or tab with an optional at address. replace starts at A1.",
        ));
    }
    Ok(())
}

async fn run(
    State(state): State<AppState>,
    caller: Caller,
    Path(path): Path<LinePath>,
    JsonBody(request): JsonBody<Request>,
) -> ApiResult<Json<Value>> {
    let line = handle(&state, Some(&caller), &path, true).await?;
    validate(&request)?;
    let output = RunOutput {
        columns: request
            .columns
            .iter()
            .enumerate()
            .map(|(c, name)| OutColumn {
                name: name.clone(),
                format: None,
                values: request
                    .rows
                    .iter()
                    .map(|row| match &row[c] {
                        Value::Null => calc_engine::Value::Empty,
                        Value::Bool(v) => calc_engine::Value::Bool(*v),
                        Value::Number(v) => {
                            calc_engine::Value::Number(v.as_f64().expect("validated finite number"))
                        }
                        Value::String(v) => calc_engine::Value::text(v.as_str()),
                        _ => unreachable!("validated scalar cell"),
                    })
                    .collect(),
            })
            .collect(),
        row_count: request.rows.len() as u64,
        has_result: true,
        ..Default::default()
    };
    let mut answer = json!({
        "columns": request.columns,
        "rows": request.rows.iter().take(1000).collect::<Vec<_>>(),
        "row_count": request.rows.len(), "truncated": request.rows.len() > 1000,
        "stdout": "", "stderr": "", "duration_ms": 0, "version": null, "tab": null
    });
    let source = CodeSource {
        language: "javascript".into(),
        code: request.code,
        inputs: request.inputs,
        run_by: caller.uuid.to_string(),
        run_at: time::OffsetDateTime::now_utc().unix_timestamp(),
        keep_updated: false,
    };
    let into = request.into;
    let target = std::sync::Arc::new(Mutex::new(None));
    let chosen = target.clone();
    let result = line
        .apply_at_version(
            ApplyRequest {
                author: caller.uuid,
                kind: OpKind::Code,
                summary: Some("saved JavaScript command result".into()),
                base_version: Some(request.base_version),
                client_id: None,
                build: Box::new(move |wb| {
                    if source.inputs.iter().any(|id| wb.tab(*id).is_none()) {
                        return Err(ApiError::invalid_field(
                            "inputs",
                            "Every input must name a tab in the input snapshot.",
                        ));
                    }
                    if let Some(name) = into.new_tab {
                        *chosen.lock().unwrap() = Some(wb.next_tab_id);
                        return Ok(Some(wb.prepare_new_tab_with(
                            &name,
                            None,
                            output.tab_content(Some(source)),
                        )));
                    }
                    let tab = tab_id(wb, into.tab.as_ref().expect("validated destination"))?;
                    *chosen.lock().unwrap() = Some(tab);
                    if into.replace {
                        return Ok(Some(Op::ReplaceTab {
                            tab,
                            content: Box::new(output.tab_content(Some(source))),
                        }));
                    }
                    let pos =
                        CellPos::parse(into.at.as_deref().unwrap_or("A1")).ok_or_else(|| {
                            ApiError::invalid_field("at", "Choose a cell address such as A1.")
                        })?;
                    if u64::from(pos.row) + output.kept_rows() as u64 + 1
                        > u64::from(calc_engine::addr::MAX_ROWS)
                        || u64::from(pos.col) + output.columns.len() as u64
                            > u64::from(calc_engine::addr::MAX_COLS)
                    {
                        return Err(ApiError::invalid_field(
                            "into",
                            "The result would extend past the grid.",
                        ));
                    }
                    Ok(Some(wb.prepare_writes(
                        tab,
                        output.cell_writes(pos.row, pos.col),
                    )?))
                }),
            },
            request.base_version,
        )
        .await?;
    let id = *target.lock().unwrap();
    answer["version"] = json!(result.version);
    answer["tab"] = line
        .read(move |wb, _| {
            wb.tabs
                .iter()
                .enumerate()
                .find(|(_, t)| Some(t.id) == id)
                .map(|(i, t)| lines::tab_json(wb, t, i))
                .unwrap_or(Value::Null)
        })
        .await?;
    lines::resolve_tab_accounts(&state, &mut answer["tab"]).await;
    Ok(Json(answer))
}
