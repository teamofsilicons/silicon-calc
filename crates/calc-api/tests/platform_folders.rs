//! Folders: nesting and paths, access inherited through nested folders (the higher role wins), what is made inside
//! someone else's folder, moving, the trash with everything inside, deleting forever.

mod platform_support;
mod support;

use platform_support::*;
use serde_json::json;

#[tokio::test]
async fn folders_nest_and_show_their_path() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let f1 = new_folder(&h, &ada, "Finance", None).await;
    let f2 = new_folder(&h, &ada, "2026", Some(&f1)).await;
    let f3 = new_folder(&h, &ada, "Q3", Some(&f2)).await;
    let s = new_sheet_id(&h, &ada, "Revenue", Some(&f3)).await;

    let view = h.get(&ada, &format!("/v1/folders/{f3}")).await;
    let view = expect(&view, 200);
    assert_eq!(view["folder"]["id"], json!(f3));
    assert_eq!(view["folder"]["name"], json!("Q3"));
    assert_eq!(view["folder"]["parent_id"], json!(f2));
    assert_eq!(view["folder"]["role"], json!("owner"));
    assert_eq!(view["folder"]["owner"]["id"], json!("c:ada"));
    assert_eq!(view["folder"]["trashed_at"], json!(null));
    assert_eq!(ids(&view["path"]), vec![f1.clone(), f2.clone()], "the folders above, top first");
    assert_eq!(ids(&view["sheets"]), vec![s.clone()]);
    assert_eq!(view["folders"], json!([]));
    let top = h.get(&ada, &format!("/v1/folders/{f1}")).await;
    let top = expect(&top, 200);
    assert_eq!(top["path"], json!([]));
    assert_eq!(ids(&top["folders"]), vec![f2.clone()]);
    let sheet = h.get(&ada, &format!("/v1/sheets/{s}")).await;
    assert_eq!(expect(&sheet, 200)["folder_id"], json!(f3));

    // Only top-level folders are on the top level.
    let list = h.get(&ada, "/v1/sheets").await;
    let list = expect(&list, 200);
    assert_eq!(ids(&list["folders"]), vec![f1.clone()]);
    assert_eq!(list["sheets"], json!([]));

    expect_error(&h.post(&ada, "/v1/folders", json!({})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, "/v1/folders", json!({"name": "  "})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, "/v1/folders", json!({"name": "x".repeat(201)})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, "/v1/folders", json!({"name": "a\tb"})).await, 422, "invalid_field");
    let e = expect_error(&h.post(&ada, "/v1/folders", json!({"name": "In nothing", "parent_id": "fnotafolder1"})).await, 422, "invalid_field");
    assert!(e["details"]["fields"]["parent_id"].is_string());
    expect_error(&h.get(&bob, &format!("/v1/folders/{f1}")).await, 404, "not_found");
    expect_error(&h.get(&ada, "/v1/folders/nope").await, 404, "not_found");
    expect_error(&h.get_anon(&format!("/v1/folders/{f1}")).await, 401, "signed_out");
}

