//! Browser-evaluated formulas. The engine only records calls and accepts version-checked values.
use std::sync::Arc;
use serde::{Deserialize, Serialize};
use crate::{Value, ErrorKind, Workbook, Rect, formula::{ast::Expr, eval::{Array, Ctx, Res}}};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub code: String,
    /// Account library this definition was imported from; local definitions take precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().enumerate().all(|(i, c)| c.is_ascii_alphabetic() || c == b'_' || (i > 0 && c.is_ascii_digit()))
}
impl Function {
    pub fn validate(&self) -> Result<(), String> {
        if !identifier(&self.name) || self.name.eq_ignore_ascii_case("JS") || crate::formula::functions::is_known(&self.name) || crate::CellPos::parse(&self.name).is_some() {
            return Err("Choose a function name such as DOUBLE_VALUE, without replacing a built-in or a cell address.".into());
        }
        const RESERVED: &[&str] = &["await", "yield", "return", "function", "class", "const", "let", "var", "new", "this", "super", "import", "export", "default", "delete", "typeof", "void", "if", "else", "for", "while", "do", "try", "catch", "finally", "switch", "case", "break", "continue", "throw", "in", "instanceof", "true", "false", "null", "undefined", "calc", "args", "console", "arguments", "eval"];
        if self.parameters.len() > 32 || self.parameters.iter().enumerate().any(|(i,p)| !identifier(p) || RESERVED.contains(&p.as_str()) || self.parameters[..i].contains(p)) {
            return Err("Use up to 32 distinct JavaScript parameter names; calc, args and reserved words cannot be parameters.".into());
        }
        if self.code.trim().is_empty() || self.code.len() > 65536 { return Err("Write a JavaScript function body of up to 64 KB.".into()); }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub name: String,
    pub tab: u32,
    pub row: u32,
    pub col: u32,
    pub args: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Answer>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub rows: Vec<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Reads observed by the browser bridge. Absent on results from older clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<Dependencies>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Dependencies {
    #[serde(default)]
    pub reads: Vec<Read>,
    #[serde(default)]
    pub tabs: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Read {
    pub tab: u32,
    /// None means the whole used tab, including cells added later.
    pub range: Option<String>,
}
impl Answer {
    pub fn validate(&self) -> Result<(), String> {
        if self.dependencies.as_ref().is_some_and(|d| d.reads.len() > 1024 || d.reads.iter().any(|r| r.range.as_ref().is_some_and(|s| s.len() > 64 || Rect::parse(s, crate::addr::MAX_ROWS, crate::addr::MAX_COLS).is_none()))) {
            return Err("Read at most 1,024 valid cell ranges per JavaScript formula.".into());
        }
        let cols = self.rows.first().map_or(0, Vec::len);
        if self.rows.is_empty() || cols == 0 || self.rows.len().saturating_mul(cols) > 200_000 || self.rows.iter().any(|r| r.len() != cols || r.iter().any(|v| matches!(v, Value::Number(n) if !n.is_finite()) || matches!(v, Value::Text(s) if s.len() > 32768))) || self.error.as_ref().is_some_and(|e| e.len() > 2048) {
            return Err("Return a scalar or a rectangular array of at most 200,000 cells; each text cell may hold 32 KB.".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub epoch: u64,
    pub next: u64,
    #[serde(default)]
    pub bytes: usize,
    pub calls: Vec<Call>,
    #[serde(skip)]
    pub used: std::collections::HashSet<u64>,
}

impl State {
    /// Only invalidate results whose observed reads intersect a changed value.
    pub(crate) fn invalidate_reads(&mut self, changes: &[(u32, Rect)], metadata: bool, legacy: bool) -> bool {
        let mut invalidated = false;
        for call in &mut self.calls {
            let affected = call.result.as_ref().is_some_and(|answer| match &answer.dependencies {
                None => legacy,
                Some(d) => (d.tabs && metadata) || d.reads.iter().any(|read| changes.iter().any(|(tab, area)| *tab == read.tab && read.range.as_ref().is_none_or(|s| Rect::parse(s, crate::addr::MAX_ROWS, crate::addr::MAX_COLS).is_some_and(|r| r.intersects(area))))),
            });
            if affected { call.result = None; invalidated = true; }
        }
        invalidated
    }

    pub(crate) fn prune(&mut self) {
        self.calls.retain(|call| self.used.contains(&call.id));
        self.bytes = self.calls.iter().map(|call| serde_json::to_vec(&(&call.args, &call.result)).map_or(usize::MAX, |s| s.len())).fold(0usize, usize::saturating_add);
    }
}

fn json(v: &Value) -> serde_json::Value {
    match v {
        Value::Empty => serde_json::Value::Null,
        Value::Number(n) => serde_json::json!(n),
        Value::Text(s) => serde_json::json!(s),
        Value::Bool(b) => serde_json::json!(b),
        Value::Error(e) => serde_json::json!({"error": e.as_str()}),
    }
}

pub fn call(ctx: &mut Ctx<'_>, name: &str, args: &[Expr]) -> Res {
    if args.len() > 32 { return Res::err(ErrorKind::Value); }
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        let r = ctx.eval(arg);
        if let Some(error) = r.as_error() { return Res::err(error); }
        match r {
            Res::V(v) => values.push(json(&v)),
            Res::L(_) => return Res::err(ErrorKind::Value),
            Res::R(ref area) if area.is_cell() => {
                let value = ctx.to_scalar(r);
                if let Value::Error(e) = value { return Res::err(e); }
                values.push(json(&value));
            },
            other => {
                // Bound open ranges to their used extent before materializing them.
                if let Some(areas) = other.areas() {
                    let count: u64 = areas.iter().map(|a| { let (rows, cols) = ctx.array_dims(a); u64::from(rows) * u64::from(cols) }).sum();
                    if count > 200_000 { return Res::err(ErrorKind::Num); }
                }
                let array = ctx.to_array(&other);
                if u64::from(array.rows) * u64::from(array.cols) > 200_000 { return Res::err(ErrorKind::Num); }
                let mut rows = Vec::new();
                for r in 0..array.rows {
                    let mut row = Vec::new();
                    for c in 0..array.cols {
                        let value = array.get(r,c);
                        if let Value::Error(e) = value { return Res::err(*e); }
                        row.push(json(value));
                    }
                    rows.push(row);
                }
                values.push(serde_json::json!(rows));
            }
        }
    }
    if !ctx.missing.is_empty() { return Res::V(Value::Empty); }
    ctx.grid.javascript(name, ctx.tab, ctx.row, ctx.col, values)
}

impl Workbook {
    pub fn javascript_names(&self) -> Vec<String> {
        let mut names = std::collections::BTreeSet::new();
        for group in self.groups.list.iter().flatten() {
            group.expr.walk(&mut |e| if let Expr::Call(name, _) = e {
                if !crate::formula::functions::is_known(name) && !name.eq_ignore_ascii_case("JS") { names.insert(name.to_ascii_uppercase()); }
            });
        }
        names.into_iter().collect()
    }
    pub(crate) fn javascript_call(&self, name: &str, tab: u32, row: u32, col: u32, args: Vec<serde_json::Value>) -> Res {
        let mut state = self.javascript.lock().unwrap();
        let name = name.to_ascii_uppercase();
        if let Some(index) = state.calls.iter().position(|c| c.name == name && c.tab == tab && c.row == row && c.col == col && c.args == args) {
            let id = state.calls[index].id;
            state.used.insert(id);
            let call = &state.calls[index];
            return match &call.result {
                None => Res::err(ErrorKind::Busy),
                Some(answer) if answer.rows.len() == 1 && answer.rows[0].len() == 1 => Res::V(answer.rows[0][0].clone()),
                Some(answer) => Res::A(Arc::new(Array::new(answer.rows.len() as u32, answer.rows[0].len() as u32, answer.rows.iter().flatten().cloned().collect()))),
            };
        }
        let bytes=serde_json::to_vec(&args).map_or(usize::MAX, |s|s.len());
        if state.calls.len() >= 20_000 || bytes > 4 * 1024 * 1024 || state.bytes.saturating_add(bytes)>16*1024*1024 { return Res::err(ErrorKind::Num); }
        state.bytes += bytes;
        state.next += 1;
        let id = state.next;
        state.used.insert(id);
        state.calls.push(Call { id, name, tab, row, col, args, result: None });
        Res::err(ErrorKind::Busy)
    }

    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().filter(|f| f.name.eq_ignore_ascii_case(name)).min_by_key(|f| f.account.is_some())
    }
}
