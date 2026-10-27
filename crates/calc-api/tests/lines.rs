//! Full HTTP journeys for edits, durable history, copy merges and stable comment anchors.
mod platform_support;
mod support;
use platform_support::*;
use serde_json::{Value, json};
use support::{Harness, TestAccount};

async fn edit(h: &Harness, a: &TestAccount, line: &str, edits: Value) -> Value {
    expect(
        &h.post(a, &format!("{line}/edits"), json!({"edits":edits}))
            .await,
        200,
    )
    .clone()
}
async fn values(h: &Harness, a: &TestAccount, line: &str, range: &str) -> Value {
    expect(
        &h.get(
            a,
            &format!("{line}/tabs/1/cells?range={range}&format=values"),
        )
        .await,
        200,
    )["values"]
        .clone()
}

#[tokio::test]
async fn edits_are_atomic_recalculate_and_survive_reload() {
    let h = Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.silicon("si:bob", "Bob");
    let id = new_sheet_id(&h, &ada, "Ledger", None).await;
    let line = format!("/v1/sheets/{id}");
    share(&h, &ada, &format!("sheets/{id}"), &["si:bob"], "editor").await;
    let written = edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["4","=A1*2"],["8","=SUM(A1:A2)"]]}]),
    )
    .await;
    assert_eq!(written["version"], 1);
    assert_eq!(
        values(&h, &ada, &line, "A1:B2").await,
        json!([[4.0, 8.0], [8.0, 12.0]])
    );
    let bad=h.post(&ada,&format!("{line}/edits"),json!({"edits":[{"type":"set","tab":1,"cells":[{"a":"A1","input":"99"}]},{"type":"rename_tab","tab":987,"name":"Missing"}]})).await;
    assert!(!bad.0.is_success());
    assert_eq!(values(&h, &ada, &line, "A1").await, json!([[4.0]]));
    let overwrite=expect(&h.post(&bob,&format!("{line}/edits"),json!({"base_version":0,"edits":[{"type":"set","tab":1,"cells":[{"a":"A1","input":"5"}]}]})).await,200).clone();
    assert_eq!(overwrite["overwrote"][0]["by"]["id"], "c:ada");
    assert_eq!(
        values(&h, &ada, &line, "B1:B2").await,
        json!([[10.0], [13.0]])
    );
    let one = expect(&h.get(&ada, &format!("{line}/tabs/1/cells/A1")).await, 200).clone();
    assert_eq!(one["history"].as_array().unwrap().len(), 2);
    assert_eq!(one["history"][0]["by"]["id"], "si:bob");
    edit(&h, &bob, &line, json!([{"type":"undo"}])).await;
    assert_eq!(values(&h, &ada, &line, "A1").await, json!([[4.0]]));
    edit(&h, &bob, &line, json!([{"type":"redo"}])).await;
    h.state.lines.discard(&main_line(&id)).await;
    assert_eq!(values(&h, &ada, &line, "A1:B1").await, json!([[5.0, 10.0]]));
    let csv = h
        .http
        .get(format!(
            "{}{line}/tabs/1/cells?range=A1:B2&format=csv",
            h.url
        ))
        .bearer_auth(&ada.token)
        .send()
        .await
        .unwrap();
    assert_eq!(csv.headers()["content-type"], "text/csv; charset=utf-8");
    assert_eq!(csv.text().await.unwrap(), "5,10\r\n8,13\r\n");
}

