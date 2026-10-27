use calc_engine::{addr::CellPos, diff::plan_restore, model::{IdRect, Chart}, ops::{Actor, CellWrite, Op, TabProps}, value::Value, workbook::Workbook};
const ACTOR: Actor<'static> = Actor { account: "c:test", at: 1_800_000_000 };
fn write(w: &mut Workbook, t: u32, a: &str, value: &str) {
    let p = CellPos::parse(a).unwrap();
    let op = w.prepare_writes(t, vec![(p.row, p.col, CellWrite { input: Some(value.into()), clear: true, ..Default::default() })]).unwrap();
    w.apply(&op, &ACTOR).unwrap();
}
fn replay(w: &mut Workbook, op: Op) {
    let encoded = serde_json::to_vec(&op).unwrap();
    let op = serde_json::from_slice(&encoded).unwrap();
    w.apply(&op, &ACTOR).unwrap();
}
#[test]
fn restores_ids_formulas_layout_and_clears_serialized_properties() {
    let mut target = Workbook::new(); let t = target.tabs[0].id;
    write(&mut target, t, "A1", "10"); write(&mut target, t, "A2", "20"); write(&mut target, t, "B2", "=A1+A2");
    target.apply(&Op::TabProps { tab: t, props: TabProps { frozen_rows: Some(1), ..Default::default() } }, &ACTOR).unwrap();
    let ids = target.tab(t).unwrap().rows.ids().to_vec();
    let mut w = target.clone();
    w.apply(&Op::DeleteRows { tab: t, ids: vec![ids[0]] }, &ACTOR).unwrap();
    write(&mut w, t, "A3", "999");
    let col = w.tab(t).unwrap().cols.id_at(0).unwrap();
    w.apply(&Op::ColWidths { tab: t, widths: vec![(col, Some(170.0))] }, &ACTOR).unwrap();
    w.apply(&Op::TabProps { tab: t, props: TabProps { frozen_rows: Some(4), color: Some(Some("#abcdef".into())), ..Default::default() } }, &ACTOR).unwrap();
    let r = w.tab(t).unwrap().rows.id_at(0).unwrap();
    w.apply(&Op::Hide { tab: t, rows: true, ids: vec![r], hidden: true }, &ACTOR).unwrap();
    w.apply(&Op::Merge { tab: t, area: IdRect { r1:r,r2:r,c1:col,c2:col+1 } }, &ACTOR).unwrap();
    w.apply(&Op::PutChart { chart: Chart { id:"new".into(),tab:t,kind:"bar".into(),title:"X".into(),range:"A1:B2".into(),anchor_row:0,anchor_col:0,width:200,height:150 } }, &ACTOR).unwrap();
    let op = plan_restore(&w, &target, None).unwrap().unwrap(); replay(&mut w, op);
    assert_eq!(w.tab(t).unwrap().rows.ids(), ids);
    assert_eq!(w.tab(t).unwrap().value_at(1,1), Value::Number(30.0));
    assert_eq!(w.capture_tab(t), target.capture_tab(t));
    assert_eq!(w.tab(t).unwrap().color, None);
    assert!(w.charts.is_empty());
    assert!(plan_restore(&w, &target, None).unwrap().is_none());
}
#[test]
fn restores_tab_names_order_and_cross_tab_formulas() {
    let mut target = Workbook::new(); let a = target.tabs[0].id;
    target.apply(&Op::RenameTab { tab:a,name:"Sales".into() }, &ACTOR).unwrap();
    target.apply(&Op::AddTab { tab:2,name:"Totals".into(),index:None }, &ACTOR).unwrap();
    write(&mut target,a,"A1","15"); write(&mut target,2,"A1","=Sales!A1*2");
    let mut w=target.clone(); w.apply(&Op::DeleteTab {tab:a},&ACTOR).unwrap();
    w.apply(&Op::RenameTab{tab:2,name:"Sales".into()},&ACTOR).unwrap();
    let op=plan_restore(&w,&target,None).unwrap().unwrap(); replay(&mut w,op);
    assert_eq!(w.tabs.iter().map(|t| (&t.name,t.id)).collect::<Vec<_>>(),target.tabs.iter().map(|t| (&t.name,t.id)).collect::<Vec<_>>());
    assert_eq!(w.tab(2).unwrap().value_at(0,0), Value::Number(30.0));
}

