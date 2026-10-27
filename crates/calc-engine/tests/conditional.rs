use calc_engine::{Workbook, Op, Value, Rect, conditional::{Kind,Paint,Rule}, ops::{Actor,CellWrite,TabProps},formats::{self,Format,ExportOptions,ImportOptions}};
const ACTOR:Actor<'_>=Actor{account:"owner",at:1_800_000_000};
fn set(w:&mut Workbook,r:u32,c:u32,text:&str){let op=w.prepare_writes(1,vec![(r,c,CellWrite{input:Some(text.into()),..Default::default()})]).unwrap();w.apply(&op,&ACTOR).unwrap();}
fn add(w:&mut Workbook,id:&str,range:Rect,kind:Kind){let area=w.tab_mut(1).unwrap().id_rect(&range);let mut rules=w.tab(1).unwrap().format_rules.clone();rules.push(Rule{id:id.into(),area,kind});w.apply(&Op::TabProps{tab:1,props:TabProps{format_rules:Some(rules),..Default::default()}},&ACTOR).unwrap();}
fn fill(w:&Workbook,r:u32,c:u32)->Option<String>{w.displayed_style(w.tab(1).unwrap(),r,c).fill}
#[test]
fn conditions_react_to_values_and_relative_formulas(){
 let mut w=Workbook::new();set(&mut w,0,0,"1");set(&mut w,1,0,"20");
 add(&mut w,"numeric",Rect::new(0,0,1,0),Kind::Condition{condition:"greater".into(),value:"10".into(),second:String::new(),paint:Paint{fill:Some("#E8EFE9".into()),..Default::default()}});
 assert_eq!(fill(&w,0,0),None);assert_eq!(fill(&w,1,0).as_deref(),Some("#E8EFE9"));set(&mut w,0,0,"30");assert_eq!(fill(&w,0,0).as_deref(),Some("#E8EFE9"));
 add(&mut w,"formula",Rect::new(0,1,1,1),Kind::Condition{condition:"formula".into(),value:"=$A1>25".into(),second:String::new(),paint:Paint{fill:Some("#FFEEDD".into()),..Default::default()}});
 assert_eq!(fill(&w,0,1).as_deref(),Some("#FFEEDD"));assert_eq!(fill(&w,1,1),None);
}
#[test]
fn alternating_colours_expand_with_rows_and_scale_cache_invalidates(){
 let mut w=Workbook::new();
 add(&mut w,"band",Rect::new(0,0,3,0),Kind::Alternating{first:"#FFFFFF".into(),second:"#EEEEEE".into(),header:Some("#CCCCCC".into()),footer:None});
 assert_eq!(fill(&w,0,0).as_deref(),Some("#CCCCCC"));assert_eq!(fill(&w,2,0).as_deref(),Some("#EEEEEE"));
 let t=w.tab(1).unwrap();let op=Op::InsertRows{tab:1,before:t.rows.id_at(2),ids:vec![t.rows.next_id()]};w.apply(&op,&ACTOR).unwrap();assert_eq!(fill(&w,4,0).as_deref(),Some("#EEEEEE"));
 set(&mut w,0,1,"0");set(&mut w,1,1,"50");set(&mut w,2,1,"100");
 add(&mut w,"scale",Rect::new(0,1,2,1),Kind::Scale{low:"#000000".into(),high:"#FFFFFF".into(),middle:None});
 assert_eq!(fill(&w,1,1).as_deref(),Some("#808080"));set(&mut w,2,1,"200");assert_eq!(fill(&w,1,1).as_deref(),Some("#404040"));
 let bytes=formats::export(&w,Format::Xlsx,&ExportOptions::default()).unwrap();let imported=formats::import(Format::Xlsx,&bytes,&ImportOptions::default()).unwrap();let mut back=Workbook::empty();back.apply(&imported.plan(&back,None).op,&ACTOR).unwrap();
 assert_eq!(fill(&back,4,0),fill(&w,4,0));assert_eq!(fill(&back,1,1),fill(&w,1,1));assert_eq!(back.tab(1).unwrap().value_at(2,1),Value::Number(200.0));
}
