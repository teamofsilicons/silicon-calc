//! Sheets: meta, me, account lookups, creating, reading, listing (mine, shared, trash, paging), renaming, moving,
//! the trash, restoring and deleting forever.

mod platform_support;
mod support;

use std::time::Duration;

use calc_api::blobs::Removal;
use platform_support::*;
use serde_json::json;

#[tokio::test]
async fn meta_me_and_account_lookups() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let analyst = h.silicon("si:analyst", "Analyst");

    let meta = h.get_anon("/v1/meta").await;
    let meta = expect(&meta, 200);
    assert_eq!(meta["app_id"], json!("calc"));
    assert_eq!(meta["version"], json!("0.1.0"));
    assert_eq!(meta["accounts_url"], json!(h.accounts.url));
    assert_eq!(meta["limits"]["max_rows"], json!(10_485_760));
    assert_eq!(meta["limits"]["max_cols"], json!(16_384));
    assert_eq!(meta["limits"]["max_upload_bytes"], json!(524_288_000));
    assert_eq!(meta["limits"]["max_cells_per_edit"], json!(200_000));
    assert_eq!(meta["limits"]["code_timeout_seconds"], json!(60));

    let me = h.get(&ada, "/v1/me").await;
    let me = expect(&me, 200);
    assert_eq!(me["account"]["id"], json!("c:ada"));
    assert_eq!(me["account"]["kind"], json!("carbon"));
    assert_eq!(me["account"]["display_name"], json!("Ada"));
    assert_eq!(me["account"]["uuid"], json!(ada.uuid));
    let me = h.get(&analyst, "/v1/me").await;
    assert_eq!(expect(&me, 200)["account"]["kind"], json!("silicon"));

    expect_error(&h.get_anon("/v1/me").await, 401, "signed_out");
    let r = h.http.get(format!("{}/v1/me", h.url)).bearer_auth("not-a-token").send().await.expect("request");
    assert_eq!(r.status().as_u16(), 401);

    let found = h.get(&ada, "/v1/accounts/lookup?id=si:analyst").await;
    let found = expect(&found, 200);
    assert_eq!(found["id"], json!("si:analyst"));
    assert_eq!(found["uuid"], json!(analyst.uuid));
    assert_eq!(found["kind"], json!("silicon"));
    let upper = h.get(&ada, "/v1/accounts/lookup?id=SI:Analyst").await;
    assert_eq!(expect(&upper, 200)["uuid"], json!(analyst.uuid));
    let e = expect_error(&h.get(&ada, "/v1/accounts/lookup?id=c:nobody").await, 404, "not_found");
    assert!(e["message"].as_str().unwrap().contains("c:nobody"));
    expect_error(&h.get(&ada, "/v1/accounts/lookup").await, 422, "invalid_field");
    expect_error(&h.get(&ada, "/v1/accounts/lookup?id=ada").await, 422, "invalid_field");
    expect_error(&h.get_anon("/v1/accounts/lookup?id=c:ada").await, 401, "signed_out");
}

