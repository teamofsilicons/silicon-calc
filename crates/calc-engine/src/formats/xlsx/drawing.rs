//! A worksheet's drawing: where each chart sits and how big it is, and each chart part's kind, title and the
//! cells its series read. Pictures and shapes are counted (a sheet can't hold them).

use ahash::HashMap;
use quick_xml::events::Event;

use crate::{
    addr::{MAX_COLS, MAX_ROWS, Rect, quote_tab, split_tab},
    formats::{
        Warnings,
        xml::{attr, attr_f64, local, name_of, read_text, reader},
    },
    model::Chart,
    ops::TabContent,
};

use super::Package;

/// English Metric Units per pixel at 96 dpi.
const EMU_PER_PX: f64 = 9525.0;
const DEFAULT_COL_PX: f64 = 64.0;
const DEFAULT_ROW_PX: f64 = 20.0;

#[derive(Default, Clone, Copy)]
struct Marker {
    col: u32,
    col_off: f64,
    row: u32,
    row_off: f64,
}

#[derive(Default)]
struct Anchor {
    from: Option<Marker>,
    to: Option<Marker>,
    ext: Option<(f64, f64)>,
    pos: Option<(f64, f64)>,
    chart: Option<String>,
    picture: bool,
    shape: bool,
}

pub(super) fn read(
    pkg: &mut Package<'_>,
    path: &str,
    tab: usize,
    content: &TabContent,
    tab_names: &[String],
    warnings: &mut Warnings,
) -> Vec<Chart> {
    let Some(xml) = pkg.read(path) else { return Vec::new() };
    let rels = pkg.rels(path);
    let anchors = read_anchors(&xml);
    let widths: HashMap<u32, f32> = content.col_widths.iter().copied().collect();
    let heights: HashMap<u32, f32> = content.row_heights.iter().copied().collect();
    let col_px = |c: u32| widths.get(&c).map_or(DEFAULT_COL_PX, |&w| w as f64);
    let row_px = |r: u32| heights.get(&r).map_or(DEFAULT_ROW_PX, |&h| h as f64);
    let mut charts = Vec::new();
    for a in anchors {
        if a.picture {
            warnings.count("picture", String::new);
            continue;
        }
        let Some(rid) = a.chart else {
            if a.shape {
                warnings.count("shape", String::new);
            }
            continue;
        };
        let Some(rel) = rels.iter().find(|r| r.id == rid && !r.external) else { continue };
        let Some(chart_xml) = pkg.read(&rel.target) else { continue };
        let Some(parsed) = parse_chart(&chart_xml) else {
            warnings.count("chart_data", String::new);
            continue;
        };
        let Some(range) = chart_range(&parsed.refs, &tab_names[tab]) else {
            warnings.count("chart_data", String::new);
            continue;
        };
        if parsed.approximate {
            warnings.count("chart_kind", String::new);
        }
        // Where it sits and how big it is.
        let (anchor_row, anchor_col, width, height) = match (a.from, a.to, a.ext, a.pos) {
            (Some(f), Some(t), _, _) => {
                let mut w = -f.col_off / EMU_PER_PX + t.col_off / EMU_PER_PX;
                for c in f.col..t.col.min(f.col + 500) {
                    w += col_px(c);
                }
                let mut h = -f.row_off / EMU_PER_PX + t.row_off / EMU_PER_PX;
                for r in f.row..t.row.min(f.row + 2000) {
                    h += row_px(r);
                }
                (f.row, f.col, w, h)
            }
            (Some(f), None, Some((cx, cy)), _) => (f.row, f.col, cx / EMU_PER_PX, cy / EMU_PER_PX),
            (None, _, Some((cx, cy)), Some((x, y))) => {
                // Absolute: find the cell under the top-left corner.
                let (mut col, mut acc) = (0u32, 0.0);
                while acc + col_px(col) <= x / EMU_PER_PX && col < 16_383 {
                    acc += col_px(col);
                    col += 1;
                }
                let (mut row, mut acc) = (0u32, 0.0);
                while acc + row_px(row) <= y / EMU_PER_PX && row < 1_048_575 {
                    acc += row_px(row);
                    row += 1;
                }
                (row, col, cx / EMU_PER_PX, cy / EMU_PER_PX)
            }
            _ => (0, 0, 480.0, 288.0),
        };
        charts.push(Chart {
            id: String::new(),
            tab: tab as u32,
            kind: parsed.kind.to_owned(),
            title: parsed.title,
            range,
            anchor_row,
            anchor_col,
            width: width.round().clamp(50.0, 4000.0) as u32,
            height: height.round().clamp(50.0, 4000.0) as u32,
        });
    }
    charts
}