#[tokio::test]
async fn a_shared_folder_shares_everything_in_it_and_the_higher_role_wins() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let analyst = h.silicon("si:analyst", "Analyst");
    let f1 = new_folder(&h, &ada, "Finance", None).await;
    let f2 = new_folder(&h, &ada, "2026", Some(&f1)).await;
    let f3 = new_folder(&h, &ada, "Q3", Some(&f2)).await;
    let before = new_sheet_id(&h, &ada, "Made before sharing", Some(&f3)).await;
    let outside = new_sheet_id(&h, &ada, "Not in the folder", None).await;

    share(&h, &ada, &format!("folders/{f1}"), &["c:bob", "si:analyst"], "viewer").await;
    // Added later, three folders down: shared all the same.
    let later = new_sheet_id(&h, &ada, "Made after sharing", Some(&f3)).await;
    let deeper = new_folder(&h, &ada, "Drafts", Some(&f3)).await;
    for s in [&before, &later] {
        assert_eq!(role_on(&h, &bob, s).await, Ok("viewer".into()));
        assert_eq!(role_on(&h, &analyst, s).await, Ok("viewer".into()));
    }
    assert_eq!(role_on(&h, &bob, &outside).await, Err(404));
    let view = h.get(&bob, &format!("/v1/folders/{deeper}")).await;
    let view = expect(&view, 200);
    assert_eq!(view["folder"]["role"], json!("viewer"));
    assert_eq!(ids(&view["path"]), vec![f1.clone(), f2.clone(), f3.clone()]);

    // Bob's shared list shows the folder itself, not everything in it.
    let shared = h.get(&bob, "/v1/sheets?scope=shared").await;
    let shared = expect(&shared, 200);
    assert_eq!(ids(&shared["folders"]), vec![f1.clone()]);
    assert_eq!(shared["sheets"], json!([]));
    assert_eq!(shared["folders"][0]["role"], json!("viewer"));
    let inside = h.get(&bob, &format!("/v1/sheets?folder={f3}")).await;
    let mut got = ids(&expect(&inside, 200)["sheets"]);
    got.sort();
    let mut want = vec![before.clone(), later.clone()];
    want.sort();
    assert_eq!(got, want);

    // A viewer can't add to it.
    expect_error(&h.post(&bob, "/v1/sheets", json!({"title": "Mine", "folder_id": f3})).await, 403, "not_allowed");
    expect_error(&h.post(&bob, "/v1/folders", json!({"name": "Mine", "parent_id": f3})).await, 403, "not_allowed");

    // Editor on a folder nearer the sheet beats viewer above it; a direct role only counts when higher.
    share(&h, &ada, &format!("folders/{f2}"), &["c:bob"], "editor").await;
    assert_eq!(role_on(&h, &bob, &later).await, Ok("editor".into()));
    share(&h, &ada, &format!("sheets/{later}"), &["c:bob"], "commenter").await;
    assert_eq!(role_on(&h, &bob, &later).await, Ok("editor".into()));
    share(&h, &ada, &format!("sheets/{before}"), &["si:analyst"], "commenter").await;
    assert_eq!(role_on(&h, &analyst, &before).await, Ok("commenter".into()));
    assert_eq!(role_on(&h, &analyst, &later).await, Ok("viewer".into()));

    // Taking the folder role away leaves what's left.
    let bob_uuid = bob.uuid.to_string();
    expect(&h.delete(&ada, &format!("/v1/folders/{f2}/sharing/{bob_uuid}")).await, 204);
    assert_eq!(role_on(&h, &bob, &later).await, Ok("commenter".into()));
    assert_eq!(role_on(&h, &bob, &before).await, Ok("viewer".into()));
    expect(&h.delete(&ada, &format!("/v1/folders/{f1}/sharing/{bob_uuid}")).await, 204);
    assert_eq!(role_on(&h, &bob, &before).await, Err(404));
    assert_eq!(role_on(&h, &bob, &later).await, Ok("commenter".into()));
}

#[tokio::test]
async fn what_is_made_in_someone_elses_folder_belongs_to_its_owner() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let analyst = h.silicon("si:analyst", "Analyst");
    let f = new_folder(&h, &ada, "Analysis", None).await;
    share(&h, &ada, &format!("folders/{f}"), &["si:analyst"], "editor").await;

    let s = new_sheet(&h, &analyst, "Weekly buckets", Some(&f)).await;
    assert_eq!(s["owner"]["id"], json!("c:ada"));
    assert_eq!(s["created_by"]["id"], json!("si:analyst"));
    assert_eq!(s["role"], json!("editor"));
    assert_eq!(s["folder_id"], json!(f));
    let sub = h.post(&analyst, "/v1/folders", json!({"name": "Runs", "parent_id": f})).await;
    let sub = expect(&sub, 201);
    assert_eq!(sub["owner"]["id"], json!("c:ada"));
    assert_eq!(sub["created_by"]["id"], json!("si:analyst"));
    assert_eq!(sub["role"], json!("editor"));
    let sub_id = sub["id"].as_str().unwrap().to_owned();
    let nested = new_sheet(&h, &analyst, "Run 1", Some(&sub_id)).await;
    assert_eq!(nested["owner"]["id"], json!("c:ada"));

    // It is Ada's: in her folder, and not in the Silicon's own sheets.
    let mine = h.get(&analyst, "/v1/sheets").await;
    let mine = expect(&mine, 200);
    assert_eq!(mine["sheets"], json!([]));
    assert_eq!(mine["folders"], json!([]));
    let view = h.get(&ada, &format!("/v1/folders/{f}")).await;
    let view = expect(&view, 200);
    assert_eq!(ids(&view["sheets"]), vec![s["id"].as_str().unwrap().to_owned()]);
    assert_eq!(view["sheets"][0]["role"], json!("owner"));
    assert_eq!(ids(&view["folders"]), vec![sub_id.clone()]);

    // Editors rename and move things around inside it; only the owner moves them out.
    let renamed = h.patch(&analyst, &format!("/v1/folders/{sub_id}"), json!({"name": "Code runs"})).await;
    assert_eq!(expect(&renamed, 200)["name"], json!("Code runs"));
    let other = new_folder(&h, &analyst, "Elsewhere", Some(&f)).await;
    let moved = h.patch(&analyst, &format!("/v1/folders/{sub_id}"), json!({"parent_id": other})).await;
    assert_eq!(expect(&moved, 200)["parent_id"], json!(other));
    expect_error(&h.patch(&analyst, &format!("/v1/folders/{sub_id}"), json!({"parent_id": null})).await, 403, "not_allowed");
    expect_error(&h.delete(&analyst, &format!("/v1/folders/{sub_id}")).await, 403, "not_allowed");
}

