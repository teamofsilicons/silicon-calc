//! `xl/styles.xml`: number formats (custom and built-in), fonts, fills, borders and alignment, resolved into one
//! engine [`Style`] per cell format (`cellXfs` entry).

use ahash::HashMap;
use quick_xml::events::Event;

use crate::{
    formats::{
        colors::{ColorSpec, Palette, parse_hex},
        numfmts,
        xml::{attr, attr_f64, attr_u32, local, name_of, reader, skip_element, truthy},
    },
    numfmt::is_date_format,
    style::{Border, BorderStyle, HAlign, Style, VAlign},
};

#[derive(Clone, Debug, Default)]
struct Font {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: Option<f64>,
    name: Option<String>,
    color: ColorSpec,
}

#[derive(Clone, Debug, Default)]
struct Fill {
    pattern: Option<String>,
    fg: ColorSpec,
    bg: ColorSpec,
    /// The first stop of a gradient fill.
    gradient: Option<ColorSpec>,
}

#[derive(Clone, Debug, Default)]
struct Side {
    style: Option<String>,
    color: ColorSpec,
}

#[derive(Clone, Debug, Default)]
struct Borders {
    left: Side,
    right: Side,
    top: Side,
    bottom: Side,
}

#[derive(Clone, Debug, Default)]
struct Xf {
    num_fmt: u32,
    font: u32,
    fill: u32,
    border: u32,
    horizontal: Option<String>,
    vertical: Option<String>,
    wrap: bool,
    rotated: bool,
}

/// One engine style per `cellXfs` index.
#[derive(Clone, Debug, Default)]
pub(crate) struct CellStyles {
    pub xfs: Vec<Style>,
    /// Whether the format shows dates (numbers in 1904 workbooks are shifted; `t="d"` cells read as dates).
    pub is_date: Vec<bool>,
    /// Styles that lost a text rotation.
    pub rotated: Vec<bool>,
}

impl CellStyles {
    pub fn style(&self, xf: u32) -> Option<&Style> {
        self.xfs.get(xf as usize)
    }

    pub fn is_date(&self, xf: u32) -> bool {
        self.is_date.get(xf as usize).copied().unwrap_or(false)
    }
}

fn side_border(s: &Side, palette: &Palette) -> Option<Border> {
    let style = match s.style.as_deref()? {
        "thin" | "hair" => BorderStyle::Thin,
        "medium" => BorderStyle::Medium,
        "thick" => BorderStyle::Thick,
        "dashed" | "mediumDashed" | "dashDot" | "mediumDashDot" | "dashDotDot" | "mediumDashDotDot" | "slantDashDot" => {
            BorderStyle::Dashed
        }
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        _ => return None,
    };
    let color = palette.hex(&s.color).filter(|c| c != "#000000");
    Some(Border { style, color })
}

