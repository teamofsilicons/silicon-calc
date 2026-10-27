mod support;
use axum::http::StatusCode;
use serde_json::json;
use support::Harness;
#[tokio::test]
async fn functions_are_scoped_and_browser_results_require_edit_and_exact_version() {
    let h=Harness::start().await;
    let owner=h.carbon("c:owner","Owner");let reader=h.carbon("c:reader","Reader");
    let f=json!({"name":"DOUBLE_VALUE","parameters":["value"],"code":"return value * 2;"});
    let (status,saved)=h.put(&owner,"/v1/me/functions/DOUBLE_VALUE",f.clone()).await;
    assert_eq!(status,StatusCode::OK,"{saved}");
    assert_eq!(h.get(&reader,"/v1/me/functions").await.1["functions"],json!([]));
    assert_eq!(h.get_anon("/v1/me/functions").await.0,StatusCode::UNAUTHORIZED);
    let (_,s)=h.post(&owner,"/v1/sheets",json!({"title":"JS"})).await;let base=format!("/v1/sheets/{}",s["id"].as_str().unwrap());
    let (status,body)=h.put(&owner,&format!("{base}/functions"),json!({"function":f,"base_version":0})).await;
    assert_eq!(status,StatusCode::OK,"{body}");
    h.post(&owner,&format!("{base}/edits"),json!({"edits":[{"type":"set","tab":1,"at":"A1","rows":[["5","=DOUBLE_VALUE(A1)","=B1+1"]]}]})).await;
    let (_,state)=h.get(&owner,&format!("{base}/functions")).await;
    let results=json!({"base_version":state["version"],"epoch":state["epoch"],"results":[[state["calls"][0]["id"],{"rows":[[{"t":"number","v":10}]],"dependencies":{"reads":[],"tabs":false}}]]});
    h.put(&owner,&format!("{base}/public"),json!({"enabled":true,"access":"view"})).await;
    assert_eq!(h.post(&reader,&format!("{base}/javascript/results"),results.clone()).await.0,StatusCode::FORBIDDEN);
    let (status,answer)=h.post(&owner,&format!("{base}/javascript/results"),results.clone()).await;
    assert_eq!(status,StatusCode::OK,"{answer}");
    let (_,cells)=h.get(&owner,&format!("{base}/tabs/1/cells?range=A1:C1&format=values")).await;
    assert_eq!(cells["values"],json!([[5.0,10.0,11.0]]));
    let (status,_) = h.post(&owner,&format!("{base}/edits"),json!({"edits":[{"type":"set","tab":1,"at":"D10","rows":[["3"]]},{"type":"style","tab":1,"range":"A1:C1","patch":{"bold":true}}]})).await;
    assert_eq!(status,StatusCode::OK);
    let (_,kept)=h.get(&owner,&format!("{base}/tabs/1/cells?range=A1:C1&format=values")).await;
    assert_eq!(kept["values"],json!([[5.0,10.0,11.0]]));
    let (_,functions)=h.get(&owner,&format!("{base}/functions")).await;
    assert_eq!(functions["calls"][0]["id"],state["calls"][0]["id"]);
    assert_eq!(functions["calls"][0]["result"]["dependencies"],json!({"reads":[],"tabs":false}));
    assert_eq!(h.post(&owner,&format!("{base}/javascript/results"),results).await.0,StatusCode::CONFLICT);
    let (_,copy)=h.post(&owner,&format!("{base}/copies"),json!({"name":"JS copy"})).await;
    let (_,copied)=h.get(&owner,&format!("{base}/copies/{}/functions",copy["id"].as_str().unwrap())).await;
    assert_eq!(copied["functions"][0]["name"],"DOUBLE_VALUE");
}