#[tokio::test]
async fn moving_folders() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let f1 = new_folder(&h, &ada, "One", None).await;
    let f2 = new_folder(&h, &ada, "Two", Some(&f1)).await;
    let f3 = new_folder(&h, &ada, "Three", Some(&f2)).await;
    let other = new_folder(&h, &ada, "Other", None).await;
    let bobs = new_folder(&h, &bob, "Bob's", None).await;
    share(&h, &bob, &format!("folders/{bobs}"), &["c:ada"], "editor").await;

    let e = expect_error(&h.patch(&ada, &format!("/v1/folders/{f1}"), json!({"parent_id": f3})).await, 422, "invalid_field");
    assert!(e["message"].as_str().unwrap().contains("inside"));
    expect_error(&h.patch(&ada, &format!("/v1/folders/{f1}"), json!({"parent_id": f1})).await, 422, "invalid_field");
    expect_error(&h.patch(&ada, &format!("/v1/folders/{f1}"), json!({"parent_id": bobs})).await, 409, "conflict");
    expect_error(&h.patch(&ada, &format!("/v1/folders/{f1}"), json!({"parent_id": "fnotafolder1"})).await, 422, "invalid_field");

    let moved = h.patch(&ada, &format!("/v1/folders/{f3}"), json!({"parent_id": other})).await;
    assert_eq!(expect(&moved, 200)["parent_id"], json!(other));
    let view = h.get(&ada, &format!("/v1/folders/{f3}")).await;
    assert_eq!(ids(&expect(&view, 200)["path"]), vec![other.clone()]);
    let top = h.patch(&ada, &format!("/v1/folders/{f2}"), json!({"parent_id": null, "name": "Two at the top"})).await;
    let top = expect(&top, 200);
    assert_eq!(top["parent_id"], json!(null));
    assert_eq!(top["name"], json!("Two at the top"));
    let list = h.get(&ada, "/v1/sheets").await;
    let mut tops = ids(&expect(&list, 200)["folders"]);
    tops.sort();
    let mut want = vec![f1.clone(), f2.clone(), other.clone()];
    want.sort();
    assert_eq!(tops, want);
    // Nothing to change answers the folder as it is.
    let same = h.patch(&ada, &format!("/v1/folders/{f1}"), json!({})).await;
    assert_eq!(expect(&same, 200)["name"], json!("One"));
}

#[tokio::test]
async fn a_folder_in_the_trash_takes_everything_in_it_and_brings_it_back() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let f1 = new_folder(&h, &ada, "Finance", None).await;
    let f2 = new_folder(&h, &ada, "2026", Some(&f1)).await;
    let s1 = new_sheet_id(&h, &ada, "Revenue", Some(&f1)).await;
    let s2 = new_sheet_id(&h, &ada, "Costs", Some(&f2)).await;
    let alone = new_sheet_id(&h, &ada, "Trashed on its own", Some(&f2)).await;
    share(&h, &ada, &format!("folders/{f1}"), &["c:bob"], "editor").await;
    expect(&h.delete(&ada, &format!("/v1/sheets/{alone}")).await, 204);

    expect_error(&h.delete(&bob, &format!("/v1/folders/{f1}")).await, 403, "not_allowed");
    expect(&h.delete(&ada, &format!("/v1/folders/{f1}")).await, 204);

    // Everything in it went along: gone for Bob, in the trash for Ada.
    for s in [&s1, &s2] {
        assert_eq!(role_on(&h, &bob, s).await, Err(404));
        let got = h.get(&ada, &format!("/v1/sheets/{s}")).await;
        assert!(expect(&got, 200)["trashed_at"].is_string(), "a sheet inside a trashed folder shows when it went");
        expect_error(&h.patch(&ada, &format!("/v1/sheets/{s}"), json!({"title": "x"})).await, 409, "conflict");
    }
    expect_error(&h.get(&bob, &format!("/v1/folders/{f2}")).await, 404, "not_found");
    let inside = h.get(&ada, &format!("/v1/folders/{f2}")).await;
    assert!(expect(&inside, 200)["folder"]["trashed_at"].is_string());
    expect_error(&h.post(&ada, "/v1/sheets", json!({"title": "New", "folder_id": f2})).await, 409, "conflict");
    let trash = h.get(&ada, "/v1/sheets?scope=trash").await;
    let trash = expect(&trash, 200);
    assert_eq!(ids(&trash["folders"]), vec![f1.clone()]);
    assert_eq!(ids(&trash["sheets"]), vec![alone.clone()], "only what went to the trash itself is listed");
    let mine = h.get(&ada, "/v1/sheets").await;
    assert_eq!(expect(&mine, 200)["folders"], json!([]));
    let shared = h.get(&bob, "/v1/sheets?scope=shared").await;
    assert_eq!(expect(&shared, 200)["folders"], json!([]));

    expect_error(&h.post(&bob, &format!("/v1/folders/{f1}/restore"), json!({})).await, 404, "not_found");
    let back = h.post(&ada, &format!("/v1/folders/{f1}/restore"), json!({})).await;
    assert_eq!(expect(&back, 200)["trashed_at"], json!(null));
    expect_error(&h.post(&ada, &format!("/v1/folders/{f1}/restore"), json!({})).await, 409, "conflict");
    for s in [&s1, &s2] {
        assert_eq!(role_on(&h, &bob, s).await, Ok("editor".into()));
    }
    // What was trashed on its own stays in the trash.
    let got = h.get(&ada, &format!("/v1/sheets/{alone}")).await;
    assert!(expect(&got, 200)["trashed_at"].is_string());
    assert_eq!(role_on(&h, &bob, &alone).await, Err(404));

    // Restoring a sheet whose folder is in the trash brings it out to the top level.
    expect(&h.delete(&ada, &format!("/v1/folders/{f2}")).await, 204);
    let out = h.post(&ada, &format!("/v1/sheets/{s2}/restore"), json!({})).await;
    let out = expect(&out, 200);
    assert_eq!(out["folder_id"], json!(null));
    assert_eq!(out["trashed_at"], json!(null));
    let restored_alone = h.post(&ada, &format!("/v1/sheets/{alone}/restore"), json!({})).await;
    assert_eq!(expect(&restored_alone, 200)["folder_id"], json!(null), "its folder is in the trash");
}

