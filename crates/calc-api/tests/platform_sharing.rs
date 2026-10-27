//! Sharing: inviting by c:id, si:id and email, invites that become memberships on first sign-in, unknown ids,
//! roles never above the inviter's, changing roles, removing and leaving, where access comes from, public links
//! signed in and out, and the `calc.sheet.shared` tings.

mod platform_support;
mod support;

use platform_support::*;
use serde_json::{Value, json};

fn member<'a>(sharing: &'a Value, id: &str) -> Option<&'a Value> {
    sharing["members"].as_array()?.iter().find(|m| m["account"]["id"] == json!(id))
}

#[tokio::test]
async fn invites_by_id_and_email_and_tings_for_new_members() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let analyst = h.silicon("si:analyst", "Analyst");
    // Carol has an account but has never signed in to Calc, so Calc doesn't know her address yet.
    let carol = h.carbon("c:carol", "Carol");
    // Dee signed in before: her address is hers.
    let dee = h.carbon("c:dee", "Dee");
    sign_in(&h, &dee).await;
    let s = new_sheet_id(&h, &ada, "Q3 revenue", None).await;
    let item = format!("sheets/{s}");

    let r = h
        .post(
            &ada,
            &format!("/v1/{item}/sharing"),
            json!({"invite": ["c:bob", "SI:Analyst", "Carol@Example.test", "dee@example.test", "nobody@else.test"], "role": "commenter"}),
        )
        .await;
    let sharing = expect(&r, 200).clone();
    assert_eq!(sharing["owner"]["id"], json!("c:ada"));
    for id in ["c:bob", "si:analyst", "c:dee"] {
        let m = member(&sharing, id).unwrap_or_else(|| panic!("{id} is a member: {sharing}"));
        assert_eq!(m["role"], json!("commenter"));
        assert_eq!(m["via"], json!("direct"));
        assert_eq!(m["account"]["display_name"].as_str().map(|s| !s.is_empty()), Some(true));
    }
    let invites: Vec<&str> = sharing["invites"].as_array().unwrap().iter().map(|i| i["email"].as_str().unwrap()).collect();
    assert_eq!(invites, vec!["carol@example.test", "nobody@else.test"]);
    assert_eq!(sharing["invites"][0]["role"], json!("commenter"));
    assert_eq!(sharing["invites"][0]["invited_by"]["id"], json!("c:ada"));
    assert_eq!(sharing["public"], json!({"enabled": false, "access": "view", "require_email": false}));
    assert_eq!(role_on(&h, &analyst, &s).await, Ok("commenter".into()));

    // Each new member hears about it once, with what they need to open it.
    let accounts = [&ada, &bob, &analyst, &carol, &dee];
    let mut sent = sent_to(&h, &accounts).await;
    sent.sort();
    let want: Vec<(String, String)> =
        ["c:bob", "c:dee", "si:analyst"].iter().map(|id| ("calc.sheet.shared".to_owned(), id.to_string())).collect();
    assert_eq!(sent, want);
    let t = &tings_for(&h, "calc.sheet.shared", &analyst).await[0];
    assert_eq!(t.body["type"], json!("calc.sheet.shared"));
    assert_eq!(t.body["for"], json!(analyst.uuid.to_string()));
    assert_eq!(t.body["key"], json!(t.key));
    assert_eq!(t.body["metadata"], json!({}));
    let data = &t.body["data"];
    assert_eq!(data["sheet"], json!({"id": s, "title": "Q3 revenue"}));
    assert_eq!(data["role"], json!("commenter"));
    assert_eq!(data["by"]["id"], json!("c:ada"));
    assert_eq!(data["url"], json!(format!("http://127.0.0.1:8731/s/{s}")));
    assert_eq!(data["cli"], json!(format!("calc sheets show {s}")));

    // Inviting someone who already has access changes their role but tells them nothing new.
    share(&h, &ada, &item, &["c:bob"], "editor").await;
    assert_eq!(role_on(&h, &bob, &s).await, Ok("editor".into()));
    assert_eq!(tings_for(&h, "calc.sheet.shared", &bob).await.len(), 1);
    // notify: false keeps it quiet.
    let eve = h.carbon("c:eve", "Eve");
    let r = h.post(&ada, &format!("/v1/{item}/sharing"), json!({"invite": "c:eve", "role": "viewer", "notify": false})).await;
    expect(&r, 200);
    assert!(tings_for(&h, "calc.sheet.shared", &eve).await.is_empty());
    assert_eq!(role_on(&h, &eve, &s).await, Ok("viewer".into()));

    // Carol signs in to Calc for the first time: her invite becomes a membership, and she is told.
    assert_eq!(role_on(&h, &carol, &s).await, Ok("commenter".into()));
    let sharing = h.get(&ada, &format!("/v1/{item}/sharing")).await;
    let sharing = expect(&sharing, 200);
    assert!(member(sharing, "c:carol").is_some());
    let invites: Vec<&str> = sharing["invites"].as_array().unwrap().iter().map(|i| i["email"].as_str().unwrap()).collect();
    assert_eq!(invites, vec!["nobody@else.test"]);
    let told = tings_for(&h, "calc.sheet.shared", &carol).await;
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].body["data"]["by"]["id"], json!("c:ada"));
    // And an address Calc now knows is that account straight away.
    let other = new_sheet_id(&h, &ada, "Other", None).await;
    let r = share(&h, &ada, &format!("sheets/{other}"), &["carol@example.test"], "viewer").await;
    assert!(member(&r, "c:carol").is_some());
    assert_eq!(r["invites"], json!([]));

    // Only those who can share see the invites.
    let as_bob = h.get(&bob, &format!("/v1/{item}/sharing")).await;
    assert_eq!(expect(&as_bob, 200)["invites"].as_array().map(Vec::len), Some(1));
    let as_dee = h.get(&dee, &format!("/v1/{item}/sharing")).await;
    let as_dee = expect(&as_dee, 200);
    assert_eq!(as_dee["invites"], json!([]));
    assert!(member(as_dee, "c:bob").is_some(), "everyone with access sees who else has it");

    // Taking an invite back.
    let gone = format!("/v1/{item}/invites/nobody@else.test");
    expect_error(&h.delete(&dee, &gone).await, 403, "not_allowed");
    expect(&h.delete(&ada, &gone).await, 204);
    expect_error(&h.delete(&ada, &gone).await, 404, "not_found");
}