/// Reads the styles part. The file's own indexed palette, if any, replaces the default in `palette`.
pub(crate) fn read(xml: &[u8], palette: &mut Palette) -> CellStyles {
    let mut num_fmts: HashMap<u32, String> = HashMap::default();
    let mut fonts: Vec<Font> = Vec::new();
    let mut fills: Vec<Fill> = Vec::new();
    let mut borders: Vec<Borders> = Vec::new();
    let mut xfs: Vec<Xf> = Vec::new();
    let mut indexed: Vec<u32> = Vec::new();

    #[derive(PartialEq, Clone, Copy)]
    enum Section {
        None,
        Fonts,
        Fills,
        Borders,
        CellXfs,
        Other,
    }
    let mut section = Section::None;
    let mut font: Option<Font> = None;
    let mut fill: Option<Fill> = None;
    let mut border: Option<Borders> = None;
    let mut side: Option<&'static str> = None;
    let mut xf: Option<Xf> = None;
    let mut in_gradient_stop = false;

    let mut r = reader(xml);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                let n = name_of(e);
                match n {
                    b"numFmt" => {
                        if let (Some(id), Some(code)) = (attr_u32(e, b"numFmtId"), attr(e, b"formatCode")) {
                            num_fmts.insert(id, code);
                        }
                    }
                    b"fonts" => section = Section::Fonts,
                    b"fills" => section = Section::Fills,
                    b"borders" => section = Section::Borders,
                    b"cellXfs" => section = Section::CellXfs,
                    b"cellStyleXfs" | b"dxfs" | b"cellStyles" | b"tableStyles" => {
                        // Not cell formats; their fonts and fills must not be read as the workbook's.
                        section = Section::Other;
                        if !empty {
                            let mut b2 = Vec::new();
                            skip_element(&mut r, &mut b2);
                            section = Section::None;
                        }
                    }
                    b"extLst" if !empty => {
                        let mut b2 = Vec::new();
                        skip_element(&mut r, &mut b2);
                    }
                    b"rgbColor" => {
                        if let Some(rgb) = attr(e, b"rgb").and_then(|v| parse_hex(&v)) {
                            indexed.push(rgb);
                        }
                    }
                    b"font" if section == Section::Fonts => {
                        font = Some(Font::default());
                        if empty {
                            fonts.push(font.take().unwrap_or_default());
                        }
                    }
                    b"b" | b"i" | b"strike" | b"u" | b"sz" | b"name" | b"rFont" | b"color" if font.is_some() => {
                        let f = font.as_mut().expect("checked");
                        let on = attr(e, b"val").is_none_or(|v| truthy(&v) || (n == b"u" && v != "none"));
                        match n {
                            b"b" => f.bold = on,
                            b"i" => f.italic = on,
                            b"strike" => f.strike = on,
                            b"u" => f.underline = on,
                            b"sz" => f.size = attr_f64(e, b"val"),
                            b"name" | b"rFont" => f.name = attr(e, b"val"),
                            _ => f.color = ColorSpec::from_element(e),
                        }
                    }
                    b"fill" if section == Section::Fills => {
                        fill = Some(Fill::default());
                        if empty {
                            fills.push(fill.take().unwrap_or_default());
                        }
                    }
                    b"patternFill" if fill.is_some() => {
                        fill.as_mut().expect("checked").pattern = Some(attr(e, b"patternType").unwrap_or_else(|| "solid".into()))
                    }
                    b"fgColor" if fill.is_some() => fill.as_mut().expect("checked").fg = ColorSpec::from_element(e),
                    b"bgColor" if fill.is_some() => fill.as_mut().expect("checked").bg = ColorSpec::from_element(e),
                    b"stop" if fill.is_some() => in_gradient_stop = true,
                    b"border" if section == Section::Borders => {
                        border = Some(Borders::default());
                        if empty {
                            borders.push(border.take().unwrap_or_default());
                        }
                    }
                    b"left" | b"right" | b"top" | b"bottom" | b"start" | b"end" if border.is_some() => {
                        let which: &'static str = match n {
                            b"left" | b"start" => "left",
                            b"right" | b"end" => "right",
                            b"top" => "top",
                            _ => "bottom",
                        };
                        let s = Side { style: attr(e, b"style").filter(|s| s != "none"), color: ColorSpec::default() };
                        let b = border.as_mut().expect("checked");
                        match which {
                            "left" => b.left = s,
                            "right" => b.right = s,
                            "top" => b.top = s,
                            _ => b.bottom = s,
                        }
                        side = if empty { None } else { Some(which) };
                    }
                    b"color" if border.is_some() || (fill.is_some() && in_gradient_stop) => {
                        let c = ColorSpec::from_element(e);
                        if let Some(f) = fill.as_mut() {
                            if f.gradient.is_none() {
                                f.gradient = Some(c);
                            }
                        } else if let (Some(b), Some(which)) = (border.as_mut(), side) {
                            match which {
                                "left" => b.left.color = c,
                                "right" => b.right.color = c,
                                "top" => b.top.color = c,
                                _ => b.bottom.color = c,
                            }
                        }
                    }
                    b"xf" if section == Section::CellXfs => {
                        let x = Xf {
                            num_fmt: attr_u32(e, b"numFmtId").unwrap_or(0),
                            font: attr_u32(e, b"fontId").unwrap_or(0),
                            fill: attr_u32(e, b"fillId").unwrap_or(0),
                            border: attr_u32(e, b"borderId").unwrap_or(0),
                            ..Default::default()
                        };
                        if empty {
                            xfs.push(x);
                        } else {
                            xf = Some(x);
                        }
                    }
                    b"alignment" if xf.is_some() => {
                        let x = xf.as_mut().expect("checked");
                        x.horizontal = attr(e, b"horizontal");
                        x.vertical = attr(e, b"vertical");
                        x.wrap = attr(e, b"wrapText").is_some_and(|v| truthy(&v));
                        x.rotated = attr_u32(e, b"textRotation").is_some_and(|v| v != 0);
                    }
                    _ => {}
                }
            }
            Event::End(ref e) => match local(e.name().into_inner()) {
                b"font" => {
                    if let Some(f) = font.take() {
                        fonts.push(f);
                    }
                }
                b"fill" => {
                    if let Some(f) = fill.take() {
                        fills.push(f);
                    }
                }
                b"stop" => in_gradient_stop = false,
                b"border" => {
                    if let Some(b) = border.take() {
                        borders.push(b);
                    }
                }
                b"left" | b"right" | b"top" | b"bottom" | b"start" | b"end" => side = None,
                b"xf" => {
                    if let Some(x) = xf.take() {
                        xfs.push(x);
                    }
                }
                b"fonts" | b"fills" | b"borders" | b"cellXfs" => section = Section::None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if !indexed.is_empty() {
        for (i, rgb) in indexed.into_iter().enumerate() {
            if i < palette.indexed.len() {
                palette.indexed[i] = rgb;
            }
        }
    }

    let default_font = fonts.first().cloned().unwrap_or_default();
    let mut out = CellStyles::default();
    for x in &xfs {
        let mut s = Style::default();
        let code = num_fmts.get(&x.num_fmt).map(String::as_str).or_else(|| numfmts::builtin(x.num_fmt));
        s.num_fmt = code.and_then(numfmts::engine_code);
        if let Some(f) = fonts.get(x.font as usize) {
            s.bold = f.bold;
            s.italic = f.italic;
            s.underline = f.underline;
            s.strike = f.strike;
            if let Some(size) = f.size
                && Some(size) != default_font.size
            {
                s.font_size = Some(size.round().clamp(1.0, 409.0) as u16);
            }
            if let Some(name) = &f.name
                && default_font.name.as_ref() != Some(name)
            {
                s.font = Some(name.clone());
            }
            if !f.color.auto {
                s.color = palette.hex(&f.color).filter(|c| c != "#000000");
            }
        }
        if let Some(f) = fills.get(x.fill as usize) {
            s.fill = match f.pattern.as_deref() {
                Some("none") | Some("gray125") => None,
                Some(_) => {
                    // A solid fill's colour is its foreground; other patterns show mostly their foreground too.
                    let c = if f.fg.is_set() { &f.fg } else { &f.bg };
                    palette.hex(c)
                }
                None => f.gradient.as_ref().and_then(|g| palette.hex(g)),
            };
        }
        if let Some(b) = borders.get(x.border as usize) {
            s.border_left = side_border(&b.left, palette);
            s.border_right = side_border(&b.right, palette);
            s.border_top = side_border(&b.top, palette);
            s.border_bottom = side_border(&b.bottom, palette);
        }
        s.h_align = match x.horizontal.as_deref() {
            Some("left") | Some("fill") | Some("justify") | Some("distributed") => HAlign::Left,
            Some("center") | Some("centerContinuous") => HAlign::Center,
            Some("right") => HAlign::Right,
            _ => HAlign::General,
        };
        s.v_align = match x.vertical.as_deref() {
            Some("top") => VAlign::Top,
            Some("center") | Some("justify") | Some("distributed") => VAlign::Middle,
            _ => VAlign::Bottom,
        };
        s.wrap = x.wrap;
        out.is_date.push(s.num_fmt.as_deref().is_some_and(is_date_format));
        out.rotated.push(x.rotated);
        out.xfs.push(s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_styles() {
        let xml = br##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd"/></numFmts>
<fonts count="3"><font><sz val="11"/><color theme="1"/><name val="Calibri"/><family val="2"/><scheme val="minor"/></font>
<font><b/><i/><u/><sz val="14"/><color rgb="FFC00000"/><name val="Calibri"/></font>
<font><strike/><sz val="11"/><color indexed="12"/><name val="Arial"/></font></fonts>
<fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill>
<fill><patternFill patternType="solid"><fgColor theme="4" tint="0.79998168889431442"/><bgColor indexed="64"/></patternFill></fill></fills>
<borders count="2"><border><left/><right/><top/><bottom/><diagonal/></border>
<border><left style="thin"><color indexed="64"/></left><right/><top style="medium"><color rgb="FF1F5FB8"/></top><bottom style="double"/><diagonal/></border></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="4"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="14" fontId="1" fillId="2" borderId="1" xfId="0" applyNumberFormat="1" applyFont="1"><alignment horizontal="center" vertical="center" wrapText="1"/></xf>
<xf numFmtId="164" fontId="2" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/>
<xf numFmtId="10" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>
<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
</styleSheet>"##;
        let mut palette = Palette::default();
        let s = read(xml, &mut palette);
        assert_eq!(s.xfs.len(), 4);
        assert!(s.xfs[0].is_default());
        let b = &s.xfs[1];
        assert!(b.bold && b.italic && b.underline && !b.strike);
        assert_eq!(b.font_size, Some(14));
        assert_eq!(b.font, None);
        assert_eq!(b.color.as_deref(), Some("#C00000"));
        assert_eq!(b.fill.as_deref(), Some("#DAE3F3"));
        assert_eq!(b.num_fmt.as_deref(), Some("m/d/yyyy"));
        assert_eq!(b.h_align, HAlign::Center);
        assert_eq!(b.v_align, VAlign::Middle);
        assert!(b.wrap);
        assert_eq!(b.border_left, Some(Border { style: BorderStyle::Thin, color: None }));
        assert_eq!(b.border_top, Some(Border { style: BorderStyle::Medium, color: Some("#1F5FB8".into()) }));
        assert_eq!(b.border_bottom.as_ref().map(|b| b.style), Some(BorderStyle::Double));
        assert!(s.is_date[1] && s.is_date[2] && !s.is_date[3]);
        let c = &s.xfs[2];
        assert!(c.strike);
        assert_eq!(c.font.as_deref(), Some("Arial"));
        assert_eq!(c.color.as_deref(), Some("#0000FF"));
        assert_eq!(c.num_fmt.as_deref(), Some("yyyy\\-mm\\-dd"));
        assert_eq!(s.xfs[3].num_fmt.as_deref(), Some("0.00%"));
    }
}