#[tokio::test]
async fn creating_and_reading_a_sheet() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");

    let s = new_sheet(&h, &ada, "", None).await;
    let id = s["id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 12);
    assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
    assert_eq!(s["title"], json!("Untitled sheet"));
    assert_eq!(s["role"], json!("owner"));
    assert_eq!(s["owner"]["id"], json!("c:ada"));
    assert_eq!(s["created_by"]["id"], json!("c:ada"));
    assert_eq!(s["updated_by"]["id"], json!("c:ada"));
    assert_eq!(s["folder_id"], json!(null));
    assert_eq!(s["public"], json!({"enabled": false, "access": "view"}));
    assert_eq!(s["version"], json!(0));
    assert_eq!(s["copies"], json!(0));
    assert_eq!(s["trashed_at"], json!(null));
    let tabs = s["tabs"].as_array().expect("tabs");
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["name"], json!("Tab 1"));
    assert_eq!(tabs[0]["index"], json!(0));
    assert_eq!(tabs[0]["rows"], json!(1000));
    assert_eq!(tabs[0]["cols"], json!(26));

    // No body at all is an untitled sheet too; titles are trimmed.
    let r = h.http.post(format!("{}/v1/sheets", h.url)).bearer_auth(&ada.token).send().await.expect("request");
    assert_eq!(r.status().as_u16(), 201);
    let q3 = new_sheet(&h, &ada, "  Q3 revenue  ", None).await;
    assert_eq!(q3["title"], json!("Q3 revenue"));

    let long = "x".repeat(301);
    let e = expect_error(&h.post(&ada, "/v1/sheets", json!({"title": long})).await, 422, "invalid_field");
    assert!(e["details"]["fields"]["title"].is_string());
    expect_error(&h.post(&ada, "/v1/sheets", json!({"title": "two\nlines"})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, "/v1/sheets", json!({"title": 12})).await, 422, "invalid_field");
    let bad = h.http.post(format!("{}/v1/sheets", h.url)).bearer_auth(&ada.token).body("{nope").send().await.expect("request");
    assert_eq!(bad.status().as_u16(), 400);
    expect_error(&h.post(&ada, "/v1/sheets", json!({"folder_id": "fnotafolder1"})).await, 422, "invalid_field");
    let anon = h.http.post(format!("{}/v1/sheets", h.url)).json(&json!({})).send().await.expect("request");
    assert_eq!(anon.status().as_u16(), 401);

    let got = h.get(&ada, &format!("/v1/sheets/{id}")).await;
    let got = expect(&got, 200);
    assert_eq!(got["id"], json!(id));
    assert_eq!(got["tabs"][0]["name"], json!("Tab 1"));

    // Nobody else can tell it exists.
    expect_error(&h.get(&bob, &format!("/v1/sheets/{id}")).await, 404, "not_found");
    expect_error(&h.get_anon(&format!("/v1/sheets/{id}")).await, 401, "signed_out");
    expect_error(&h.get(&ada, "/v1/sheets/nope").await, 404, "not_found");
    expect_error(&h.get(&ada, "/v1/sheets/abcdefghijkm").await, 404, "not_found");
}