#[tokio::test]
async fn invites_for_email_addresses_wait_for_the_account() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let f = new_folder(&h, &ada, "Finance", None).await;
    let s = new_sheet_id(&h, &ada, "Q3", Some(&f)).await;
    share(&h, &ada, &format!("folders/{f}"), &["frank@example.test"], "editor").await;
    share(&h, &ada, &format!("sheets/{s}"), &["frank@example.test"], "viewer").await;
    // The account comes later, with that verified address.
    let frank = h.carbon("c:frank", "Frank");
    sign_in(&h, &frank).await;
    assert_eq!(role_on(&h, &frank, &s).await, Ok("editor".into()), "the folder's editor role is higher than the direct viewer one");
    let folder = h.get(&frank, &format!("/v1/folders/{f}")).await;
    assert_eq!(expect(&folder, 200)["folder"]["role"], json!("editor"));
    let mut kinds: Vec<String> = tings_for(&h, "calc.sheet.shared", &frank).await.into_iter().map(|t| t.body["data"]["kind"].as_str().unwrap().to_owned()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["folder", "sheet"]);
    let folder_ting = tings_for(&h, "calc.sheet.shared", &frank).await.into_iter().find(|t| t.body["data"]["kind"] == json!("folder")).unwrap();
    assert_eq!(folder_ting.body["data"]["folder"], json!({"id": f, "name": "Finance"}));
    assert_eq!(folder_ting.body["data"]["cli"], json!(format!("calc folders show {f}")));
    let left: (i64,) = sqlx::query_as("select count(*) from invites").fetch_one(&h.state.db).await.unwrap();
    assert_eq!(left.0, 0);
    // A Silicon has no address to match: a Silicon is invited by its si:id.
}

