use calc_engine::{Workbook, Op, Value, ErrorKind, snapshot, javascript::{Function, Answer}, ops::{Actor, CellWrite}};
const ACTOR: Actor<'_> = Actor { account: "owner", at: 1_800_000_000 };
fn set(w: &mut Workbook, r: u32, c: u32, input: &str) {
    let op = w.prepare_writes(1, vec![(r,c,CellWrite { input: Some(input.into()), ..Default::default() })]).unwrap();
    w.apply(&op, &ACTOR).unwrap();
}
fn function() -> Function { Function { name: "DOUBLE_VALUE".into(), parameters: vec!["value".into()], code: "return value * 2;".into(), account: None } }
#[test]
fn browser_values_recalculate_dependents_and_persist_without_running_source() {
    let mut w = Workbook::new();
    w.apply(&Op::SetFunctions { functions:vec![function()] }, &ACTOR).unwrap();
    set(&mut w,0,0,"5"); set(&mut w,0,1,"=DOUBLE_VALUE(A1)+2"); set(&mut w,0,2,"=B1*3");
    assert_eq!(w.tab(1).unwrap().value_at(0,1), Value::Error(ErrorKind::Busy));
    let js = w.javascript.lock().unwrap().clone();
    assert_eq!(js.calls.len(),1);
    assert_eq!(js.calls[0].args,vec![serde_json::json!(5.0)]);
    let answer = Answer { rows:vec![vec![Value::Number(10.0)]],error:None,dependencies:None };
    let op = Op::JavascriptResults { epoch:js.epoch,results:vec![(js.calls[0].id,answer.clone())] };
    w.apply(&op,&ACTOR).unwrap();
    assert_eq!(w.tab(1).unwrap().value_at(0,1),Value::Number(12.0));
    assert_eq!(w.tab(1).unwrap().value_at(0,2),Value::Number(36.0));
    let mut copy = snapshot::decode(&snapshot::encode(&w)).unwrap();
    assert_eq!(copy.functions,vec![function()]);
    assert_eq!(copy.tab(1).unwrap().value_at(0,2),Value::Number(36.0));
    set(&mut copy,0,0,"6");
    assert_eq!(copy.tab(1).unwrap().value_at(0,1),Value::Error(ErrorKind::Busy));
    assert!(copy.apply(&op,&ACTOR).is_err());
    let state=copy.javascript.lock().unwrap().clone();
    assert_eq!(state.calls[0].args,vec![serde_json::json!(6.0)]);
}
#[test]
fn inline_arrays_nested_calls_and_unknown_functions() {
    let mut w = Workbook::new();
    set(&mut w,0,0,"=JS(\"return [[1,2],[3,4]];\")");
    let js = w.javascript.lock().unwrap().clone();
    w.apply(&Op::JavascriptResults { epoch:js.epoch,results:vec![(js.calls[0].id,Answer { rows:vec![vec![Value::Number(1.0),Value::Number(2.0)],vec![Value::Number(3.0),Value::Number(4.0)]],error:None,dependencies:None })] },&ACTOR).unwrap();
    assert_eq!(w.tab(1).unwrap().value_at(1,1),Value::Number(4.0));
    set(&mut w,5,5,"=NOT_A_FUNCTION(1)");
    assert_eq!(w.tab(1).unwrap().value_at(5,5),Value::Error(ErrorKind::Name));
    w.apply(&Op::SetFunctions { functions:vec![function()] }, &ACTOR).unwrap();
    set(&mut w,3,0,"=DOUBLE_VALUE(DOUBLE_VALUE(2))");
    let js = w.javascript.lock().unwrap().clone();
    let call=js.calls.iter().find(|c|c.name=="DOUBLE_VALUE").unwrap();
    assert_eq!(call.args,vec![serde_json::json!(2.0)]);
    w.apply(&Op::JavascriptResults { epoch:js.epoch,results:vec![(call.id,Answer { rows:vec![vec![Value::Number(4.0)]],error:None,dependencies:None })] },&ACTOR).unwrap();
    let js=w.javascript.lock().unwrap();
    assert!(js.calls.iter().any(|c|c.name=="DOUBLE_VALUE"&&c.args==vec![serde_json::json!(4.0)]&&c.result.is_none()));
}
#[test]
fn definitions_and_result_shapes_are_bounded() {
    for name in ["SUM","JS","A1","bad-name"] { let mut f=function(); f.name=name.into(); assert!(f.validate().is_err()); }
    let mut f=function();f.parameters=vec!["calc".into()];assert!(f.validate().is_err());
    assert!(Answer{ rows:vec![vec![Value::Number(1.0)],vec![]],error:None,dependencies:None }.validate().is_err());
    assert!(Answer{ rows:vec![vec![Value::Number(f64::INFINITY)]],error:None,dependencies:None }.validate().is_err());
}

