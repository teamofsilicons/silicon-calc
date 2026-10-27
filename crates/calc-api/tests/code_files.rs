mod support;
use axum::http::StatusCode;
use serde_json::json;
use support::Harness;

#[tokio::test]
async fn browser_results_are_literal_versioned_and_never_executed() {
    let h = Harness::start().await;
    let owner = h.carbon("c:writer", "Writer");
    let viewer = h.carbon("c:reader", "Reader");
    let (status, sheet) = h
        .upload(
            &owner,
            "/v1/sheets/import?filename=Sales.csv",
            b"region,amount\nEU,10\nUS,20\n".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sheet}");
    let id = sheet["id"].as_str().unwrap();
    let tab = sheet["tabs"][0]["id"].clone();
    let path = format!("/v1/sheets/{id}");
    let (status, search) = h.get(&owner, "/v1/search?q=EU").await;
    assert_eq!(status, StatusCode::OK, "{search}");
    assert!(
        search["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["sheet"]["id"] == id)
    );
    // This source would fail if executed. The server stores it as metadata only.
    let body = json!({"code":"throw new Error('must never execute on server')",
        "base_version":sheet["version"],"inputs":[tab],"columns":["total","literal","flag","blank"],
        "rows":[[30,"=1+1",true,null],[60,"00123",false,null]],"into":{"new_tab":"Totals"}});
    let (status, result) = h.post(&owner, &format!("{path}/run"), body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["rows"], body["rows"]);
    assert_eq!(result["tab"]["code"]["language"], "javascript");
    assert!(result["tab"]["code"].get("keep_updated").is_none());
    let derived = result["tab"]["id"].clone();
    let (_, cells) = h
        .get(
            &owner,
            &format!("{path}/tabs/{derived}/cells?range=A2:D3&format=values"),
        )
        .await;
    assert_eq!(
        cells["values"],
        json!([[30.0, "=1+1", true, null], [60.0, "00123", false, null]])
    );
    let (status, stale) = h.post(&owner, &format!("{path}/run"), body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    let replacement = json!({"code":"return [{total:90}]", "base_version":result["version"],
        "inputs":[tab],"columns":["total"],"rows":[[90]],"into":{"tab":derived,"at":"A1","replace":true}});
    let (status, replaced) = h.post(&owner, &format!("{path}/run"), replacement).await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    let (_, cells) = h
        .get(
            &owner,
            &format!("{path}/tabs/{derived}/cells?range=A2:D3&format=values"),
        )
        .await;
    assert_eq!(
        cells["values"],
        json!([[90.0, null, null, null], [null, null, null, null]])
    );
    let (status, changed) = h
        .post(
            &owner,
            &format!("{path}/edits"),
            json!({"edits":[{"type":"values","tab":tab,"at":"B2","rows":[[40]]}]}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    // Inputs changing never starts background execution.
    let (_, cells) = h
        .get(
            &owner,
            &format!("{path}/tabs/{derived}/cells?range=A2&format=values"),
        )
        .await;
    assert_eq!(cells["values"], json!([[90.0]]));
    let (status, removed) = h
        .post(&owner, &format!("{path}/tabs/{derived}/rerun"), json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{removed}");
    let (status, share) = h
        .post(
            &owner,
            &format!("{path}/sharing"),
            json!({"invite":[viewer.id],"role":"viewer","notify":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    let (status, denied) = h.post(&viewer, &format!("{path}/run"), body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{denied}");
    let (status, bytes) = h
        .download(&owner, &format!("{path}/download?format=xlsx"))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(bytes.starts_with(b"PK"));
    let (status, imported) = h
        .upload(
            &owner,
            &format!("{path}/tabs/import?filename=again.xlsx&mode=new_tab"),
            bytes,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["tabs"].as_array().unwrap().len(), 4);
}

#[tokio::test]
async fn browser_result_validation_rejects_legacy_runtimes_and_malformed_cells() {
    let h = Harness::start().await;
    let owner = h.carbon("c:writer", "Writer");
    let (_, sheet) = h
        .upload(
            &owner,
            "/v1/sheets/import?filename=data.csv",
            b"a\n1\n".to_vec(),
        )
        .await;
    let path = format!("/v1/sheets/{}", sheet["id"].as_str().unwrap());
    let body = json!({"code":"return [[1]]", "base_version":sheet["version"],"inputs":[sheet["tabs"][0]["id"]],
        "columns":["a"],"rows":[[1]],"into":{"new_tab":"Result"}});
    for (field, value) in [
        ("language", json!("python")),
        ("keep_updated", json!(true)),
        ("rows", json!([[{"t":"s","v":"bad"}]])),
        ("rows", json!([[1, 2]])),
        ("columns", json!([])),
        ("inputs", json!([999999])),
        ("base_version", json!(-1)),
        ("into", json!({"new_tab":"Result","tab":1})),
        ("into", json!({"tab":1,"replace":true,"at":"B2"})),
        ("into", json!({"tab":1,"at":"A0"})),
    ] {
        let mut bad = body.clone();
        bad[field] = value;
        let (status, error) = h.post(&owner, &format!("{path}/run"), bad).await;
        assert!(status.is_client_error(), "{field}: {error}");
    }
    let mut missing = body.clone();
    missing.as_object_mut().unwrap().remove("base_version");
    assert!(
        h.post(&owner, &format!("{path}/run"), missing)
            .await
            .0
            .is_client_error()
    );
    let mut large = body.clone();
    large["rows"] = json!(vec![vec![0]; 200_000]);
    let (status, error) = h.post(&owner, &format!("{path}/run"), large).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{error}");
    let (status, ok) = h.post(&owner, &format!("{path}/run"), body).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok["version"], sheet["version"].as_i64().unwrap() + 1);
}

#[tokio::test]
async fn browser_result_commit_respects_protection_for_writes_and_replacement() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let editor = h.carbon("c:editor", "Editor");
    let (_, sheet) = h
        .upload(
            &owner,
            "/v1/sheets/import?filename=data.csv",
            b"a\n1\n".to_vec(),
        )
        .await;
    let path = format!("/v1/sheets/{}", sheet["id"].as_str().unwrap());
    let tab = sheet["tabs"][0]["id"].clone();
    let (status, share) = h
        .post(
            &owner,
            &format!("{path}/sharing"),
            json!({"invite":[editor.id],"role":"editor","notify":false}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{share}");
    let (status,protected)=h.post(&owner,&format!("{path}/edits"),json!({"edits":[{"type":"protect","protection":{"tab":tab,"range":"A1","editors":[],"description":"Header"}}]})).await;
    assert_eq!(status, StatusCode::OK, "{protected}");
    for into in [
        json!({"tab":tab,"at":"A1"}),
        json!({"tab":tab,"replace":true}),
    ] {
        let (status,error)=h.post(&editor,&format!("{path}/run"),json!({"code":"return [[2]]","base_version":protected["version"],"inputs":[],"columns":["changed"],"rows":[[2]],"into":into})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{error}");
        assert_eq!(error["error"]["code"], "protected");
    }
}

#[tokio::test]
async fn download_version_and_replace_import_are_versioned() {
    let h = Harness::start().await;
    let owner = h.carbon("c:owner", "Owner");
    let (_, sheet) = h
        .upload(
            &owner,
            "/v1/sheets/import?filename=data.csv",
            b"a,b\n1,2\n".to_vec(),
        )
        .await;
    let id = sheet["id"].as_str().unwrap();
    let tab = sheet["tabs"][0]["id"].clone();
    let (status, changed) = h
        .upload(
            &owner,
            &format!("/v1/sheets/{id}/tabs/import?filename=updated.csv&mode=replace&tab={tab}"),
            b"a,b\n8,9\n".to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert!(changed["version"].as_i64().unwrap() > 0);
    for (version, expected) in [("&version=1", "1,2"), ("", "8,9")] {
        let (status, bytes) = h
            .download(
                &owner,
                &format!("/v1/sheets/{id}/download?format=csv{version}"),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert!(String::from_utf8(bytes).unwrap().contains(expected));
    }
}

#[tokio::test]
async fn imported_notes_keep_author_text_and_blank_cell_anchor() {
    let h = Harness::start().await;
    let owner = h.carbon("c:importer", "Importer");
    let (status, sheet) = h
        .upload(
            &owner,
            "/v1/sheets/import?filename=notes.xlsx",
            include_bytes!("files_fixtures/blank-cell-note.xlsx").to_vec(),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sheet}");
    assert_eq!(sheet["version"], 1);
    let id = sheet["id"].as_str().unwrap();
    let (status, notes) = h.get(&owner, &format!("/v1/sheets/{id}/comments")).await;
    assert_eq!(status, StatusCode::OK, "{notes}");
    let text = notes.to_string();
    assert!(text.contains("Ada: Keep this blank-cell note."), "{notes}");
    assert!(text.contains("C5"), "{notes}");
    assert!(!text.contains("\"detached\":true"), "{notes}");
}