#[tokio::test]
async fn unknown_ids_answer_422_naming_them_and_change_nothing() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    h.carbon("c:bob", "Bob");
    let s = new_sheet_id(&h, &ada, "Q3", None).await;
    let r = h
        .post(&ada, &format!("/v1/sheets/{s}/sharing"), json!({"invite": ["c:bob", "c:nobody", "si:ghost", "not an address"], "role": "viewer"}))
        .await;
    let e = expect_error(&r, 422, "invalid_field");
    assert_eq!(e["details"]["unknown"], json!(["c:nobody", "si:ghost", "not an address"]));
    let message = e["message"].as_str().unwrap();
    assert!(message.contains("c:nobody") && message.contains("si:ghost"), "{message}");
    assert!(e["details"]["fields"]["invite"].is_string());
    let sharing = h.get(&ada, &format!("/v1/sheets/{s}/sharing")).await;
    assert_eq!(expect(&sharing, 200)["members"], json!([]), "nobody was added");
    assert!(outbox(&h).await.is_empty());

    expect_error(&h.post(&ada, &format!("/v1/sheets/{s}/sharing"), json!({"invite": [], "role": "viewer"})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, &format!("/v1/sheets/{s}/sharing"), json!({"role": "viewer"})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, &format!("/v1/sheets/{s}/sharing"), json!({"invite": ["c:bob"], "role": "boss"})).await, 422, "invalid_field");
    expect_error(&h.post(&ada, &format!("/v1/sheets/{s}/sharing"), json!({"invite": ["c:bob"], "role": "owner"})).await, 422, "invalid_field");
}

#[tokio::test]
async fn only_owners_and_editors_share_and_never_above_their_own_role() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let cy = h.carbon("c:cy", "Cy");
    let dee = h.carbon("c:dee", "Dee");
    let analyst = h.silicon("si:analyst", "Analyst");
    let s = new_sheet_id(&h, &ada, "Q3", None).await;
    let item = format!("sheets/{s}");
    share(&h, &ada, &item, &["si:analyst"], "editor").await;
    share(&h, &ada, &item, &["c:bob"], "commenter").await;
    share(&h, &ada, &item, &["c:cy"], "viewer").await;

    // A Silicon editor invites like any editor.
    let r = share(&h, &analyst, &item, &["c:dee"], "editor").await;
    let m = member(&r, "c:dee").unwrap();
    assert_eq!(m["role"], json!("editor"));
    let told = tings_for(&h, "calc.sheet.shared", &dee).await;
    assert_eq!(told[0].body["data"]["by"]["id"], json!("si:analyst"));

    for (who, role) in [(&bob, "commenter"), (&cy, "viewer")] {
        let e = expect_error(&h.post(who, &format!("/v1/{item}/sharing"), json!({"invite": ["c:ada"], "role": "viewer"})).await, 403, "not_allowed");
        assert!(e["message"].as_str().unwrap().contains(role));
        expect_error(&h.patch(who, &format!("/v1/{item}/sharing/{}", dee.uuid), json!({"role": "viewer"})).await, 403, "not_allowed");
        expect_error(&h.delete(who, &format!("/v1/{item}/sharing/{}", dee.uuid)).await, 403, "not_allowed");
        expect_error(&h.put(who, &format!("/v1/{item}/public"), json!({"enabled": true, "access": "view"})).await, 403, "not_allowed");
    }
    // Owner can't be given away by sharing.
    expect_error(&h.patch(&analyst, &format!("/v1/{item}/sharing/{}", dee.uuid), json!({"role": "owner"})).await, 422, "invalid_field");
    // Sharing a sheet in the trash waits until it is restored.
    expect(&h.delete(&ada, &format!("/v1/{item}")).await, 204);
    expect_error(&h.post(&ada, &format!("/v1/{item}/sharing"), json!({"invite": ["c:bob"], "role": "viewer"})).await, 409, "conflict");
}