#[tokio::test]
async fn versions_named_compared_restored_and_selective_undo() {
    let h = Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.silicon("si:bob", "Bob");
    let id = new_sheet_id(&h, &ada, "Versions", None).await;
    let line = format!("/v1/sheets/{id}");
    share(&h, &ada, &format!("sheets/{id}"), &["si:bob"], "editor").await;
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["1","2"]]}]),
    )
    .await;
    expect(
        &h.post(
            &ada,
            &format!("{line}/versions/1/name"),
            json!({"name":"Baseline"}),
        )
        .await,
        200,
    );
    edit(
        &h,
        &bob,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"A1","input":"9"}]}]),
    )
    .await;
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"B1","input":"7"}]}]),
    )
    .await;
    expect(
        &h.post(
            &ada,
            &format!("{line}/versions/0/name"),
            json!({"name":"Empty baseline"}),
        )
        .await,
        200,
    );
    let initial = expect(
        &h.get(&ada, &format!("{line}/versions?cursor=1&limit=1"))
            .await,
        200,
    )
    .clone();
    assert_eq!(initial["versions"][0]["version"], 0);
    assert_eq!(initial["versions"][0]["name"], "Empty baseline");
    assert!(initial["next_cursor"].is_null());
    let old = expect(
        &h.get(
            &ada,
            &format!("{line}/versions/1/tabs/1/cells?range=A1:B1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(old["values"], json!([[1.0, 2.0]]));
    let diff = expect(
        &h.get(&ada, &format!("{line}/compare?from=1&to=head")).await,
        200,
    )
    .clone();
    assert_eq!(diff["cells_changed"], 2);
    let page = expect(&h.get(&ada, &format!("{line}/versions?limit=1")).await, 200).clone();
    assert_eq!(page["versions"].as_array().unwrap().len(), 1);
    assert!(page["next_cursor"].is_string());
    let names = expect(&h.get(&ada, &format!("{line}/versions")).await, 200).clone();
    assert!(
        names["versions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["version"] == 1 && v["name"] == "Baseline")
    );
    expect(
        &h.post(
            &ada,
            &format!("{line}/versions/1/undo-account"),
            json!({"account":"si:bob"}),
        )
        .await,
        200,
    );
    assert_eq!(values(&h, &ada, &line, "A1:B1").await, json!([[1.0, 7.0]]));
    expect(
        &h.post(
            &ada,
            &format!("{line}/versions/1/restore"),
            json!({"tab":1,"range":"B1"}),
        )
        .await,
        200,
    );
    assert_eq!(values(&h, &ada, &line, "A1:B1").await, json!([[1.0, 2.0]]));
    let copied = expect(
        &h.post(
            &ada,
            &format!("{line}/versions/1/new-sheet"),
            json!({"title":"Recovered"}),
        )
        .await,
        201,
    )
    .clone();
    assert_eq!(copied["title"], "Recovered");
    assert_eq!(copied["version"], 0);
    assert_eq!(
        values(
            &h,
            &ada,
            &format!("/v1/sheets/{}", copied["id"].as_str().unwrap()),
            "A1:B1"
        )
        .await,
        json!([[1.0, 2.0]])
    );
    expect_error(
        &h.get(&ada, &format!("{line}/versions/999/tabs")).await,
        404,
        "no_such_version",
    );
}

#[tokio::test]
async fn copies_conflicts_partial_merge_nested_and_closed_history() {
    let h = Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.silicon("si:bob", "Bob");
    let id = new_sheet_id(&h, &ada, "Copies", None).await;
    let line = format!("/v1/sheets/{id}");
    share(&h, &ada, &format!("sheets/{id}"), &["si:bob"], "editor").await;
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["1","2"]]}]),
    )
    .await;
    let copy = expect(
        &h.post(
            &bob,
            &format!("{line}/copies"),
            json!({"name":"Experiment"}),
        )
        .await,
        201,
    )
    .clone();
    let cid = copy["id"].as_str().unwrap();
    let copied = format!("{line}/copies/{cid}");
    assert_eq!(copy["forked_at"], 1);
    assert!(copy["parent_id"].is_null());
    edit(
        &h,
        &bob,
        &copied,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["10","20"]]}]),
    )
    .await;
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"A1","input":"5"}]}]),
    )
    .await;
    let comp = expect(&h.get(&ada, &format!("{copied}/compare")).await, 200).clone();
    assert_eq!(comp["parent_changed"], true);
    assert_eq!(comp["conflicts"].as_array().unwrap().len(), 1);
    assert_eq!(comp["conflicts"][0]["a"], "A1");
    let dry = expect(
        &h.post(
            &bob,
            &format!("{copied}/merge"),
            json!({"dry_run":true,"default":"sheet"}),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(dry["cells_taken"], 1);
    assert_eq!(values(&h, &ada, &line, "B1").await, json!([[2.0]]));
    expect(
        &h.post(
            &bob,
            &format!("{copied}/merge"),
            json!({"range":{"tab":1,"range":"B1"}}),
        )
        .await,
        200,
    );
    assert_eq!(values(&h, &ada, &line, "A1:B1").await, json!([[5.0, 20.0]]));
    let nested = expect(
        &h.post(&bob, &format!("{copied}/copies"), json!({"name":"Nested"}))
            .await,
        201,
    )
    .clone();
    assert_eq!(nested["parent_id"], cid);
    expect(
        &h.post(
            &bob,
            &format!("{copied}/merge"),
            json!({"resolutions":[{"tab":1,"a":"A1","take":"sheet"}],"close":true}),
        )
        .await,
        200,
    );
    expect_error(
        &h.post(
            &bob,
            &format!("{copied}/edits"),
            json!({"edits":[{"type":"set","tab":1,"cells":[{"a":"A1","input":"99"}]}]}),
        )
        .await,
        409,
        "conflict",
    );
    assert_eq!(
        values(&h, &ada, &copied, "A1:B1").await,
        json!([[10.0, 20.0]])
    );
    assert!(!tings_for(&h, "calc.copy.merged", &ada).await.is_empty());
    expect(
        &h.delete(
            &ada,
            &format!("{line}/copies/{}", nested["id"].as_str().unwrap()),
        )
        .await,
        204,
    );
    let all = expect(
        &h.get(&ada, &format!("{line}/copies?status=all")).await,
        200,
    )
    .clone();
    assert_eq!(all["copies"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn comments_follow_moved_cells_and_protections_enforce_roles() {
    let h = Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.silicon("si:bob", "Bob");
    let id = new_sheet_id(&h, &ada, "Comments", None).await;
    let line = format!("/v1/sheets/{id}");
    share(&h, &ada, &format!("sheets/{id}"), &["si:bob"], "editor").await;
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["1"],["2"]]}]),
    )
    .await;
    let thread = expect(
        &h.post(
            &ada,
            &format!("{line}/comments"),
            json!({"tab":1,"range":"A2","body":"@si:bob check this"}),
        )
        .await,
        201,
    )
    .clone();
    let tid = thread["id"].as_str().unwrap();
    assert_eq!(tings_for(&h, "calc.mention.received", &bob).await.len(), 1);
    edit(
        &h,
        &ada,
        &line,
        json!([{"type":"insert_rows","tab":1,"at":0,"count":1}]),
    )
    .await;
    let moved = expect(&h.get(&ada, &format!("/v1/comments/{tid}")).await, 200).clone();
    assert_eq!(moved["range"], "A3");
    let reply = expect(
        &h.post(
            &bob,
            &format!("/v1/comments/{tid}/replies"),
            json!({"body":"Checked"}),
        )
        .await,
        201,
    )
    .clone();
    expect(
        &h.post(
            &ada,
            &format!("/v1/comments/{}/reactions", reply["id"].as_str().unwrap()),
            json!({"emoji":"👍"}),
        )
        .await,
        200,
    );
    expect(
        &h.post(&bob, &format!("/v1/comments/{tid}/resolve"), json!({}))
            .await,
        200,
    );
    expect(
        &h.post(&ada, &format!("/v1/comments/{tid}/reopen"), json!({}))
            .await,
        200,
    );
    edit(&h,&ada,&line,json!([{"type":"protect","protection":{"tab":1,"range":"A3","editors":[],"description":"Original"}}])).await;
    expect_error(
        &h.post(
            &bob,
            &format!("{line}/edits"),
            json!({"edits":[{"type":"set","tab":1,"cells":[{"a":"A3","input":"9"}]}]}),
        )
        .await,
        403,
        "protected",
    );
    let cells = expect(
        &h.get(&ada, &format!("{line}/tabs/1/cells?range=A3")).await,
        200,
    )
    .clone();
    let protection_id = cells["protections"][0]["id"].as_str().unwrap();
    expect_error(
        &h.post(
            &bob,
            &format!("{line}/edits"),
            json!({"edits":[{"type":"unprotect","id":protection_id}]}),
        )
        .await,
        403,
        "not_allowed",
    );
    let presence = h
        .post(
            &bob,
            &format!("{line}/presence"),
            json!({"tab":1,"range":"A3","client_id":"browser"}),
        )
        .await;
    expect(&presence, 204);
    let events = h
        .http
        .get(format!("{}{line}/events?since=0", h.url))
        .bearer_auth(&ada.token)
        .send()
        .await
        .unwrap();
    assert_eq!(events.status().as_u16(), 200);
    assert!(
        events.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    use futures::StreamExt;
    let mut stream = events.bytes_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&first).contains("event: change"));
}

