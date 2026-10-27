mod support;
use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{Harness, TestAccount};
use uuid::Uuid;

async fn new_job(
    h: &Harness,
    who: &TestAccount,
    name: &str,
    size: usize,
    sheet: Option<&str>,
) -> String {
    let id = Uuid::new_v4().to_string();
    let (status, body) = h
        .put(
            who,
            &format!("/v1/imports/{id}"),
            json!({"filename":name,"size":size,"sheet":sheet}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    id
}
async fn part(
    h: &Harness,
    who: &TestAccount,
    id: &str,
    n: usize,
    bytes: Vec<u8>,
) -> (StatusCode, Value) {
    h.upload(who, &format!("/v1/imports/{id}/chunks/{n}"), bytes)
        .await
}
async fn wait(h: &Harness, who: &TestAccount, id: &str, state: &str) -> Value {
    for _ in 0..200 {
        let (_, body) = h.get(who, &format!("/v1/imports/{id}")).await;
        if body["state"] == state {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!(
        "job never reached {state}: {:?}",
        h.get(who, &format!("/v1/imports/{id}")).await
    );
}
async fn hold_worker(h: &Harness) -> sqlx::PgConnection {
    let mut conn = h.state.db.acquire().await.unwrap().detach();
    sqlx::query("select pg_advisory_lock($1)")
        .bind(0x43414c43494d50_i64)
        .execute(&mut conn)
        .await
        .unwrap();
    conn
}
#[tokio::test]
async fn chunks_survive_restart_and_receipts_prevent_duplicate_sheets() {
    let h = Harness::start().await;
    let a = h.carbon("c:importer", "Importer");
    let b = h.carbon("c:other", "Other");
    let lock = hold_worker(&h).await;
    let mut bytes = b"value\n".to_vec();
    bytes.extend("7\n".repeat(550000).bytes());
    let id = new_job(&h, &a, "recover.csv", bytes.len(), None).await;
    assert_eq!(
        h.get(&b, &format!("/v1/imports/{id}")).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let size = calc_api::imports::CHUNK_BYTES;
    assert_eq!(
        part(&h, &a, &id, 0, bytes[..size].to_vec()).await.0,
        StatusCode::OK
    );
    // An acknowledged part can be retried, but never replaced with different bytes.
    assert_eq!(
        part(&h, &a, &id, 0, bytes[..size].to_vec()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        part(&h, &a, &id, 0, vec![b'x'; size]).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        part(&h, &b, &id, 1, bytes[size..].to_vec()).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        part(&h, &a, &id, 1, bytes[size..].to_vec()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
            .await
            .0,
        StatusCode::OK
    );
    // A previous process died after claiming work. A new state/registry recovers the saved job.
    sqlx::query("update import_jobs set state='processing' where id=$1")
        .bind(Uuid::parse_str(&id).unwrap())
        .execute(&h.state.db)
        .await
        .unwrap();
    drop(lock);
    let mut restarted = h.state.clone();
    restarted.lines = calc_api::lines::Registry::new();
    calc_api::imports::work_once(&restarted).await.unwrap();
    let done = wait(&h, &a, &id, "completed").await;
    let finished_at = done["updated_at"].as_i64().unwrap();
    assert!((time::OffsetDateTime::now_utc().unix_timestamp() * 1000 - finished_at).abs() < 10_000);
    assert_eq!(
        h.get(&a, &format!("/v1/imports/{id}")).await.1["updated_at"],
        done["updated_at"]
    );
    let sheet = done["result"]["sheet"].as_str().unwrap();
    let (_, cells) = h
        .get(
            &a,
            &format!("/v1/sheets/{sheet}/tabs/1/cells?range=A1:A3&format=values"),
        )
        .await;
    assert_eq!(cells["values"], json!([["value"], [7.0], [7.0]]));
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    h.post(&a, &format!("/v1/imports/{id}/retry"), json!({}))
        .await;
    calc_api::imports::work_once(&restarted).await.unwrap();
    assert_eq!(
        h.get(&a, &format!("/v1/imports/{id}")).await.1["result"],
        done["result"]
    );
    let count: i64 = sqlx::query_scalar("select count(*) from sheets where created_by=$1")
        .bind(a.uuid)
        .fetch_one(&h.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let (_, other) = h.get(&b, "/v1/imports").await;
    assert_eq!(other["imports"], json!([]));
}
#[tokio::test]
async fn completion_receipt_and_tab_changes_roll_back_together() {
    let h = Harness::start().await;
    let a = h.carbon("c:importer", "Importer");
    let (_, sheet) = h
        .post(&a, "/v1/sheets", json!({"title":"Keep editing"}))
        .await;
    let sheet = sheet["id"].as_str().unwrap();
    let lock = hold_worker(&h).await;
    let bytes = b"name,value\nOne,12\n";
    let id = new_job(&h, &a, "atomic.csv", bytes.len(), Some(sheet)).await;
    part(&h, &a, &id, 0, bytes.to_vec()).await;
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    sqlx::raw_sql("create function fail_import_receipt() returns trigger language plpgsql as $$ begin if new.state='completed' then raise exception 'simulated transaction failure'; end if; return new; end $$; create trigger fail_receipt before update on import_jobs for each row execute function fail_import_receipt();").execute(&h.state.db).await.unwrap();
    drop(lock);
    calc_api::imports::work_once(&h.state).await.unwrap();
    let queued = wait(&h, &a, &id, "queued").await;
    assert!(queued["error"].is_object());
    let (_, tabs) = h.get(&a, &format!("/v1/sheets/{sheet}/tabs")).await;
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 1);
    sqlx::raw_sql("drop trigger fail_receipt on import_jobs; drop function fail_import_receipt(); update import_jobs set next_attempt_at=now();").execute(&h.state.db).await.unwrap();
    calc_api::imports::work_once(&h.state).await.unwrap();
    let done = wait(&h, &a, &id, "completed").await;
    assert_eq!(done["result"]["version"], 1);
    let (_, tabs) = h.get(&a, &format!("/v1/sheets/{sheet}/tabs")).await;
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2);
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    calc_api::imports::work_once(&h.state).await.unwrap();
    let (_, tabs) = h.get(&a, &format!("/v1/sheets/{sheet}/tabs")).await;
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2);
}
#[tokio::test]
async fn invalid_files_fail_without_sheets_and_cancelled_uploads_stay_cancelled() {
    let h = Harness::start().await;
    let a = h.carbon("c:importer", "Importer");
    let id = new_job(&h, &a, "broken.xlsx", 6, None).await;
    part(&h, &a, &id, 0, b"broken".to_vec()).await;
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    wait(&h, &a, &id, "failed").await;
    let count: i64 = sqlx::query_scalar("select count(*) from sheets where created_by=$1")
        .bind(a.uuid)
        .fetch_one(&h.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let id = new_job(&h, &a, "cancel.csv", 4, None).await;
    part(&h, &a, &id, 0, b"x\n1\n".to_vec()).await;
    assert_eq!(
        h.delete(&a, &format!("/v1/imports/{id}")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        part(&h, &a, &id, 0, b"x\n1\n".to_vec()).await.0,
        StatusCode::CONFLICT
    );
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    assert_eq!(
        h.get(&a, &format!("/v1/imports/{id}")).await.1["state"],
        "cancelled"
    );
}
#[tokio::test]
async fn destination_permissions_are_rechecked_before_processing() {
    let h = Harness::start().await;
    let a = h.carbon("c:importer", "Importer");
    let (_, sheet) = h.post(&a, "/v1/sheets", json!({})).await;
    let sheet = sheet["id"].as_str().unwrap();
    let lock = hold_worker(&h).await;
    let id = new_job(&h, &a, "late.csv", 4, Some(sheet)).await;
    part(&h, &a, &id, 0, b"x\n1\n".to_vec()).await;
    h.post(&a, &format!("/v1/imports/{id}/start"), json!({}))
        .await;
    h.delete(&a, &format!("/v1/sheets/{sheet}")).await;
    drop(lock);
    calc_api::imports::work_once(&h.state).await.unwrap();
    wait(&h, &a, &id, "failed").await;
}