#[tokio::test]
async fn changing_roles_removing_and_leaving() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let analyst = h.silicon("si:analyst", "Analyst");
    let s = new_sheet_id(&h, &ada, "Q3", None).await;
    let item = format!("sheets/{s}");
    share(&h, &ada, &item, &["c:bob", "si:analyst"], "editor").await;

    let r = h.patch(&ada, &format!("/v1/{item}/sharing/{}", bob.uuid), json!({"role": "viewer"})).await;
    assert_eq!(member(expect(&r, 200), "c:bob").unwrap()["role"], json!("viewer"));
    assert_eq!(role_on(&h, &bob, &s).await, Ok("viewer".into()));
    // An editor changes another member's role too.
    let r = h.patch(&analyst, &format!("/v1/{item}/sharing/{}", bob.uuid), json!({"role": "commenter"})).await;
    expect(&r, 200);
    assert_eq!(role_on(&h, &bob, &s).await, Ok("commenter".into()));

    expect_error(&h.patch(&ada, &format!("/v1/{item}/sharing/{}", ada.uuid), json!({"role": "viewer"})).await, 409, "conflict");
    expect_error(&h.patch(&ada, &format!("/v1/{item}/sharing/{}", uuid::Uuid::new_v4()), json!({"role": "viewer"})).await, 404, "not_found");
    expect_error(&h.patch(&ada, &format!("/v1/{item}/sharing/c:bob"), json!({"role": "viewer"})).await, 404, "not_found");
    expect_error(&h.patch(&ada, &format!("/v1/{item}/sharing/{}", bob.uuid), json!({})).await, 422, "invalid_field");

    // Leaving: anyone with a direct role; the owner can't.
    expect(&h.delete(&bob, &format!("/v1/{item}/sharing/{}", bob.uuid)).await, 204);
    assert_eq!(role_on(&h, &bob, &s).await, Err(404));
    let e = expect_error(&h.delete(&ada, &format!("/v1/{item}/sharing/{}", ada.uuid)).await, 409, "conflict");
    assert!(e["hint"].as_str().unwrap().contains("transfer"));
    // Removing someone else.
    expect(&h.delete(&ada, &format!("/v1/{item}/sharing/{}", analyst.uuid)).await, 204);
    assert_eq!(role_on(&h, &analyst, &s).await, Err(404));
    expect_error(&h.delete(&ada, &format!("/v1/{item}/sharing/{}", analyst.uuid)).await, 404, "not_found");
    let sharing = h.get(&ada, &format!("/v1/{item}/sharing")).await;
    assert_eq!(expect(&sharing, 200)["members"], json!([]));
}

#[tokio::test]
async fn members_show_where_their_access_comes_from() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let analyst = h.silicon("si:analyst", "Analyst");
    let f1 = new_folder(&h, &ada, "Finance", None).await;
    let f2 = new_folder(&h, &ada, "2026", Some(&f1)).await;
    let s = new_sheet_id(&h, &ada, "Q3", Some(&f2)).await;
    let item = format!("sheets/{s}");
    share(&h, &ada, &format!("folders/{f1}"), &["c:bob"], "viewer").await;
    share(&h, &ada, &format!("folders/{f2}"), &["si:analyst"], "editor").await;
    share(&h, &ada, &item, &["si:analyst"], "commenter").await;

    let sharing = h.get(&ada, &format!("/v1/{item}/sharing")).await;
    let sharing = expect(&sharing, 200);
    let b = member(sharing, "c:bob").unwrap();
    assert_eq!(b["role"], json!("viewer"));
    assert_eq!(b["via"]["folder"]["id"], json!(f1));
    assert_eq!(b["via"]["folder"]["name"], json!("Finance"));
    let a = member(sharing, "si:analyst").unwrap();
    assert_eq!(a["role"], json!("editor"), "the folder's editor role beats the direct commenter one");
    assert_eq!(a["via"]["folder"]["id"], json!(f2));

    // Bob's access comes from the folder: changing it here is refused, and leaving here is too.
    let e = expect_error(&h.patch(&ada, &format!("/v1/{item}/sharing/{}", bob.uuid), json!({"role": "editor"})).await, 409, "conflict");
    assert!(e["message"].as_str().unwrap().contains("Finance"));
    let e = expect_error(&h.delete(&bob, &format!("/v1/{item}/sharing/{}", bob.uuid)).await, 409, "conflict");
    assert!(e["hint"].as_str().unwrap().contains("Finance"));
    // A folder member sees the folder's sharing too; one who can't open a folder above sees no role for it.
    let as_analyst = h.get(&analyst, &format!("/v1/{item}/sharing")).await;
    let as_analyst = expect(&as_analyst, 200);
    let via = &member(as_analyst, "c:bob").unwrap()["via"]["folder"];
    assert_eq!(via["id"], json!(f1));
    assert!(via.get("role").is_none(), "the Silicon can't open Finance: {via}");
    let folder_sharing = h.get(&bob, &format!("/v1/folders/{f2}/sharing")).await;
    let folder_sharing = expect(&folder_sharing, 200);
    assert_eq!(member(folder_sharing, "si:analyst").unwrap()["via"], json!("direct"));
    assert_eq!(member(folder_sharing, "c:bob").unwrap()["via"]["folder"]["id"], json!(f1));
}