#[tokio::test]
async fn listing_mine_shared_trash_and_pages() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");

    let mut made = Vec::new();
    for i in 0..5 {
        made.push(new_sheet_id(&h, &ada, &format!("Budget {i}"), None).await);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let folder = new_folder(&h, &ada, "Finance", None).await;
    let inside = new_sheet_id(&h, &ada, "Budget in a folder", Some(&folder)).await;

    let list = h.get(&ada, "/v1/sheets").await;
    let list = expect(&list, 200);
    let newest_first: Vec<String> = made.iter().rev().cloned().collect();
    assert_eq!(ids(&list["sheets"]), newest_first, "the top level, newest first");
    assert_eq!(ids(&list["folders"]), vec![folder.clone()]);
    assert_eq!(list["next_cursor"], json!(null));
    assert_eq!(list["sheets"][0]["role"], json!("owner"));
    assert!(list["sheets"][0].get("tabs").is_none(), "lists leave the tabs out");

    // Pages of two, each sheet once.
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..5 {
        let path = match &cursor {
            Some(c) => format!("/v1/sheets?scope=mine&limit=2&cursor={c}"),
            None => "/v1/sheets?scope=mine&limit=2".to_owned(),
        };
        let page = h.get(&ada, &path).await;
        let page = expect(&page, 200).clone();
        seen.extend(ids(&page["sheets"]));
        if cursor.is_some() {
            assert_eq!(page["folders"], json!([]), "folders come with the first page");
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen, newest_first);

    // Searching titles looks inside folders too.
    let found = h.get(&ada, "/v1/sheets?q=budget").await;
    let mut found = ids(&expect(&found, 200)["sheets"]);
    found.sort();
    let mut all = made.clone();
    all.push(inside.clone());
    all.sort();
    assert_eq!(found, all);
    let one = h.get(&ada, "/v1/sheets?q=in%20a%20folder").await;
    assert_eq!(ids(&expect(&one, 200)["sheets"]), vec![inside.clone()]);
    let pct = h.get(&ada, "/v1/sheets?q=%25").await;
    assert_eq!(ids(&expect(&pct, 200)["sheets"]), Vec::<String>::new(), "% is matched literally");
    let in_folder = h.get(&ada, &format!("/v1/sheets?folder={folder}")).await;
    assert_eq!(ids(&expect(&in_folder, 200)["sheets"]), vec![inside.clone()]);

    expect_error(&h.get(&ada, "/v1/sheets?scope=everything").await, 422, "invalid_field");
    expect_error(&h.get(&ada, "/v1/sheets?limit=0").await, 422, "invalid_field");
    expect_error(&h.get(&ada, "/v1/sheets?limit=501").await, 422, "invalid_field");
    expect_error(&h.get(&ada, "/v1/sheets?cursor=garbage").await, 422, "invalid_field");
    expect_error(&h.get(&ada, "/v1/sheets?limit=abc").await, 400, "invalid_request");
    expect_error(&h.get(&bob, &format!("/v1/sheets?folder={folder}")).await, 404, "not_found");

    // Shared with Bob: in his shared list, not in his own.
    share(&h, &ada, &format!("sheets/{}", made[0]), &["c:bob"], "commenter").await;
    let shared = h.get(&bob, "/v1/sheets?scope=shared").await;
    let shared = expect(&shared, 200);
    assert_eq!(ids(&shared["sheets"]), vec![made[0].clone()]);
    assert_eq!(shared["sheets"][0]["role"], json!("commenter"));
    assert_eq!(shared["sheets"][0]["owner"]["id"], json!("c:ada"));
    let mine = h.get(&bob, "/v1/sheets").await;
    assert_eq!(ids(&expect(&mine, 200)["sheets"]), Vec::<String>::new());
    let adas_shared = h.get(&ada, "/v1/sheets?scope=shared").await;
    assert_eq!(ids(&expect(&adas_shared, 200)["sheets"]), Vec::<String>::new());

    // The trash: only there, newest first, and no longer anywhere else.
    expect(&h.delete(&ada, &format!("/v1/sheets/{}", made[1])).await, 204);
    tokio::time::sleep(Duration::from_millis(5)).await;
    expect(&h.delete(&ada, &format!("/v1/sheets/{}", made[2])).await, 204);
    let trash = h.get(&ada, "/v1/sheets?scope=trash").await;
    let trash = expect(&trash, 200);
    assert_eq!(ids(&trash["sheets"]), vec![made[2].clone(), made[1].clone()]);
    assert!(trash["sheets"][0]["trashed_at"].is_string());
    let mine = h.get(&ada, "/v1/sheets").await;
    let mine = ids(&expect(&mine, 200)["sheets"]);
    assert!(!mine.contains(&made[1]) && !mine.contains(&made[2]));
    let page = h.get(&ada, "/v1/sheets?scope=trash&limit=1").await;
    let page = expect(&page, 200).clone();
    assert_eq!(ids(&page["sheets"]), vec![made[2].clone()]);
    let next = page["next_cursor"].as_str().expect("a second page");
    let page2 = h.get(&ada, &format!("/v1/sheets?scope=trash&limit=1&cursor={next}")).await;
    assert_eq!(ids(&expect(&page2, 200)["sheets"]), vec![made[1].clone()]);
    let bobs_trash = h.get(&bob, "/v1/sheets?scope=trash").await;
    assert_eq!(ids(&expect(&bobs_trash, 200)["sheets"]), Vec::<String>::new());
}

#[tokio::test]
async fn renaming_follows_the_roles() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let cy = h.carbon("c:cy", "Cy");
    let analyst = h.silicon("si:analyst", "Analyst");
    let id = new_sheet_id(&h, &ada, "Q3", None).await;
    let item = format!("sheets/{id}");
    share(&h, &ada, &item, &["si:analyst"], "editor").await;
    share(&h, &ada, &item, &["c:bob"], "commenter").await;
    share(&h, &ada, &item, &["c:cy"], "viewer").await;

    // A Silicon editor renames it like any editor.
    let r = h.patch(&analyst, &format!("/v1/sheets/{id}"), json!({"title": "Q3 revenue"})).await;
    let r = expect(&r, 200);
    assert_eq!(r["title"], json!("Q3 revenue"));
    assert_eq!(r["role"], json!("editor"));
    assert_eq!(r["updated_by"]["id"], json!("si:analyst"));
    assert_eq!(r["owner"]["id"], json!("c:ada"));

    let e = expect_error(&h.patch(&bob, &format!("/v1/sheets/{id}"), json!({"title": "Mine now"})).await, 403, "not_allowed");
    assert!(e["message"].as_str().unwrap().contains("commenter"));
    expect_error(&h.patch(&cy, &format!("/v1/sheets/{id}"), json!({"title": "Mine now"})).await, 403, "not_allowed");
    expect_error(&h.patch(&ada, &format!("/v1/sheets/{id}"), json!({"title": "   "})).await, 422, "invalid_field");
    // Nothing to change answers the sheet as it is, for any role.
    let same = h.patch(&cy, &format!("/v1/sheets/{id}"), json!({})).await;
    assert_eq!(expect(&same, 200)["title"], json!("Q3 revenue"));
    assert_eq!(role_on(&h, &cy, &id).await, Ok("viewer".into()));
}