#[tokio::test]
async fn selective_undo_keeps_later_edits_in_created_rows_and_tabs() {
    let h = Harness::start().await;
    let ada = h.carbon("c:ada", "Ada");
    let bob = h.silicon("si:bob", "Bob");
    let id = new_sheet_id(&h, &ada, "Selective", None).await;
    let line = format!("/v1/sheets/{id}");
    share(&h, &ada, &format!("sheets/{id}"), &["si:bob"], "editor").await;
    edit(&h,&bob,&line,json!([{"type":"set","tab":1,"at":"A1","rows":[["10","20"]]},{"type":"add_tab","name":"New"}])).await;
    edit(&h,&ada,&line,json!([{"type":"set","tab":1,"cells":[{"a":"B1","input":"20"}]},{"type":"set","tab":"New","cells":[{"a":"A1","input":"Keep"}]}])).await;
    expect(
        &h.post(
            &ada,
            &format!("{line}/versions/0/undo-account"),
            json!({"account":"si:bob"}),
        )
        .await,
        200,
    );
    assert_eq!(
        values(&h, &ada, &line, "A1:B1").await,
        json!([[null, 20.0]])
    );
    let kept = expect(
        &h.get(
            &ada,
            &format!("{line}/tabs/New/cells?range=A1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(kept["values"], json!([["Keep"]]));
    edit(&h, &ada, &line, json!([{"type":"undo"}])).await;
    assert_eq!(
        values(&h, &ada, &line, "A1:B1").await,
        json!([[10.0, 20.0]])
    );
    edit(&h, &ada, &line, json!([{"type":"redo"}])).await;
    assert_eq!(
        values(&h, &ada, &line, "A1:B1").await,
        json!([[null, 20.0]])
    );
}

#[tokio::test]
async fn historical_mutations_obey_roles_and_restores_keep_layout() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let editor = h.silicon("si:editor", "Editor");
    let viewer = h.carbon("c:viewer", "Viewer");
    let id = new_sheet_id(&h, &owner, "Roles", None).await;
    let line = format!("/v1/sheets/{id}");
    share(
        &h,
        &owner,
        &format!("sheets/{id}"),
        &["si:editor"],
        "editor",
    )
    .await;
    share(&h, &owner, &format!("sheets/{id}"), &["c:viewer"], "viewer").await;
    for suffix in [
        "copies",
        "versions/0/restore",
        "versions/0/new-sheet",
        "versions/0/name",
    ] {
        expect_error(
            &h.post(
                &viewer,
                &format!("{line}/{suffix}"),
                json!({"name":"Denied","title":"Denied"}),
            )
            .await,
            403,
            "not_allowed",
        );
    }
    edit(&h,&owner,&line,json!([{"type":"set","tab":1,"at":"A1","rows":[["1","2"]]}, {"type":"tab_props","tab":1,"frozen_rows":1,"color":"#123456"},{"type":"col_width","tab":1,"cols":"A","width":190},{"type":"row_height","tab":1,"rows":"1","height":40}])).await;
    let thread = expect(
        &h.post(
            &owner,
            &format!("{line}/comments"),
            json!({"tab":1,"range":"A1","body":"Original"}),
        )
        .await,
        201,
    )
    .clone();
    edit(&h,&owner,&line,json!([{"type":"tab_props","tab":1,"frozen_rows":0,"color":null},{"type":"col_width","tab":1,"cols":"A","width":null},{"type":"insert_rows","tab":1,"at":0,"count":1}])).await;
    expect(
        &h.post(&editor, &format!("{line}/versions/1/restore"), json!({}))
            .await,
        200,
    );
    let cells = expect(
        &h.get(&owner, &format!("{line}/tabs/1/cells?range=A1:B2"))
            .await,
        200,
    )
    .clone();
    assert_eq!(cells["frozen_rows"], 1);
    assert_eq!(cells["tab"]["color"], "#123456");
    assert_eq!(cells["col_widths"]["A"], 190.0);
    assert_eq!(cells["row_heights"]["1"], 40.0);
    let moved = expect(
        &h.get(
            &owner,
            &format!("/v1/comments/{}", thread["id"].as_str().unwrap()),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(moved["range"], "A1");
    assert_eq!(moved["detached"], false);
    edit(&h,&owner,&line,json!([{"type":"protect","protection":{"tab":1,"range":"A1","editors":[],"description":"Keep original"}}])).await;
    expect_error(
        &h.post(&editor, &format!("{line}/versions/1/restore"), json!({}))
            .await,
        403,
        "not_allowed",
    );
    let copy = expect(
        &h.post(
            &owner,
            &format!("{line}/copies"),
            json!({"name":"Read-only after close"}),
        )
        .await,
        201,
    )
    .clone();
    let copyline = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    expect(
        &h.post(&owner, &format!("{copyline}/merge"), json!({"close":true}))
            .await,
        200,
    );
    expect_error(
        &h.post(
            &owner,
            &format!("{copyline}/comments"),
            json!({"tab":1,"range":"A1","body":"No"}),
        )
        .await,
        409,
        "conflict",
    );
    expect(
        &h.get(&viewer, &format!("{copyline}/versions/0/tabs")).await,
        200,
    );
}

#[tokio::test]
async fn partial_copy_merge_extends_the_parent_grid() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let id = new_sheet_id(&h, &owner, "Partial", None).await;
    let line = format!("/v1/sheets/{id}");
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"A1","input":"1"}]}]),
    )
    .await;
    let copy = expect(
        &h.post(
            &owner,
            &format!("{line}/copies"),
            json!({"name":"Add notes"}),
        )
        .await,
        201,
    )
    .clone();
    let cp = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    edit(&h,&owner,&cp,json!([{"type":"set","tab":1,"cells":[{"a":"F1","input":"note"},{"a":"F8","input":"Corrected"},{"a":"E2","input":"outside"}]}])).await;
    let merged = expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":1,"range":"F1:F8"}}),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(merged["cells_taken"], 2);
    assert_eq!(values(&h, &owner, &line, "F1").await, json!([["note"]]));
    assert_eq!(
        values(&h, &owner, &line, "F8").await,
        json!([["Corrected"]])
    );
    assert_eq!(values(&h, &owner, &line, "E2").await, json!([[null]]));
}