#[tokio::test]
async fn public_links_signed_in_and_out() {
    let h = support::Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.carbon("c:bob", "Bob");
    let stranger = h.silicon("si:stranger", "Stranger");
    let s = new_sheet_id(&h, &ada, "Open data", None).await;
    let path = format!("/v1/sheets/{s}");
    share(&h, &ada, &format!("sheets/{s}"), &["c:bob"], "editor").await;

    let r = h.put(&bob, &format!("{path}/public"), json!({"enabled": true, "access": "view"})).await;
    let r = expect(&r, 200);
    assert_eq!(r["enabled"], json!(true));
    assert_eq!(r["access"], json!("view"));
    assert_eq!(r["url"], json!(format!("http://127.0.0.1:8731/s/{s}")));

    // Viewing never needs a sign-in.
    let anon = h.get_anon(&path).await;
    let anon = expect(&anon, 200);
    assert_eq!(anon["role"], json!("viewer"));
    assert_eq!(anon["public"], json!({"enabled": true, "access": "view", "require_email": false}));
    assert_eq!(anon["tabs"][0]["name"], json!("Tab 1"));
    let threads = h.get_anon(&format!("{path}/comments")).await;
    assert_eq!(expect(&threads, 200)["threads"], json!([]));
    assert_eq!(role_on(&h, &stranger, &s).await, Ok("viewer".into()));
    expect_error(&h.patch(&stranger, &path, json!({"title": "Mine"})).await, 403, "not_allowed");

    // Commenting and editing through the link need a sign-in.
    expect(&h.put(&ada, &format!("{path}/public"), json!({"access": "comment"})).await, 200);
    assert_eq!(role_on(&h, &stranger, &s).await, Ok("commenter".into()));
    assert_eq!(expect(&h.get_anon(&path).await, 200)["role"], json!("viewer"));
    let anon_comment = h.http.post(format!("{}{path}/comments", h.url)).json(&json!({"body": "hi"})).send().await.unwrap();
    assert_eq!(anon_comment.status().as_u16(), 401);
    expect(&h.put(&ada, &format!("{path}/public"), json!({"enabled": true, "access": "edit"})).await, 200);
    assert_eq!(role_on(&h, &stranger, &s).await, Ok("editor".into()));
    let renamed = h.patch(&stranger, &path, json!({"title": "Open data (edited)"})).await;
    assert_eq!(expect(&renamed, 200)["updated_by"]["id"], json!("si:stranger"));
    let anon_patch = h.http.patch(format!("{}{path}", h.url)).json(&json!({"title": "x"})).send().await.unwrap();
    assert_eq!(anon_patch.status().as_u16(), 401);
    // Members keep their own role when it's higher than the link's; the owner stays the owner.
    assert_eq!(role_on(&h, &bob, &s).await, Ok("editor".into()));
    assert_eq!(role_on(&h, &ada, &s).await, Ok("owner".into()));

    // A public sheet is reachable, never listed: not in the stranger's lists or search.
    let lists = h.get(&stranger, "/v1/sheets?scope=shared").await;
    assert_eq!(expect(&lists, 200)["sheets"], json!([]));
    let found = h.get(&stranger, "/v1/search?q=open").await;
    assert_eq!(expect(&found, 200)["results"], json!([]));

    // Off again: the link stops working.
    let off = h.put(&ada, &format!("{path}/public"), json!({"enabled": false})).await;
    assert_eq!(expect(&off, 200)["enabled"], json!(false));
    expect_error(&h.get_anon(&path).await, 401, "signed_out");
    assert_eq!(role_on(&h, &stranger, &s).await, Err(404));
    // In the trash, the link doesn't work either.
    expect(&h.put(&ada, &format!("{path}/public"), json!({"enabled": true, "access": "view"})).await, 200);
    expect(&h.delete(&ada, &path).await, 204);
    expect_error(&h.get_anon(&path).await, 401, "signed_out");

    expect_error(&h.put(&ada, &format!("{path}/public"), json!({"access": "everything"})).await, 422, "invalid_field");
    let f = new_folder(&h, &ada, "Folder", None).await;
    expect_error(&h.put(&ada, &format!("/v1/folders/{f}/public"), json!({"enabled": true, "access": "view"})).await, 400, "invalid_request");
}