#[tokio::test]
async fn moving_sheets_between_folders() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let cy = h.carbon("c:cy", "Cy");
    let a1 = new_folder(&h, &ada, "Plans", None).await;
    let a2 = new_folder(&h, &ada, "Archive", None).await;
    let b1 = new_folder(&h, &bob, "Bob's", None).await;
    let s = new_sheet_id(&h, &ada, "Q3", None).await;
    let path = format!("/v1/sheets/{s}");

    let moved = h.patch(&ada, &path, json!({"folder_id": a1})).await;
    assert_eq!(expect(&moved, 200)["folder_id"], json!(a1));

    // Bob edits both of Ada's folders, so he can move her sheet between them, but not out to her top level.
    share(&h, &ada, &format!("folders/{a1}"), &["c:bob"], "editor").await;
    share(&h, &ada, &format!("folders/{a2}"), &["c:bob"], "editor").await;
    let by_bob = h.patch(&bob, &path, json!({"folder_id": a2})).await;
    assert_eq!(expect(&by_bob, 200)["folder_id"], json!(a2));
    expect_error(&h.patch(&bob, &path, json!({"folder_id": null})).await, 403, "not_allowed");

    // A folder only holds what its owner owns: into Bob's folder needs a transfer first.
    share(&h, &bob, &format!("folders/{b1}"), &["c:ada"], "editor").await;
    let e = expect_error(&h.patch(&ada, &path, json!({"folder_id": b1})).await, 409, "conflict");
    assert!(e["hint"].as_str().unwrap().contains("calc transfer"));

    // A viewer of a folder can't put things in it.
    share(&h, &ada, &format!("sheets/{s}"), &["c:cy"], "editor").await;
    share(&h, &ada, &format!("folders/{a1}"), &["c:cy"], "viewer").await;
    expect_error(&h.patch(&cy, &path, json!({"folder_id": a1})).await, 403, "not_allowed");
    expect_error(&h.patch(&ada, &path, json!({"folder_id": "fnotafolder1"})).await, 422, "invalid_field");

    let top = h.patch(&ada, &path, json!({"folder_id": null})).await;
    assert_eq!(expect(&top, 200)["folder_id"], json!(null));
    let top = h.get(&ada, "/v1/sheets").await;
    assert!(ids(&expect(&top, 200)["sheets"]).contains(&s));
}