#[test]
fn copying_layout_changes_preserves_unrelated_parent_layout() {
    use calc_engine::diff::{plan_merge, MergeChoice};
    let mut base=Workbook::new(); let t=base.tabs[0].id;
    write(&mut base,t,"A1","10"); write(&mut base,t,"B2","20");
    base.apply(&Op::ColWidths{tab:t,widths:vec![(0,Some(120.0))]},&ACTOR).unwrap();
    let mut copy=base.clone(); let mut parent=base.clone();
    copy.apply(&Op::ColWidths{tab:t,widths:vec![(0,None)]},&ACTOR).unwrap();
    copy.apply(&Op::TabProps{tab:t,props:TabProps {frozen_rows:Some(1),color:Some(Some("#ff0000".into())),..Default::default()}},&ACTOR).unwrap();
    copy.apply(&Op::Hide{tab:t,rows:true,ids:vec![1],hidden:true},&ACTOR).unwrap();
    parent.apply(&Op::RowHeights{tab:t,heights:vec![(0,Some(33.0))]},&ACTOR).unwrap();
    assert!(!calc_engine::diff::diff(&base,&copy,100,None).is_empty());
    let plan=plan_merge(&base,&parent,&copy,&MergeChoice::default()).unwrap();
    replay(&mut parent,plan.op.unwrap());
    let tab=parent.tab(t).unwrap();
    assert!(tab.col_widths.is_empty()); assert_eq!(tab.row_heights.get(&0),Some(&33.0));
    assert_eq!(tab.hidden_rows,vec![1]); assert_eq!(tab.frozen_rows,1); assert_eq!(tab.color.as_deref(),Some("#ff0000"));
}

#[test]
fn partial_merge_can_extend_parent_with_only_selected_cells() {
    use calc_engine::{diff::{plan_merge,MergeChoice},addr::Rect};
    let mut base=Workbook::new();let t=base.tabs[0].id;write(&mut base,t,"A1","1");
    let mut copy=base.clone();write(&mut copy,t,"F1","1");write(&mut copy,t,"F8","8");write(&mut copy,t,"G1","100");
    let choice=MergeChoice{area:Some((t,Rect::new(0,5,7,5))),..Default::default()};
    let plan=plan_merge(&base,&base,&copy,&choice).unwrap();assert_eq!(plan.cells_taken,2);
    let mut parent=base.clone();replay(&mut parent,plan.op.unwrap());
    assert_eq!(parent.tab(t).unwrap().value_at(0,5),Value::Number(1.0));assert_eq!(parent.tab(t).unwrap().value_at(7,5),Value::Number(8.0));
    assert_eq!(parent.tab(t).unwrap().value_at(0,6),Value::Empty);
}

#[test]
fn merged_new_tabs_remap_forward_formulas_names_code_inputs_and_charts() {
    use calc_engine::{diff::{plan_merge,MergeChoice},model::{CodeSource,DefinedName}};
    let base=Workbook::new(); let mut parent=base.clone(); let mut copy=base.clone();
    parent.apply(&Op::AddTab{tab:2,name:"Inputs".into(),index:None},&ACTOR).unwrap();
    write(&mut parent,2,"A1","999");
    copy.apply(&Op::AddTab{tab:2,name:"Totals".into(),index:None},&ACTOR).unwrap();
    copy.apply(&Op::AddTab{tab:3,name:"Inputs".into(),index:None},&ACTOR).unwrap();
    write(&mut copy,3,"A1","7");write(&mut copy,2,"A1","=Inputs!A1*2");
    write(&mut copy,1,"A1","=Totals!A1");
    copy.apply(&Op::TabProps{tab:2,props:TabProps{color:Some(Some("#abcdef".into())),code:Some(Some(CodeSource{language:"sql".into(),code:"select 1".into(),inputs:vec![3],run_by:"c:test".into(),run_at:0,keep_updated:false})),..Default::default()}},&ACTOR).unwrap();
    copy.apply(&Op::SetNames{names:vec![DefinedName{name:"Source".into(),text:"Inputs!$A$1".into(),tab:Some(3)}]},&ACTOR).unwrap();
    copy.apply(&Op::PutChart{chart:Chart{id:"new-chart".into(),tab:2,kind:"bar".into(),title:"Totals".into(),range:"A1".into(),anchor_row:0,anchor_col:2,width:200,height:150}},&ACTOR).unwrap();
    let plan=plan_merge(&base,&parent,&copy,&MergeChoice::default()).unwrap();replay(&mut parent,plan.op.unwrap());
    let totals=parent.tab_by_name("Totals").unwrap(); let input=parent.tabs.iter().find(|t| t.name.starts_with("Inputs") && t.id != 2).unwrap();
    assert_eq!(totals.value_at(0,0),Value::Number(14.0));assert_eq!(parent.tab(1).unwrap().value_at(0,0),Value::Number(14.0));
    assert_eq!(parent.tab(2).unwrap().value_at(0,0),Value::Number(999.0));
    assert_eq!(totals.color.as_deref(),Some("#abcdef"));assert_eq!(totals.code.as_ref().unwrap().inputs,vec![input.id]);
    assert_eq!(parent.charts[0].tab,totals.id); assert_eq!(parent.names[0].tab,Some(input.id));
    assert!(parent.names[0].text.contains(&input.name));
}