fn answer(w: &mut Workbook, col: u32, value: f64, reads: Vec<calc_engine::javascript::Read>, tabs: bool) {
    let js = w.javascript.lock().unwrap().clone();
    let call = js.calls.iter().find(|c| c.col == col && c.result.is_none()).expect("pending call");
    w.apply(&Op::JavascriptResults { epoch: js.epoch, results: vec![(call.id, Answer {
        rows: vec![vec![Value::Number(value)]], error: None,
        dependencies: Some(calc_engine::javascript::Dependencies { reads, tabs }),
    })] }, &ACTOR).unwrap();
}
fn read(range: &str) -> calc_engine::javascript::Read { calc_engine::javascript::Read { tab: 1, range: Some(range.into()) } }
fn value(w: &Workbook, col: u32) -> Value { w.tab(1).unwrap().value_at(0,col) }

#[test]
fn unrelated_edits_and_formatting_keep_cached_results_and_only_changed_arguments_run() {
    let mut w=Workbook::new();
    w.apply(&Op::SetFunctions { functions:vec![function()] }, &ACTOR).unwrap();
    set(&mut w,0,0,"5");set(&mut w,0,1,"=DOUBLE_VALUE(A1)");set(&mut w,0,2,"=B1+1");set(&mut w,0,3,"=JS(\"return 4;\")");
    answer(&mut w,1,10.0,vec![],false);answer(&mut w,3,4.0,vec![],false);
    let ids:Vec<_>=w.javascript.lock().unwrap().calls.iter().map(|c|c.id).collect();
    set(&mut w,9,3,"3");
    let op=w.prepare_style(1,calc_engine::Rect::new(0,0,3,3),calc_engine::style::StylePatch { bold:Some(true),..Default::default() }).unwrap();
    w.apply(&op,&ACTOR).unwrap();
    assert_eq!(value(&w,1),Value::Number(10.0));assert_eq!(value(&w,2),Value::Number(11.0));assert_eq!(value(&w,3),Value::Number(4.0));
    assert_eq!(w.javascript.lock().unwrap().calls.iter().map(|c|c.id).collect::<Vec<_>>(),ids);
    let mut w=snapshot::decode(&snapshot::encode(&w)).unwrap();
    set(&mut w,0,0,"6");
    assert_eq!(value(&w,1),Value::Error(ErrorKind::Busy));assert_eq!(value(&w,3),Value::Number(4.0));
    assert_eq!(w.javascript.lock().unwrap().calls.len(),2,"obsolete argument cache is pruned");
    answer(&mut w,1,12.0,vec![],false);assert_eq!(value(&w,2),Value::Number(13.0));
    let mut f=function();f.code="return value*3;".into();w.apply(&Op::SetFunctions { functions:vec![f] },&ACTOR).unwrap();
    assert_eq!(value(&w,1),Value::Error(ErrorKind::Busy));
}

#[test]
fn observed_reads_propagate_normal_and_javascript_dependencies_without_unrelated_busy() {
    let mut w=Workbook::new();
    set(&mut w,0,0,"5");set(&mut w,0,1,"=A1*2");
    set(&mut w,0,2,"=JS(\"return await calc.cell('B1');\")");
    set(&mut w,0,3,"=JS(\"return (await calc.cell('C1'))+1;\")");
    set(&mut w,0,4,"=D1+1");
    answer(&mut w,2,10.0,vec![read("B1")],false);answer(&mut w,3,11.0,vec![read("C1")],false);
    set(&mut w,9,3,"3");assert_eq!(value(&w,4),Value::Number(12.0));
    let op=w.prepare_style(1,calc_engine::Rect::new(0,1,0,1),calc_engine::style::StylePatch { bold:Some(true),..Default::default() }).unwrap();w.apply(&op,&ACTOR).unwrap();
    assert_eq!(value(&w,2),Value::Number(10.0));
    set(&mut w,0,0,"7");assert_eq!(value(&w,2),Value::Error(ErrorKind::Busy));assert_eq!(value(&w,3),Value::Error(ErrorKind::Busy));
    answer(&mut w,2,14.0,vec![read("B1")],false);answer(&mut w,3,15.0,vec![read("C1")],false);
    assert_eq!(value(&w,4),Value::Number(16.0));
    // Direct reads also invalidate without any formula reference in the AST.
    set(&mut w,0,1,"20");assert_eq!(value(&w,2),Value::Error(ErrorKind::Busy));
}

#[test]
fn tab_metadata_reads_do_not_invalidate_their_own_result() {
    let mut w=Workbook::new();set(&mut w,0,0,"=JS(\"return calc.tabs().length;\")");
    answer(&mut w,0,1.0,vec![],true);assert_eq!(value(&w,0),Value::Number(1.0));
    set(&mut w,5,0,"9");assert_eq!(value(&w,0),Value::Error(ErrorKind::Busy));
    answer(&mut w,0,1.0,vec![],true);assert_eq!(value(&w,0),Value::Number(1.0));
    set(&mut w,5,0,"10");assert_eq!(value(&w,0),Value::Number(1.0));
    w.apply(&w.prepare_add_tab("Another",None),&ACTOR).unwrap();assert_eq!(value(&w,0),Value::Error(ErrorKind::Busy));
}
