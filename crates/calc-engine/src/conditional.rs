//! Dynamic presentation rules. Ranges use stable row and column ids in the workbook.
use serde::{Deserialize, Serialize};
use crate::{Workbook, Tab, Rect, CellPos, Style, Value, model::IdRect};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rule { pub id: String, pub area: IdRect, pub kind: Kind }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PositionRule { pub id: String, pub range: Rect, pub kind: Kind }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag="kind",rename_all="snake_case")]
pub enum Kind {
    Condition { condition: String, #[serde(default)] value: String, #[serde(default)] second: String, paint: Paint },
    Scale { low: String, high: String, #[serde(default)] middle: Option<String> },
    Alternating { first: String, second: String, #[serde(default)] header: Option<String>, #[serde(default)] footer: Option<String> },
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Paint {
    pub fill: Option<String>, pub color: Option<String>,
    #[serde(default)] pub bold: bool, #[serde(default)] pub italic: bool,
    #[serde(default)] pub underline: bool, #[serde(default)] pub strike: bool,
}
impl Kind {
    pub fn validate(&self) -> Result<(), String> {
        let colors: Vec<&str> = match self {
            Self::Condition {condition,value,second,paint} => {
                if !["empty","not_empty","contains","not_contains","starts","ends","text_is","text_not","greater","greater_equal","less","less_equal","equal","not_equal","between","not_between","date_is","date_before","date_after","formula"].contains(&condition.as_str()) || value.len()>4096 || second.len()>4096 { return Err("Choose a supported condition with values up to 4 KB.".into()); }
                [&paint.fill,&paint.color].into_iter().flatten().map(String::as_str).collect()
            }
            Self::Scale {low,high,middle} => [Some(low),Some(high),middle.as_ref()].into_iter().flatten().map(String::as_str).collect(),
            Self::Alternating {first,second,header,footer} => [Some(first),Some(second),header.as_ref(),footer.as_ref()].into_iter().flatten().map(String::as_str).collect(),
        };
        if colors.iter().any(|c| c.len()!=7 || !c.starts_with('#') || !c[1..].bytes().all(|b|b.is_ascii_hexdigit())) { return Err("Use colours written as #RRGGBB.".into()); }
        Ok(())
    }
}
fn blend(a: &str,b: &str,t:f64)->String {
    let channel=|s:&str,i|u8::from_str_radix(&s[i..i+2],16).unwrap_or(0) as f64;
    let c=|i|(channel(a,i)+(channel(b,i)-channel(a,i))*t.clamp(0.,1.)).round() as u8;
    format!("#{:02X}{:02X}{:02X}",c(1),c(3),c(5))
}
fn number(text:&str)->Option<f64>{ text.parse().ok().or_else(||crate::dates::parse_date_time(text).map(|(n,_)|n)) }
impl Workbook {
    pub fn has_format_rule(&self, tab:&Tab,row:u32,col:u32)->bool {
        tab.format_rules.iter().any(|r|tab.pos_rect(&r.area).is_some_and(|a|row>=a.r1&&row<=a.r2&&col>=a.c1&&col<=a.c2))
    }
    pub fn displayed_style(&self,tab:&Tab,row:u32,col:u32)->Style {
        let cell=tab.cell_at(CellPos::new(row,col));let mut style=self.style_of(cell.style).clone();
        // Alternating colours form the base; the first matching conditional rule takes precedence.
        for rule in tab.format_rules.iter().filter(|r|matches!(r.kind,Kind::Alternating{..})) {
            let Some(a)=tab.pos_rect(&rule.area) else {continue};
            if row<a.r1||row>a.r2||col<a.c1||col>a.c2 {continue;}
            if let Kind::Alternating{first,second,header,footer}=&rule.kind {
                style.fill=Some(if row==a.r1&&header.is_some(){ header.as_ref().unwrap() } else if row==a.r2&&footer.is_some(){ footer.as_ref().unwrap() } else if (row-a.r1-u32::from(header.is_some())).is_multiple_of(2){first}else{second}.clone());
            }
        }
        for rule in &tab.format_rules {
            let Some(a)=tab.pos_rect(&rule.area) else {continue};
            if row<a.r1||row>a.r2||col<a.c1||col>a.c2 {continue;}
            match &rule.kind {
                Kind::Alternating{..}=>{},
                Kind::Scale{low,high,middle}=>{
                    let Value::Number(n)=&cell.value else {continue};
                    let mut cache=self.format_stats.lock().unwrap();
                    let (min,max)=*cache.entry((tab.id,rule.id.clone())).or_insert_with(||{
                        let mut min=f64::INFINITY;let mut max=f64::NEG_INFINITY;
                        for r in a.r1..=a.r2 { for c in a.c1..=a.c2 { if let Value::Number(v)=tab.value_at(r,c){min=min.min(v);max=max.max(v);} } }
                        (min,max)
                    });
                    let t=if max>min{(n-min)/(max-min)}else{0.5};
                    style.fill=Some(match middle {Some(mid) if t<=0.5=>blend(low,mid,t*2.),Some(mid)=>blend(mid,high,(t-0.5)*2.),None=>blend(low,high,t)});break;
                },
                Kind::Condition{condition,value,second,paint}=>{
                    let text=cell.value.display().to_lowercase();let wanted=value.to_lowercase();
                    let n=if let Value::Number(n)=&cell.value{Some(*n)}else{None};let x=number(value);let y=number(second);
                    let matches=match condition.as_str(){
                        "empty"=>cell.value.is_empty()||text.is_empty(),"not_empty"=>!cell.value.is_empty()&&!text.is_empty(),
                        "contains"=>text.contains(&wanted),"not_contains"=>!text.contains(&wanted),"starts"=>text.starts_with(&wanted),"ends"=>text.ends_with(&wanted),"text_is"=>text==wanted,"text_not"=>text!=wanted,
                        "greater"=>n.zip(x).is_some_and(|(n,x)|n>x),"greater_equal"=>n.zip(x).is_some_and(|(n,x)|n>=x),"less"=>n.zip(x).is_some_and(|(n,x)|n<x),"less_equal"=>n.zip(x).is_some_and(|(n,x)|n<=x),"equal"=>n.zip(x).is_some_and(|(n,x)|n==x),"not_equal"=>n.zip(x).is_some_and(|(n,x)|n!=x),
                        "between"=>n.zip(x.zip(y)).is_some_and(|(n,(x,y))|n>=x&&n<=y),"not_between"=>n.zip(x.zip(y)).is_some_and(|(n,(x,y))|n<x||n>y),
                        "date_is"=>n.zip(x).is_some_and(|(n,x)|n.floor()==x.floor()),"date_before"=>n.zip(x).is_some_and(|(n,x)|n.floor()<x.floor()),"date_after"=>n.zip(x).is_some_and(|(n,x)|n.floor()>x.floor()),
                        "formula"=>crate::workbook::evaluate_relative(self,tab.id,row,col,value,a.r1,a.c1).is_ok_and(|v|v.coerce_bool().unwrap_or(false)),
                        _=>false,
                    };
                    if matches {if let Some(f)=&paint.fill{style.fill=Some(f.clone())}if let Some(c)=&paint.color{style.color=Some(c.clone())}style.bold=paint.bold;style.italic=paint.italic;style.underline=paint.underline;style.strike=paint.strike;break;}
                }
            }
        }
        style
    }
}