#[tokio::test]
async fn the_trash_keeps_everything_until_deleted_forever() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let s = new_sheet_id(&h, &ada, "Q3 revenue", None).await;
    let other = new_sheet_id(&h, &ada, "Kept", None).await;
    share(&h, &ada, &format!("sheets/{s}"), &["c:bob"], "editor").await;
    let path = format!("/v1/sheets/{s}");

    // Only the owner moves it to the trash; an editor is told to leave instead.
    let e = expect_error(&h.delete(&bob, &path).await, 403, "not_allowed");
    assert!(e["hint"].as_str().unwrap().contains("leave"));
    expect(&h.delete(&ada, &path).await, 204);
    expect(&h.delete(&ada, &path).await, 204);

    // In the trash it is only its owner's, and can't be changed until restored.
    expect_error(&h.get(&bob, &path).await, 404, "not_found");
    let got = h.get(&ada, &path).await;
    let got = expect(&got, 200);
    assert_eq!(got["role"], json!("owner"));
    assert!(got["trashed_at"].is_string());
    expect_error(&h.patch(&ada, &path, json!({"title": "Renamed"})).await, 409, "conflict");
    expect_error(&h.post(&ada, &format!("/v1/sheets/{s}/transfer"), json!({"to": "c:bob"})).await, 409, "conflict");

    // Restoring brings it back with everything, sharing included.
    expect_error(&h.post(&bob, &format!("{path}/restore"), json!({})).await, 404, "not_found");
    let restored = h.post(&ada, &format!("{path}/restore"), json!({})).await;
    let restored = expect(&restored, 200);
    assert_eq!(restored["trashed_at"], json!(null));
    assert_eq!(restored["tabs"][0]["name"], json!("Tab 1"));
    assert_eq!(role_on(&h, &bob, &s).await, Ok("editor".into()));
    expect_error(&h.post(&ada, &format!("{path}/restore"), json!({})).await, 409, "conflict");

    // Deleting forever: only from the trash, only by the owner, only with the exact title.
    let confirm = format!("{path}/forever?confirm=Q3%20revenue");
    let e = expect_error(&h.delete(&ada, &confirm).await, 409, "conflict");
    assert!(e["hint"].as_str().unwrap().contains("trash"));
    expect(&h.delete(&ada, &path).await, 204);
    expect_error(&h.delete(&bob, &confirm).await, 404, "not_found");
    let e = expect_error(&h.delete(&ada, &format!("{path}/forever")).await, 422, "invalid_field");
    assert!(e["details"]["fields"]["confirm"].is_string());
    expect_error(&h.delete(&ada, &format!("{path}/forever?confirm=Q3")).await, 422, "invalid_field");

    let blobs_of = |sheet: String| {
        let db = h.state.db.clone();
        async move {
            let rows: Vec<(String,)> = sqlx::query_as("select blob from snapshots where line_id = $1")
                .bind(main_line(&sheet))
                .fetch_all(&db)
                .await
                .expect("snapshots");
            rows.into_iter().map(|r| r.0).collect::<Vec<String>>()
        }
    };
    let keys = blobs_of(s.clone()).await;
    let other_keys = blobs_of(other.clone()).await;
    assert!(!keys.is_empty());
    expect(&h.delete(&ada, &confirm).await, 204);

    // Gone for everyone, everywhere.
    expect_error(&h.get(&ada, &path).await, 404, "not_found");
    expect_error(&h.post(&ada, &format!("{path}/restore"), json!({})).await, 404, "not_found");
    expect_error(&h.delete(&ada, &confirm).await, 404, "not_found");
    let trash = h.get(&ada, "/v1/sheets?scope=trash").await;
    assert!(!ids(&expect(&trash, 200)["sheets"]).contains(&s));
    let bobs = h.get(&bob, "/v1/sheets?scope=shared").await;
    assert!(!ids(&expect(&bobs, 200)["sheets"]).contains(&s));

    // Its id stays taken by a tombstone holding nothing.
    let (title, gone, folder): (String, bool, Option<String>) =
        sqlx::query_as("select title, deleted_at is not null, folder_id from sheets where id = $1")
            .bind(&s)
            .fetch_one(&h.state.db)
            .await
            .expect("tombstone");
    assert_eq!((title.as_str(), gone, folder), ("", true, None));
    for (table, column) in [("lines", "sheet_id"), ("threads", "sheet_id")] {
        let n: (i64,) = sqlx::query_as(sqlx::AssertSqlSafe(format!("select count(*) from {table} where {column} = $1")))
            .bind(&s)
            .fetch_one(&h.state.db)
            .await
            .expect("count");
        assert_eq!(n.0, 0, "{table} rows of a sheet deleted forever");
    }
    let n: (i64,) = sqlx::query_as("select count(*) from snapshots where line_id = $1")
        .bind(main_line(&s))
        .fetch_one(&h.state.db)
        .await
        .expect("count");
    assert_eq!(n.0, 0);
    let n: (i64,) = sqlx::query_as("select count(*) from members where item_id = $1").bind(&s).fetch_one(&h.state.db).await.expect("count");
    assert_eq!(n.0, 0);

    // Its blobs go once nothing points at them (an empty sheet's snapshot may be the very same bytes as another's).
    let shared_key: Vec<String> = keys.iter().filter(|k| other_keys.contains(k)).cloned().collect();
    let result = calc_api::sheets::collect_blobs(&h.state, &keys, Duration::ZERO).await;
    for (key, removal) in &result {
        let want = if shared_key.contains(key) { Removal::InUse } else { Removal::Removed };
        assert_eq!(*removal, want, "blob {key}");
    }
    expect(&h.delete(&ada, &format!("/v1/sheets/{other}")).await, 204);
    expect(&h.delete(&ada, &format!("/v1/sheets/{other}/forever?confirm=Kept")).await, 204);
    let result = calc_api::sheets::collect_blobs(&h.state, &shared_key, Duration::ZERO).await;
    assert!(result.iter().all(|(_, r)| *r == Removal::Removed), "{result:?}");
    for key in keys.iter().chain(&other_keys) {
        assert!(!h.state.blobs.exists(key).await, "blob {key} is gone");
    }
    // A blob used just now stays, whoever asks.
    let fresh = h.state.blobs.put(b"just stored").await.expect("put");
    let kept = calc_api::sheets::collect_blobs(&h.state, std::slice::from_ref(&fresh), Duration::from_secs(600)).await;
    assert_eq!(kept, vec![(fresh, Removal::Recent)]);
}
