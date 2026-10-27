use calc_engine::{ops::{Actor, CellWrite}, value::Value, workbook::Workbook};
#[test]
fn named_and_immediate_lambdas_evaluate_and_capture_scope() {
    for (formula, value) in [("=LET(f,LAMBDA(a,b,a*b),f(2,3))",6.0), ("=LAMBDA(x,x+1)(4)",5.0), ("=LET(offset,7,f,LAMBDA(x,x+offset),f(2))",9.0), ("=LAMBDA(x,y,IF(ISOMITTED(y),x,x+y))(5)",5.0)] {
        let mut w=Workbook::new(); let t=w.tabs[0].id;
        let op=w.prepare_writes(t,vec![(0,0,CellWrite {input:Some(formula.into()),clear:true,..Default::default()})]).unwrap();
        w.apply(&op,&Actor {account:"c:test",at:0}).unwrap();
        assert_eq!(w.tab(t).unwrap().value_at(0,0),Value::Number(value),"{formula}");
    }
}
