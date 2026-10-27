//! Durable, account-owned imports. No bearer tokens are stored. Upload parts are
//! immutable, retries reuse one job id, and applying data commits its receipt in
//! the same transaction. The worker's database lock is released by a crash.
use crate::{
    auth::Caller,
    code::{self, LinePath},
    error::{ApiError, ApiResult, JsonBody},
    files, lines, sheets,
    state::AppState,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    routing::{get, post},
};
use calc_engine::Workbook;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

pub const CHUNK_BYTES: usize = 1024 * 1024;
const WORKER_LOCK: i64 = 0x43414c43494d50;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub filename: String,
    pub size: usize,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub copy: Option<String>,
}
#[derive(sqlx::FromRow)]
struct Job {
    id: Uuid,
    owner: Uuid,
    caller_id: String,
    caller_kind: String,
    request: Value,
    state: String,
    result: Option<Value>,
    error: Option<Value>,
    dismissed: bool,
    attempts: i32,
    updated_at: time::OffsetDateTime,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/imports", get(list))
        .route("/imports/{id}", get(status).put(create).delete(remove))
        .route("/imports/{id}/chunks/{part}", post(chunk))
        .route("/imports/{id}/start", post(start))
        .route("/imports/{id}/retry", post(retry))
}
async fn owned(state: &AppState, caller: &Caller, id: Uuid) -> ApiResult<Job> {
    sqlx::query_as("select * from import_jobs where id=$1 and owner=$2")
        .bind(id)
        .bind(caller.uuid)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::not_found("This import"))
}
async fn answer(state: &AppState, job: Job) -> ApiResult<Value> {
    let chunks: Vec<(i32, i32)> =
        sqlx::query_as("select part,bytes from import_chunks where job_id=$1 order by part")
            .bind(job.id)
            .fetch_all(&state.db)
            .await?;
    Ok(
        json!({"id":job.id,"request":job.request,"state":job.state,"result":job.result,"error":job.error,
        "updated_at":job.updated_at.unix_timestamp() * 1000,"dismissed":job.dismissed,"chunk_size":CHUNK_BYTES,"chunks":chunks.iter().map(|c|c.0).collect::<Vec<_>>(),
        "uploaded":chunks.iter().map(|c|c.1 as i64).sum::<i64>()}),
    )
}
async fn list(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    let jobs: Vec<Job> = sqlx::query_as("select * from import_jobs where owner=$1 and not dismissed and state <> 'cancelled' order by created_at")
        .bind(caller.uuid).fetch_all(&state.db).await?;
    let mut result = Vec::with_capacity(jobs.len());
    for job in jobs {
        result.push(answer(&state, job).await?);
    }
    Ok(Json(json!({"imports":result})))
}
async fn status(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(
        answer(&state, owned(&state, &caller, id).await?).await?,
    ))
}
async fn validate_destination(
    state: &AppState,
    caller: &Caller,
    request: &ImportRequest,
) -> ApiResult<()> {
    if let Some(sheet) = &request.sheet {
        code::handle(
            state,
            Some(caller),
            &LinePath {
                sheet: sheet.clone(),
                copy: request.copy.clone(),
            },
            true,
        )
        .await?;
    } else {
        sheets::placement(state, caller, request.folder_id.as_deref()).await?;
    }
    Ok(())
}
async fn create(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
    JsonBody(request): JsonBody<ImportRequest>,
) -> ApiResult<Json<Value>> {
    if request.filename.is_empty()
        || request.filename.len() > 1024
        || request.filename.chars().any(char::is_control)
    {
        return Err(ApiError::invalid_field(
            "filename",
            "Choose a file with a valid name.",
        ));
    }
    if request.size > state.cfg.max_upload_bytes {
        return Err(ApiError::too_large(
            "The file is larger than the upload limit.",
            "Split it into smaller files.",
        ));
    }
    if request.copy.is_some() && request.sheet.is_none()
        || request.sheet.is_some() && request.folder_id.is_some()
    {
        return Err(ApiError::invalid_field(
            "sheet",
            "Choose either a new sheet or an existing sheet.",
        ));
    }
    sheets::clean_title(request.title.as_deref())?;
    validate_destination(&state, &caller, &request).await?;
    sqlx::query("insert into import_jobs(id,owner,caller_id,caller_kind,request) values($1,$2,$3,$4,$5) on conflict(id) do nothing")
        .bind(id).bind(caller.uuid).bind(&caller.id).bind(&caller.kind).bind(json!(request)).execute(&state.db).await?;
    let job = owned(&state, &caller, id).await?;
    if job.request != json!(request) {
        return Err(ApiError::conflict(
            "This import id already belongs to another file.",
            "Start a new import.",
        ));
    }
    Ok(Json(answer(&state, job).await?))
}
async fn chunk(
    State(state): State<AppState>,
    caller: Caller,
    Path((id, part)): Path<(Uuid, i32)>,
    body: Body,
) -> ApiResult<Json<Value>> {
    let job = owned(&state, &caller, id).await?;
    let request: ImportRequest = serde_json::from_value(job.request).map_err(ApiError::internal)?;
    let offset = usize::try_from(part)
        .ok()
        .and_then(|p| p.checked_mul(CHUNK_BYTES))
        .filter(|n| *n < request.size)
        .ok_or_else(|| ApiError::invalid_field("part", "This chunk is outside the file."))?;
    let bytes = to_bytes(body, CHUNK_BYTES).await.map_err(|_| {
        ApiError::bad_request(
            "The upload chunk was interrupted or too large.",
            "Retry this chunk.",
        )
    })?;
    if bytes.len() != (request.size - offset).min(CHUNK_BYTES) {
        return Err(ApiError::invalid_field(
            "size",
            "The uploaded chunk is incomplete.",
        ));
    }
    let key = state.blobs.put(&bytes).await?;
    let mut tx = state.db.begin().await?;
    let current: String =
        sqlx::query_scalar("select state from import_jobs where id=$1 for update")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if current != "uploading" {
        return Err(ApiError::conflict(
            "This import is no longer accepting uploads.",
            "Read its current progress.",
        ));
    }
    let inserted = sqlx::query("insert into import_chunks(job_id,part,bytes,blob) values($1,$2,$3,$4) on conflict(job_id,part) do update set blob=excluded.blob where import_chunks.blob=excluded.blob")
        .bind(id).bind(part).bind(bytes.len() as i32).bind(key).execute(&mut *tx).await?;
    if inserted.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "This chunk has different contents from the saved file.",
            "Start a new import for a different file.",
        ));
    }
    sqlx::query("update import_jobs set updated_at=now() where id=$1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        answer(&state, owned(&state, &caller, id).await?).await?,
    ))
}
async fn start(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let job = owned(&state, &caller, id).await?;
    let request: ImportRequest = serde_json::from_value(job.request).map_err(ApiError::internal)?;
    validate_destination(&state, &caller, &request).await?;
    let mut tx = state.db.begin().await?;
    let current: String =
        sqlx::query_scalar("select state from import_jobs where id=$1 for update")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if current == "uploading" {
        let total: i64 = sqlx::query_scalar(
            "select coalesce(sum(bytes),0)::bigint from import_chunks where job_id=$1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if total != request.size as i64 {
            return Err(ApiError::conflict(
                "Some of this file has not arrived yet.",
                "Resume the remaining upload chunks.",
            ));
        }
        sqlx::query("update import_jobs set state='queued',updated_at=now() where id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(
        answer(&state, owned(&state, &caller, id).await?).await?,
    ))
}
async fn retry(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let job = owned(&state, &caller, id).await?;
    let request: ImportRequest = serde_json::from_value(job.request).map_err(ApiError::internal)?;
    validate_destination(&state, &caller, &request).await?;
    sqlx::query("update import_jobs set state='queued',error=null,attempts=0,next_attempt_at=now(),updated_at=now() where id=$1 and state='failed'")
        .bind(id).execute(&state.db).await?;
    Ok(Json(
        answer(&state, owned(&state, &caller, id).await?).await?,
    ))
}
async fn remove(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    owned(&state, &caller, id).await?;
    let changed=sqlx::query("update import_jobs set state=case when state='completed' then state else 'cancelled' end,dismissed=true,updated_at=now() where id=$1 and state <> 'processing'")
        .bind(id).execute(&state.db).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "This import is already being processed.",
            "It will finish in the background.",
        ));
    }
    release_chunks(&state, id).await?;
    Ok(Json(
        answer(&state, owned(&state, &caller, id).await?).await?,
    ))
}
async fn release_chunks(state: &AppState, id: Uuid) -> ApiResult<()> {
    let keys: Vec<String> =
        sqlx::query_scalar("delete from import_chunks where job_id=$1 returning blob")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    sheets::collect_blobs_later(state.clone(), keys);
    Ok(())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(error) = work_once(&state).await {
                tracing::warn!(error=%error.message,"import worker will retry");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// One job per service at a time, including unfinished work from a previous process.
/// A detached connection closes on cancellation, so its advisory lock cannot leak into the pool.
pub async fn work_once(state: &AppState) -> ApiResult<()> {
    let mut lock = state.db.acquire().await?.detach();
    let acquired: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
        .bind(WORKER_LOCK)
        .fetch_one(&mut lock)
        .await?;
    if !acquired {
        return Ok(());
    }
    let job:Option<Job>=sqlx::query_as("update import_jobs set state='processing',attempts=attempts+1,updated_at=now() where id=(select id from import_jobs where state in ('queued','processing') and next_attempt_at<=now() order by created_at limit 1 for update skip locked) returning *")
        .fetch_optional(&state.db).await?;
    if let Some(job) = job {
        match process(state, &job).await {
            Ok(()) => {
                release_chunks(state, job.id).await?;
            }
            Err(error) => {
                let again = error.status.is_server_error() && job.attempts < 5;
                sqlx::query("update import_jobs set state=$2,error=$3,next_attempt_at=now()+interval '10 seconds',updated_at=now() where id=$1 and state='processing'")
                    .bind(job.id).bind(if again {"queued"}else{"failed"}).bind(json!({"message":error.message,"hint":error.hint})).execute(&state.db).await?;
            }
        }
    }
    // Recover cleanup after a crash between committing the receipt and releasing source data.
    let done:Vec<Uuid>=sqlx::query_scalar("select distinct j.id from import_jobs j join import_chunks c on c.job_id=j.id where j.state in ('completed','cancelled')")
        .fetch_all(&state.db).await?;
    for id in done {
        release_chunks(state, id).await?;
    }
    Ok(())
}
async fn complete(conn: &mut sqlx::PgConnection, id: Uuid, result: Value) -> ApiResult<()> {
    let changed=sqlx::query("update import_jobs set state='completed',result=$2,error=null,updated_at=now() where id=$1 and state='processing'")
        .bind(id).bind(result).execute(conn).await?;
    if changed.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "This import has already finished.",
            "Open its existing result.",
        ));
    }
    Ok(())
}
async fn process(state: &AppState, job: &Job) -> ApiResult<()> {
    let request: ImportRequest =
        serde_json::from_value(job.request.clone()).map_err(ApiError::internal)?;
    let caller = Caller {
        uuid: job.owner,
        id: job.caller_id.clone(),
        kind: job.caller_kind.clone(),
        token: Arc::from(""),
    };
    validate_destination(state, &caller, &request).await?;
    let keys: Vec<String> =
        sqlx::query_scalar("select blob from import_chunks where job_id=$1 order by part")
            .bind(job.id)
            .fetch_all(&state.db)
            .await?;
    let mut bytes = Vec::with_capacity(request.size);
    for key in keys {
        bytes.extend_from_slice(&state.blobs.get(&key).await?);
    }
    if bytes.len() != request.size {
        return Err(ApiError::internal(
            "durable import has missing source chunks",
        ));
    }
    let imported = files::parse_bytes(state, bytes, request.filename.clone()).await?;
    validate_destination(state, &caller, &request).await?;
    let warnings = imported.warnings.clone();
    let notes = imported.notes.clone();
    let id = job.id;
    if let Some(sheet) = request.sheet {
        let line = code::handle(
            state,
            Some(&caller),
            &LinePath {
                sheet: sheet.clone(),
                copy: request.copy.clone(),
            },
            true,
        )
        .await?;
        let capture = Arc::new(Mutex::new((Vec::new(), Vec::new())));
        let ids = capture.clone();
        let placed = capture.clone();
        let mut change = lines::task::Job::new(
            job.owner,
            lines::OpKind::Import,
            vec![
                Box::new(move |wb| {
                    let plan = imported.plan(wb, None);
                    ids.lock().unwrap().0 = plan.tabs;
                    Ok(Some(plan.op))
                }),
                Box::new(move |wb| {
                    let mut capture = placed.lock().unwrap();
                    capture.1 = files::place_notes(wb, notes, &capture.0);
                    Ok(None)
                }),
            ],
        );
        change.summary = Some(format!("imported {}", request.filename));
        let line_id = line.line_id.to_string();
        let author = job.owner;
        change.hook = Some(Box::new(move |conn, version| {
            Box::pin(async move {
                let (tabs, notes) = std::mem::take(&mut *capture.lock().unwrap());
                files::insert_notes(conn, &sheet, &line_id, author, notes).await?;
                complete(conn,id,json!({"sheet":sheet,"copy":request.copy,"tabs":tabs,"version":version,"warnings":warnings})).await
            })
        }));
        line.shared.run(change).await?;
        // Receipt is already committed. A failed index refresh must never replay the import.
        if let Err(error) = line.shared.snapshot().await {
            tracing::warn!(error=%error.message,"import search index will refresh later");
        }
    } else {
        let owner = job.owner;
        let (wb, tabs, placed) = tokio::task::spawn_blocking(move || -> ApiResult<_> {
            let mut wb = Workbook::empty();
            let plan = imported.plan(&wb, None);
            wb.apply(
                &plan.op,
                &calc_engine::ops::Actor {
                    account: &owner.to_string(),
                    at: time::OffsetDateTime::now_utc().unix_timestamp(),
                },
            )
            .map_err(ApiError::internal)?;
            let placed = files::place_notes(&wb, notes, &plan.tabs);
            Ok((wb, plan.tabs, placed))
        })
        .await
        .map_err(ApiError::internal)??;
        let default_title = std::path::Path::new(&request.filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Imported sheet");
        let title = sheets::clean_title(Some(request.title.as_deref().unwrap_or(default_title)))?;
        let (folder, owner) =
            sheets::placement(state, &caller, request.folder_id.as_deref()).await?;
        let sheet = crate::ids::sheet();
        let search = crate::search::search_text_of(&wb);
        let mut tx = state.db.begin().await?;
        sqlx::query("insert into sheets(id,title,folder_id,owner,created_by,updated_by,search_text) values($1,$2,$3,$4,$5,$5,$6)")
            .bind(&sheet).bind(title).bind(folder).bind(owner).bind(job.owner).bind(search).execute(&mut *tx).await?;
        lines::create_main_line(state, &mut tx, &sheet, job.owner, &wb).await?;
        files::insert_notes(
            &mut tx,
            &sheet,
            &crate::ids::main_line(&sheet),
            job.owner,
            placed,
        )
        .await?;
        complete(
            &mut tx,
            id,
            json!({"sheet":sheet,"copy":null,"tabs":tabs,"version":0,"warnings":warnings}),
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}