fn read_anchors(xml: &[u8]) -> Vec<Anchor> {
    let mut out = Vec::new();
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut b2 = Vec::new();
    let mut current: Option<Anchor> = None;
    let mut marker: Option<(bool, Marker)> = None;
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                match name_of(e) {
                    b"twoCellAnchor" | b"oneCellAnchor" | b"absoluteAnchor" => {
                        current = Some(Anchor::default());
                    }
                    b"from" | b"to" if current.is_some() && !empty => {
                        marker = Some((name_of(e) == b"from", Marker::default()));
                    }
                    n @ (b"col" | b"colOff" | b"row" | b"rowOff") if marker.is_some() && !empty => {
                        let n = n.to_vec();
                        let t = read_text(&mut r, &mut b2, &[]);
                        let v: f64 = t.trim().parse().unwrap_or(0.0);
                        if let Some((_, m)) = marker.as_mut() {
                            match n.as_slice() {
                                b"col" => m.col = v.max(0.0) as u32,
                                b"colOff" => m.col_off = v,
                                b"row" => m.row = v.max(0.0) as u32,
                                _ => m.row_off = v,
                            }
                        }
                    }
                    b"ext" => {
                        if let (Some(a), Some(cx), Some(cy)) = (current.as_mut(), attr_f64(e, b"cx"), attr_f64(e, b"cy"))
                            && a.ext.is_none()
                        {
                            a.ext = Some((cx, cy));
                        }
                    }
                    b"pos" => {
                        if let (Some(a), Some(x), Some(y)) = (current.as_mut(), attr_f64(e, b"x"), attr_f64(e, b"y")) {
                            a.pos = Some((x, y));
                        }
                    }
                    b"chart" => {
                        if let (Some(a), Some(id)) = (current.as_mut(), attr(e, b"id")) {
                            a.chart = Some(id);
                        }
                    }
                    b"pic" => {
                        if let Some(a) = current.as_mut() {
                            a.picture = true;
                        }
                    }
                    b"sp" | b"cxnSp" => {
                        if let Some(a) = current.as_mut() {
                            a.shape = true;
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => match local(e.name().into_inner()) {
                b"from" | b"to" => {
                    if let (Some(a), Some((is_from, m))) = (current.as_mut(), marker.take()) {
                        if is_from {
                            a.from = Some(m);
                        } else {
                            a.to = Some(m);
                        }
                    }
                }
                b"twoCellAnchor" | b"oneCellAnchor" | b"absoluteAnchor" => {
                    if let Some(a) = current.take() {
                        out.push(a);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    out
}

struct ParsedChart {
    kind: &'static str,
    /// The file's chart kind isn't one a sheet has (or it combines several), so the nearest was taken.
    approximate: bool,
    title: String,
    refs: Vec<String>,
}

fn parse_chart(xml: &[u8]) -> Option<ParsedChart> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut b2 = Vec::new();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut kind: Option<&'static str> = None;
    let mut approximate = false;
    let mut bar_dir: Option<String> = None;
    let mut title = String::new();
    let mut title_cache = String::new();
    let mut refs = Vec::new();
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(ev, Event::Empty(_));
                let n = name_of(e).to_vec();
                let in_title = stack.len() >= 3 && stack[1] == b"chart" && stack[2] == b"title";
                let parent_is_plot = stack.last().is_some_and(|p| p == b"plotArea");
                if parent_is_plot {
                    let k: Option<(&'static str, bool)> = match n.as_slice() {
                        b"barChart" | b"bar3DChart" => Some(("column", false)),
                        b"lineChart" | b"line3DChart" => Some(("line", false)),
                        b"areaChart" | b"area3DChart" => Some(("area", false)),
                        b"pieChart" | b"pie3DChart" => Some(("pie", false)),
                        b"doughnutChart" | b"ofPieChart" => Some(("pie", true)),
                        b"scatterChart" => Some(("scatter", false)),
                        b"bubbleChart" => Some(("scatter", true)),
                        b"radarChart" | b"stockChart" => Some(("line", true)),
                        b"surfaceChart" | b"surface3DChart" => Some(("area", true)),
                        _ => None,
                    };
                    if let Some((k, approx)) = k {
                        if kind.is_none() {
                            kind = Some(k);
                            approximate |= approx;
                        } else {
                            approximate = true;
                        }
                    }
                }
                match n.as_slice() {
                    b"barDir" if bar_dir.is_none() => bar_dir = attr(e, b"val"),
                    b"f" if !empty => {
                        let t = read_text(&mut r, &mut b2, &[]);
                        if stack.iter().any(|s| s == b"ser") {
                            refs.push(t);
                        }
                        continue;
                    }
                    b"t" if in_title && !empty => {
                        title.push_str(&read_text(&mut r, &mut b2, &[]));
                        continue;
                    }
                    b"v" if in_title && !empty => {
                        if title_cache.is_empty() {
                            title_cache = read_text(&mut r, &mut b2, &[]);
                        } else {
                            read_text(&mut r, &mut b2, &[]);
                        }
                        continue;
                    }
                    _ => {}
                }
                if !empty {
                    stack.push(n);
                }
            }
            Event::End(_) => {
                stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut kind = kind?;
    if kind == "column" && bar_dir.as_deref() == Some("bar") {
        kind = "bar";
    }
    let title = if title.trim().is_empty() { title_cache } else { title };
    Some(ParsedChart { kind, approximate, title: title.trim().to_owned(), refs })
}

/// The one range holding every series' data, names and categories, as A1 text (prefixed with the tab's name
/// when it isn't the chart's own tab). None when they aren't all on one tab.
fn chart_range(refs: &[String], own_tab: &str) -> Option<String> {
    let mut tab: Option<String> = None;
    let mut rect: Option<Rect> = None;
    for f in refs {
        let f = f.trim().trim_start_matches('(').trim_end_matches(')');
        for part in split_areas(f) {
            let (t, rest) = split_tab(&part);
            let t = t.unwrap_or_else(|| own_tab.to_owned());
            match &tab {
                Some(existing) if !existing.eq_ignore_ascii_case(&t) => return None,
                None => tab = Some(t),
                _ => {}
            }
            let area = Rect::parse(rest, MAX_ROWS, MAX_COLS)?;
            rect = Some(match rect {
                Some(r) => Rect::new(r.r1.min(area.r1), r.c1.min(area.c1), r.r2.max(area.r2), r.c2.max(area.c2)),
                None => area,
            });
        }
    }
    let (tab, rect) = (tab?, rect?);
    Some(if tab.eq_ignore_ascii_case(own_tab) { rect.to_string() } else { format!("{}!{rect}", quote_tab(&tab)) })
}

/// `Sheet1!$A$1:$A$3,Sheet1!$C$1:$C$3` → the areas, respecting quoted tab names.
fn split_areas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '\'' => {
                quoted = !quoted;
                cur.push(c);
            }
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.into_iter().map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_parts() {
        let xml = br#"<c:chartSpace xmlns:c="c" xmlns:a="a"><c:chart><c:title><c:tx><c:rich><a:p><a:r><a:t>Revenue</a:t></a:r><a:r><a:t> by month</a:t></a:r></a:p></c:rich></c:tx></c:title>
<c:plotArea><c:barChart><c:barDir val="bar"/><c:ser><c:tx><c:strRef><c:f>Sales!$B$1</c:f></c:strRef></c:tx>
<c:cat><c:strRef><c:f>Sales!$A$2:$A$6</c:f></c:strRef></c:cat><c:val><c:numRef><c:f>Sales!$B$2:$B$6</c:f></c:numRef></c:val></c:ser>
<c:ser><c:tx><c:strRef><c:f>Sales!$C$1</c:f></c:strRef></c:tx><c:val><c:numRef><c:f>Sales!$C$2:$C$6</c:f></c:numRef></c:val></c:ser></c:barChart>
<c:valAx><c:title><c:tx><c:rich><a:p><a:r><a:t>Axis</a:t></a:r></a:p></c:rich></c:tx></c:title></c:valAx></c:plotArea></c:chart></c:chartSpace>"#;
        let p = parse_chart(xml).expect("chart");
        assert_eq!(p.kind, "bar");
        assert_eq!(p.title, "Revenue by month");
        assert_eq!(chart_range(&p.refs, "Sales").as_deref(), Some("A1:C6"));
        assert_eq!(chart_range(&p.refs, "Summary").as_deref(), Some("Sales!A1:C6"));
        assert_eq!(chart_range(&["('My data'!$A$1:$A$3,'My data'!$C$1:$C$3)".into()], "x").as_deref(), Some("'My data'!A1:C3"));
        assert_eq!(chart_range(&["A!$A$1".into(), "B!$A$2".into()], "x"), None);
    }
}