#[tokio::test]
async fn deleting_a_folder_forever_takes_everything_in_it() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let f1 = new_folder(&h, &ada, "Old stuff", None).await;
    let f2 = new_folder(&h, &ada, "Older", Some(&f1)).await;
    let s1 = new_sheet_id(&h, &ada, "One", Some(&f1)).await;
    let s2 = new_sheet_id(&h, &ada, "Two", Some(&f2)).await;
    let keep = new_sheet_id(&h, &ada, "Keep", None).await;
    share(&h, &ada, &format!("folders/{f1}"), &["c:bob", "someone@example.test"], "viewer").await;

    let forever = format!("/v1/folders/{f1}/forever?confirm=Old%20stuff");
    expect_error(&h.delete(&ada, &forever).await, 409, "conflict");
    expect(&h.delete(&ada, &format!("/v1/folders/{f1}")).await, 204);
    expect_error(&h.delete(&bob, &forever).await, 404, "not_found");
    expect_error(&h.delete(&ada, &format!("/v1/folders/{f1}/forever")).await, 422, "invalid_field");
    expect_error(&h.delete(&ada, &format!("/v1/folders/{f1}/forever?confirm=Old")).await, 422, "invalid_field");
    expect(&h.delete(&ada, &forever).await, 204);

    for f in [&f1, &f2] {
        expect_error(&h.get(&ada, &format!("/v1/folders/{f}")).await, 404, "not_found");
        let (name, gone): (String, bool) = sqlx::query_as("select name, deleted_at is not null from folders where id = $1")
            .bind(f)
            .fetch_one(&h.state.db)
            .await
            .expect("tombstone");
        assert_eq!((name.as_str(), gone), ("", true));
    }
    for s in [&s1, &s2] {
        expect_error(&h.get(&ada, &format!("/v1/sheets/{s}")).await, 404, "not_found");
        let gone: (bool,) = sqlx::query_as("select deleted_at is not null from sheets where id = $1").bind(s).fetch_one(&h.state.db).await.unwrap();
        assert!(gone.0);
    }
    let left: (i64, i64) = sqlx::query_as(
        "select (select count(*) from members where item_id = any($1)), (select count(*) from invites where item_id = any($1))",
    )
    .bind(vec![f1.clone(), f2.clone(), s1.clone(), s2.clone()])
    .fetch_one(&h.state.db)
    .await
    .unwrap();
    assert_eq!(left, (0, 0));
    let trash = h.get(&ada, "/v1/sheets?scope=trash").await;
    let trash = expect(&trash, 200);
    assert_eq!(trash["folders"], json!([]));
    assert_eq!(trash["sheets"], json!([]));
    assert_eq!(role_on(&h, &ada, &keep).await, Ok("owner".into()));
}
