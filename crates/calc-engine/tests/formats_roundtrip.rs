use calc_engine::{formats::{self, Format, ImportOptions, ExportOptions}, model::{Chart,DefinedName}, ops::{Actor, CellWrite, Op, TabProps}, style::StylePatch, value::Value, workbook::Workbook};
const A:Actor<'static>=Actor{account:"c:fixture",at:1_800_000_000};
fn fixture()->Workbook {
    let mut w=Workbook::new(); let t=w.tabs[0].id;
    w.apply(&Op::RenameTab{tab:t,name:"Sales & costs".into()},&A).unwrap();
    let mut writes=vec![];
    for (r,c,input) in [(0,0,"Category"),(0,1,"Amount"),(1,0,"Coffee ☕"),(1,1,"12.50"),(2,0,"Tea"),(2,1,"7.50"),(3,1,"=SUM(B2:B3)")] {
        writes.push((r,c,CellWrite{input:Some(input.into()),clear:true,style_patch:Some(StylePatch{bold:Some(r==0),..Default::default()}),..Default::default()}));
    }
    let op=w.prepare_writes(t,writes).unwrap();w.apply(&op,&A).unwrap();
    w.apply(&Op::TabProps{tab:t,props:TabProps{frozen_rows:Some(1),color:Some(Some("#2266AA".into())),..Default::default()}},&A).unwrap();
    w.apply(&Op::ColWidths{tab:t,widths:vec![(0,Some(180.0))]},&A).unwrap();
    w.apply(&Op::SetNames{names:vec![DefinedName{name:"Amounts".into(),text:"'Sales & costs'!$B$2:$B$3".into(),tab:None}]},&A).unwrap();
    w.apply(&Op::PutChart{chart:Chart{id:"revenue".into(),tab:t,kind:"column".into(),title:"Revenue".into(),range:"A1:B3".into(),anchor_row:0,anchor_col:3,width:400,height:240}},&A).unwrap();
    w
}
fn imported(format:Format,bytes:&[u8])->(Workbook,formats::Imported) {
    let i=formats::import(format,bytes,&ImportOptions::default()).unwrap();
    let mut w=Workbook::empty();let p=i.plan(&w,None);w.apply(&p.op,&A).unwrap();(w,i)
}
#[test]
fn excel_roundtrip_preserves_formulas_unicode_styles_names_charts_and_layout(){
    let w=fixture();let data=formats::export(&w,Format::Xlsx,&ExportOptions::default()).unwrap();
    let (back,i)=imported(Format::Xlsx,&data);assert!(i.warnings.is_empty(),"{:?}",i.warnings);
    let t=&back.tabs[0];assert_eq!(t.name,"Sales & costs");assert_eq!(t.value_at(1,0),Value::from("Coffee ☕"));
    assert_eq!(t.value_at(3,1),Value::Number(20.0));assert_eq!(back.formula_text(t.id,3,1).as_deref(),Some("SUM(B2:B3)"));
    assert!(back.style_of(t.cell_at(calc_engine::addr::CellPos::new(0,0)).style).bold);
    assert_eq!(t.frozen_rows,1);assert_eq!(t.color.as_deref(),Some("#2266AA"));
    assert_eq!(back.names[0].name,"Amounts");assert_eq!(back.charts[0].title,"Revenue");
    assert!((t.col_widths[&0]-180.0).abs()<2.0);
}
#[test]
fn json_formulas_and_delimited_data_roundtrip(){
    let w=fixture();for format in [Format::Csv,Format::Tsv,Format::Json] {
        let data=formats::export(&w,format,&ExportOptions::default()).unwrap();let (back,_)=imported(format,&data);let t=&back.tabs[0];
        assert_eq!(t.value_at(1,0),Value::from("Coffee ☕"));assert_eq!(t.value_at(3,1),Value::Number(20.0));
        if format==Format::Json {assert_eq!(back.formula_text(t.id,3,1).as_deref(),Some("SUM(B2:B3)"));}
    }
}