#[test]
fn partial_merge_formula_references_follow_source_cell_identities() {
    use calc_engine::{diff::{plan_merge,MergeChoice},addr::Rect};
    let mut base=Workbook::new();let t=base.tabs[0].id;
    write(&mut base,t,"B1","10");write(&mut base,t,"B2","20");
    let mut copy=base.clone();
    copy.apply(&Op::DeleteRows{tab:t,ids:vec![0]},&ACTOR).unwrap();
    write(&mut copy,t,"A1","=$B$1");write(&mut copy,t,"C1","=SUM($B$1:B1)");
    let choice=MergeChoice{area:Some((t,Rect::new(0,0,0,2))),..Default::default()};
    let plan=plan_merge(&base,&base,&copy,&choice).unwrap();
    let mut parent=base.clone();replay(&mut parent,plan.op.unwrap());
    assert_eq!(parent.tab(t).unwrap().value_at(1,0),Value::Number(20.0));
    assert_eq!(parent.tab(t).unwrap().value_at(1,2),Value::Number(20.0));
    assert_eq!(parent.formula_text(t,1,0).as_deref(),Some("$B$2"));
    assert_eq!(parent.formula_text(t,1,2).as_deref(),Some("SUM($B$2:B2)"));
    assert_eq!(parent.tab(t).unwrap().value_at(0,1),Value::Number(10.0));
}

#[test]
fn merge_names_and_chart_ranges_follow_parent_insertions() {
    use calc_engine::{diff::{plan_merge,MergeChoice},model::DefinedName};
    let mut base=Workbook::new();let t=base.tabs[0].id;
    write(&mut base,t,"B1","10");write(&mut base,t,"B2","20");
    let mut parent=base.clone();let inserted=parent.prepare_insert(t,true,0,1).unwrap();parent.apply(&inserted,&ACTOR).unwrap();
    let mut copy=base.clone();let name=copy.tabs[0].name.clone();
    copy.apply(&Op::SetNames{names:vec![DefinedName{name:"Revenue".into(),text:format!("'{name}'!$B$2"),tab:Some(t)}]},&ACTOR).unwrap();
    copy.apply(&Op::PutChart{chart:Chart{id:"range-chart".into(),tab:t,kind:"bar".into(),title:"Revenue".into(),range:"B1:B2".into(),anchor_row:0,anchor_col:2,width:200,height:150}},&ACTOR).unwrap();
    let plan=plan_merge(&base,&parent,&copy,&MergeChoice::default()).unwrap();replay(&mut parent,plan.op.unwrap());
    assert!(parent.names[0].text.ends_with("!$B$3"));assert_eq!(parent.charts[0].range,"B2:B3");
    let accepted=copy.clone();copy.names[0].text=format!("'{name}'!$B$1");
    let plan=plan_merge(&accepted,&parent,&copy,&MergeChoice::default()).unwrap();replay(&mut parent,plan.op.unwrap());
    assert!(parent.names[0].text.ends_with("!$B$2"));
}
