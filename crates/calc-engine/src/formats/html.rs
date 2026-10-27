//! A self-contained, script-free view of the calculated workbook.
use super::{ExportOptions, FormatError, export_tabs};
use crate::{CellPos, Workbook, numfmt::format_number, value::Value};

pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn export(wb: &Workbook, opts: &ExportOptions) -> Result<Vec<u8>, FormatError> {
    let tabs = export_tabs(wb, opts, "HTML")?;
    let mut out = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>Calc workbook</title><style>body{font:14px system-ui,sans-serif;color:#202124;margin:32px;background:#fff}section{margin:0 0 40px;overflow:auto}table{border-collapse:collapse}caption{text-align:left;font-size:22px;font-weight:600;padding:0 0 16px}td{border:1px solid #dadce0;padding:6px 10px;white-space:pre-wrap;min-width:60px}.number{text-align:right}</style></head><body>",
    );
    for id in tabs {
        let tab = wb.tab(id).expect("export tab");
        out.push_str(&format!(
            "<section><table><caption>{}</caption><tbody>",
            escape(&tab.name)
        ));
        if let Some(used) = tab.used_rect() {
            for r in 0..=used.r2 {
                out.push_str("<tr>");
                for c in 0..=used.c2 {
                    let cell = tab.cell_at(CellPos::new(r, c));
                    let style = wb.style_of(cell.style);
                    let text = match &cell.value {
                        Value::Number(n) => {
                            format_number(*n, style.num_fmt.as_deref().unwrap_or("General")).0
                        }
                        Value::Text(s) => s.to_string(),
                        Value::Bool(v) => if *v { "TRUE" } else { "FALSE" }.into(),
                        Value::Error(e) => e.as_str().into(),
                        Value::Empty => String::new(),
                    };
                    let mut css = String::new();
                    if style.bold {
                        css.push_str("font-weight:bold;");
                    }
                    if style.italic {
                        css.push_str("font-style:italic;");
                    }
                    if let Some(color) = style.color.as_deref().and_then(super::colors::parse_hex) {
                        css.push_str(&format!("color:#{color:06x};"));
                    }
                    if let Some(fill) = style.fill.as_deref().and_then(super::colors::parse_hex) {
                        css.push_str(&format!("background:#{fill:06x};"));
                    }
                    out.push_str(&format!(
                        "<td class=\"{}\" style=\"{css}\">{}</td>",
                        if matches!(cell.value, Value::Number(_)) {
                            "number"
                        } else {
                            "text"
                        },
                        escape(&text)
                    ));
                }
                out.push_str("</tr>");
            }
        }
        out.push_str("</tbody></table></section>");
    }
    out.push_str("</body></html>");
    Ok(out.into_bytes())
}
