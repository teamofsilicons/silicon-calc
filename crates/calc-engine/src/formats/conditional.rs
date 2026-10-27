//! Native Excel rules plus Calc's stable, editable representation for a lossless round trip.
use std::io::{Cursor, Write};
use rust_xlsxwriter::{Worksheet, ConditionalFormatFormula, ConditionalFormat2ColorScale, ConditionalFormat3ColorScale};
use crate::{Workbook, Tab, CellPos, Style, conditional::{Kind, PositionRule}, ops::TabContent};
use super::{FormatError, xml::{reader,push_text}};
pub const PART:&str="xl/silicon-calc-format-rules.xml";
fn error(e:impl ToString)->FormatError{FormatError::write("Excel",e.to_string())}
pub fn write(wb:&Workbook, tab:&Tab, ws:&mut Worksheet)->Result<(),FormatError>{
    let quoted=|s:&str|format!("\"{}\"",s.replace('"',"\"\""));
    for rule in tab.format_rules.iter().filter(|r|!matches!(r.kind,Kind::Alternating{..})).chain(tab.format_rules.iter().filter(|r|matches!(r.kind,Kind::Alternating{..}))) {
        let Some(a)=tab.pos_rect(&rule.area) else{continue};
        if a.r2>=1_048_576||a.c2>=16_384{continue;}
        let add=|ws:&mut Worksheet, formula:String,style:Style,stop:bool|->Result<(),FormatError>{
            let cf=ConditionalFormatFormula::new().set_rule(formula.as_str()).set_format(super::xlsx_write::to_format(&style)).set_stop_if_true(stop);
            ws.add_conditional_format(a.r1,a.c1 as u16,a.r2,a.c2 as u16,&cf).map_err(error)?;Ok(())
        };
        let cell=CellPos::new(a.r1,a.c1).to_string();
        match &rule.kind {
            Kind::Scale{low,high,middle}=>{
                if let Some(mid)=middle{let cf=ConditionalFormat3ColorScale::new().set_minimum_color(low.as_str()).set_midpoint_color(mid.as_str()).set_maximum_color(high.as_str());ws.add_conditional_format(a.r1,a.c1 as u16,a.r2,a.c2 as u16,&cf).map_err(error)?;}
                else{let cf=ConditionalFormat2ColorScale::new().set_minimum_color(low.as_str()).set_maximum_color(high.as_str());ws.add_conditional_format(a.r1,a.c1 as u16,a.r2,a.c2 as u16,&cf).map_err(error)?;}
            },
            Kind::Alternating{first,second,header,footer}=>{
                let fill=|s:&String|Style{fill:Some(s.clone()),..Default::default()};
                if let Some(h)=header{add(ws,format!("ROW()={}",a.r1+1),fill(h),true)?;}
                if let Some(f)=footer{add(ws,format!("ROW()={}",a.r2+1),fill(f),true)?;}
                add(ws,format!("MOD(ROW()-{},2)=0",a.r1+1+u32::from(header.is_some())),fill(first),true)?;
                add(ws,"TRUE".into(),fill(second),true)?;
            },
            Kind::Condition{condition,value,second,paint}=>{
                let q=quoted(value);let num=|v:&str|v.parse::<f64>().ok().or_else(||crate::dates::parse_date_time(v).map(|(n,_)|n)).map(|v|v.to_string()).unwrap_or_else(||"0".into());
                let x=num(value);let y=num(second);
                let formula=match condition.as_str(){
                    "empty"=>format!("LEN({cell})=0"),"not_empty"=>format!("LEN({cell})>0"),"contains"=>format!("ISNUMBER(SEARCH({q},{cell}))"),"not_contains"=>format!("NOT(ISNUMBER(SEARCH({q},{cell})))"),"starts"=>format!("LEFT({cell},LEN({q}))={q}"),"ends"=>format!("RIGHT({cell},LEN({q}))={q}"),"text_is"=>format!("{cell}={q}"),"text_not"=>format!("{cell}<>{q}"),
                    "greater"=>format!("AND(ISNUMBER({cell}),{cell}>{x})"),"greater_equal"=>format!("AND(ISNUMBER({cell}),{cell}>={x})"),"less"=>format!("AND(ISNUMBER({cell}),{cell}<{x})"),"less_equal"=>format!("AND(ISNUMBER({cell}),{cell}<={x})"),"equal"=>format!("AND(ISNUMBER({cell}),{cell}={x})"),"not_equal"=>format!("AND(ISNUMBER({cell}),{cell}<>{x})"),"between"=>format!("AND(ISNUMBER({cell}),{cell}>={x},{cell}<={y})"),"not_between"=>format!("AND(ISNUMBER({cell}),OR({cell}<{x},{cell}>{y}))"),
                    "date_is"=>format!("AND(ISNUMBER({cell}),INT({cell})=INT({x}))"),"date_before"=>format!("AND(ISNUMBER({cell}),INT({cell})<INT({x}))"),"date_after"=>format!("AND(ISNUMBER({cell}),INT({cell})>INT({x}))"),"formula"=>value.clone(),_=>"FALSE".into(),
                };
                add(ws,formula,Style{fill:paint.fill.clone(),color:paint.color.clone(),bold:paint.bold,italic:paint.italic,underline:paint.underline,strike:paint.strike,..Default::default()},true)?;
            }
        }
    }
    let _=wb;Ok(())
}
pub fn append(bytes:Vec<u8>,rules:&[(String,Vec<PositionRule>)])->Result<Vec<u8>,FormatError>{
    if rules.is_empty(){return Ok(bytes)}
    let json=serde_json::to_string(rules).map_err(error)?;
    let xml=format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><calc-format-rules>{}</calc-format-rules>",json.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;"));
    let mut zip=zip::ZipWriter::new_append(Cursor::new(bytes)).map_err(error)?;
    zip.start_file(PART,zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).map_err(error)?;zip.write_all(xml.as_bytes()).map_err(error)?;Ok(zip.finish().map_err(error)?.into_inner())
}
pub fn restore(xml:&[u8],tabs:&mut[(String,TabContent)]){
    if xml.len()>4*1024*1024{return;}
    let mut r=reader(xml);let mut buf=Vec::new();let mut json=String::new();
    loop{buf.clear();match r.read_event_into(&mut buf){Ok(quick_xml::events::Event::Eof)|Err(_)=>break,Ok(event)=>push_text(&mut json,&event)}}
    let Ok(rules)=serde_json::from_str::<Vec<(String,Vec<PositionRule>)>>(&json) else{return;};
    for(name,rules)in rules{
        let Some((_,tab))=tabs.iter_mut().find(|(n,_)|*n==name)else{continue};
        tab.format_rules=rules.into_iter().take(100).filter(|r|r.id.len()<=100&&r.kind.validate().is_ok()&&r.range.r2<1_048_576&&r.range.c2<16_384&&u64::from(r.range.rows())*u64::from(r.range.cols())<=200_000).collect();
        for r in &tab.format_rules{tab.rows=tab.rows.max(r.range.r2+1);tab.cols=tab.cols.max(r.range.c2+1);}
        tab.columns.resize_with(tab.cols as usize,Default::default);
    }
}