#[tokio::test]
async fn repeated_copy_merges_keep_new_tabs_rows_and_later_parent_edits() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let id = new_sheet_id(&h, &owner, "Repeat merges", None).await;
    let line = format!("/v1/sheets/{id}");
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["1"],["2"]]}]),
    )
    .await;
    let copy = expect(
        &h.post(
            &owner,
            &format!("{line}/copies"),
            json!({"name":"Open work"}),
        )
        .await,
        201,
    )
    .clone();
    let cp = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"add_tab","name":"Parent notes"}]),
    )
    .await;
    edit(&h,&owner,&cp,json!([{"type":"add_tab","name":"Source data"},{"type":"set","tab":2,"at":"A1","rows":[["7","=A1*2"]]},{"type":"insert_rows","tab":1,"at":1,"count":1},{"type":"set","tab":1,"cells":[{"a":"A2","input":"3"}]}])).await;
    let first = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(first["tabs_added"], json!(["Source data"]));
    assert_eq!(
        values(&h, &owner, &line, "A1:A3").await,
        json!([[1.0], [3.0], [2.0]])
    );
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"A2","input":"99"}]}]),
    )
    .await;
    h.state.lines.discard(&main_line(&id)).await;
    h.state.lines.discard(copy["id"].as_str().unwrap()).await;
    let repeated = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(repeated["cells_taken"], 0);
    assert_eq!(repeated["tabs_added"], json!([]));
    assert_eq!(
        values(&h, &owner, &line, "A1:A3").await,
        json!([[1.0], [99.0], [2.0]])
    );
    let tabs = expect(&h.get(&owner, &format!("{line}/tabs")).await, 200).clone();
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 3);
    let dest = tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Source data")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    assert_ne!(dest, 2);
    edit(
        &h,
        &owner,
        &cp,
        json!([{"type":"set","tab":2,"cells":[{"a":"A1","input":"11"}]}]),
    )
    .await;
    let next = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(next["tabs_added"], json!([]));
    let cells = expect(
        &h.get(
            &owner,
            &format!("{line}/tabs/{dest}/cells?range=A1:B1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(cells["values"], json!([[11.0, 22.0]]));
    assert_eq!(values(&h, &owner, &line, "A2").await, json!([[99.0]]));
    let compare = expect(&h.get(&owner, &format!("{cp}/compare")).await, 200).clone();
    assert_eq!(compare["diff"]["tabs"], json!([]));
    assert_eq!(compare["parent_changed"], false);
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":dest,"cells":[{"a":"A1","input":"12"}]}]),
    )
    .await;
    edit(
        &h,
        &owner,
        &cp,
        json!([{"type":"set","tab":2,"cells":[{"a":"A1","input":"13"}]}]),
    )
    .await;
    expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({"default":"sheet"}))
            .await,
        200,
    );
    let repeated = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(repeated["cells_taken"], 0);
    let cells = expect(
        &h.get(
            &owner,
            &format!("{line}/tabs/{dest}/cells?range=A1:B1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(cells["values"], json!([[12.0, 24.0]]));
    let historical = expect(&h.get(&owner, &format!("{cp}/versions/0/tabs")).await, 200).clone();
    assert_eq!(historical["tabs"].as_array().unwrap().len(), 1);
    expect(&h.delete(&owner, &line).await, 204);
    expect(
        &h.delete(&owner, &format!("{line}/forever?confirm=Repeat%20merges"))
            .await,
        204,
    );
    let remaining: bool =
        sqlx::query_scalar("select exists(select 1 from merge_receipts where line_id=$1)")
            .bind(copy["id"].as_str().unwrap())
            .fetch_one(&h.state.db)
            .await
            .unwrap();
    assert!(!remaining);
}

#[tokio::test]
async fn partial_then_full_merge_accepts_only_selected_changes() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let id = new_sheet_id(&h, &owner, "Partial receipts", None).await;
    let line = format!("/v1/sheets/{id}");
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["1"],["2"]]}]),
    )
    .await;
    let copy = expect(
        &h.post(&owner, &format!("{line}/copies"), json!({"name":"Ranges"}))
            .await,
        201,
    )
    .clone();
    let cp = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    edit(&h,&owner,&cp,json!([{"type":"insert_rows","tab":1,"at":1,"count":1},{"type":"set","tab":1,"at":"A2","rows":[["3","pending"]]},{"type":"add_tab","name":"Added"},{"type":"set","tab":2,"at":"A1","rows":[["10","20"]]}])).await;
    expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":1,"range":"A2"}}),
        )
        .await,
        200,
    );
    assert_eq!(
        values(&h, &owner, &line, "A1:B3").await,
        json!([[1.0, null], [3.0, null], [2.0, null]])
    );
    expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":2,"range":"A1"}}),
        )
        .await,
        200,
    );
    let partial = expect(
        &h.get(
            &owner,
            &format!("{line}/tabs/2/cells?range=A1:B1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(partial["values"], json!([[10.0, null]]));
    edit(&h,&owner,&line,json!([{"type":"set","tab":1,"cells":[{"a":"A2","input":"99"}]},{"type":"set","tab":2,"cells":[{"a":"A1","input":"88"}]}])).await;
    expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    );
    assert_eq!(
        values(&h, &owner, &line, "A1:B3").await,
        json!([[1.0, null], [99.0, "pending"], [2.0, null]])
    );
    let full = expect(
        &h.get(
            &owner,
            &format!("{line}/tabs/2/cells?range=A1:B1&format=values"),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(full["values"], json!([[88.0, 20.0]]));
    let again = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(again["cells_taken"], 0);
    assert_eq!(again["tabs_added"], json!([]));
    let tabs = expect(&h.get(&owner, &format!("{line}/tabs")).await, 200).clone();
    assert_eq!(tabs["tabs"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn partial_formula_merge_tracks_references_and_consumes_the_change_once() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let id = new_sheet_id(&h, &owner, "Formula merge", None).await;
    let line = format!("/v1/sheets/{id}");
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"at":"B1","rows":[["10"],["20"]]}]),
    )
    .await;
    let copy = expect(
        &h.post(&owner, &format!("{line}/copies"), json!({"name":"Shorter"}))
            .await,
        201,
    )
    .clone();
    let cp = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    edit(&h,&owner,&cp,json!([{"type":"delete_rows","tab":1,"at":0,"count":1},{"type":"set","tab":1,"cells":[{"a":"A1","input":"=$B$1"}]}])).await;
    expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":1,"range":"A1"}}),
        )
        .await,
        200,
    );
    assert_eq!(
        values(&h, &owner, &line, "A2:B2").await,
        json!([[20.0, 20.0]])
    );
    let repeated = expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":1,"range":"A1"}}),
        )
        .await,
        200,
    )
    .clone();
    assert_eq!(repeated["cells_taken"], 0);
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"cells":[{"a":"B2","input":"30"}]}]),
    )
    .await;
    expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    );
    assert_eq!(
        values(&h, &owner, &line, "A1:B1").await,
        json!([[30.0, 30.0]])
    );
}

#[tokio::test]
async fn partial_formula_with_unselected_new_tab_remains_pending_until_full_merge() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let id = new_sheet_id(&h, &owner, "Pending formula", None).await;
    let line = format!("/v1/sheets/{id}");
    edit(
        &h,
        &owner,
        &line,
        json!([{"type":"set","tab":1,"at":"A1","rows":[["10"]]}]),
    )
    .await;
    let copy = expect(
        &h.post(&owner, &format!("{line}/copies"), json!({"name":"Future"}))
            .await,
        201,
    )
    .clone();
    let cp = format!("{line}/copies/{}", copy["id"].as_str().unwrap());
    edit(&h,&owner,&cp,json!([{"type":"add_tab","name":"Future"},{"type":"set","tab":2,"at":"A1","rows":[["7"]]},{"type":"set","tab":1,"at":"A1","rows":[["=Future!A1*2"]]}])).await;
    expect(
        &h.post(
            &owner,
            &format!("{cp}/merge"),
            json!({"range":{"tab":1,"range":"A1"}}),
        )
        .await,
        200,
    );
    let pending = expect(&h.get(&owner, &format!("{cp}/compare")).await, 200).clone();
    assert!(
        pending["diff"]["cells"]
            .as_array()
            .unwrap()
            .iter()
            .any(|cell| cell["tab"] == 1 && cell["a"] == "A1")
    );
    expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    );
    assert_eq!(values(&h, &owner, &line, "A1").await, json!([[14.0]]));
    let repeated = expect(
        &h.post(&owner, &format!("{cp}/merge"), json!({})).await,
        200,
    )
    .clone();
    assert_eq!(repeated["cells_taken"], 0);
    assert_eq!(repeated["tabs_added"], json!([]));
}
