//! Colours as spreadsheet files write them: `#RRGGBB`, Excel's ARGB, its 64-colour indexed palette (which a
//! file may replace), theme colours with a tint, and back.

use quick_xml::events::BytesStart;

use crate::formats::xml::{attr, attr_f64, attr_u32};

/// Excel's default indexed palette (0–63; 64 and 65 are the system foreground and background).
const INDEXED: [u32; 66] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000, 0xFFFFFF, 0xFF0000,
    0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000, 0x000080, 0x808000, 0x800080, 0x008080,
    0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC, 0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080,
    0xFF00FF, 0xFFFF00, 0x00FFFF, 0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99,
    0x99CCFF, 0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600, 0x666699,
    0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399, 0x333333, 0x000000, 0xFFFFFF,
];

/// The Office theme's colours (dk1, lt1, dk2, lt2, accent1–6, hlink, folHlink), used when a file has no theme.
const OFFICE_THEME: [u32; 12] =
    [0x000000, 0xFFFFFF, 0x44546A, 0xE7E6E6, 0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, 0x0563C1, 0x954F72];

/// A colour as a file names it, before the palette and theme are known.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ColorSpec {
    pub rgb: Option<u32>,
    pub theme: Option<u32>,
    pub indexed: Option<u32>,
    pub auto: bool,
    pub tint: f64,
}

impl ColorSpec {
    /// From a `<color>`, `<fgColor>`, `<tabColor>` … element.
    pub fn from_element(e: &BytesStart<'_>) -> Self {
        Self {
            rgb: attr(e, b"rgb").and_then(|v| parse_hex(&v)),
            theme: attr_u32(e, b"theme"),
            indexed: attr_u32(e, b"indexed"),
            auto: attr(e, b"auto").is_some_and(|v| crate::formats::xml::truthy(&v)),
            tint: attr_f64(e, b"tint").unwrap_or(0.0),
        }
    }

    pub fn is_set(&self) -> bool {
        self.rgb.is_some() || self.theme.is_some() || self.indexed.is_some()
    }
}

/// `FF1F5FB8`, `1F5FB8` or `#1F5FB8` as 0xRRGGBB.
pub(crate) fn parse_hex(v: &str) -> Option<u32> {
    let h = v.trim().trim_start_matches('#');
    let h = if h.len() == 8 { &h[2..] } else { h };
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// `#RRGGBB` (upper case).
pub(crate) fn hex(rgb: u32) -> String {
    format!("#{:06X}", rgb & 0xFF_FFFF)
}

/// What colours resolve against: the indexed palette (the default, or the file's) and the theme.
#[derive(Clone, Debug)]
pub(crate) struct Palette {
    pub indexed: Vec<u32>,
    pub theme: Vec<u32>,
}

impl Default for Palette {
    fn default() -> Self {
        Self { indexed: INDEXED.to_vec(), theme: OFFICE_THEME.to_vec() }
    }
}

impl Palette {
    /// The colour as 0xRRGGBB, or None for automatic / unknown.
    pub fn rgb(&self, c: &ColorSpec) -> Option<u32> {
        let base = if let Some(rgb) = c.rgb {
            rgb
        } else if let Some(t) = c.theme {
            // SpreadsheetML numbers the first four theme colours lt1, dk1, lt2, dk2.
            let i = match t {
                0 => 1,
                1 => 0,
                2 => 3,
                3 => 2,
                n => n as usize,
            };
            *self.theme.get(i)?
        } else if let Some(i) = c.indexed {
            *self.indexed.get(i as usize)?
        } else {
            return None;
        };
        Some(if c.tint != 0.0 { tint(base, c.tint) } else { base })
    }

    pub fn hex(&self, c: &ColorSpec) -> Option<String> {
        self.rgb(c).map(hex)
    }
}

/// Applies an OOXML tint (−1 darker … 1 lighter) to a colour's lightness.
pub(crate) fn tint(rgb: u32, tint: f64) -> u32 {
    let (h, s, l) = rgb_to_hsl(rgb);
    let l = if tint < 0.0 { l * (1.0 + tint) } else { l * (1.0 - tint) + tint };
    hsl_to_rgb(h, s, l.clamp(0.0, 1.0))
}

fn rgb_to_hsl(rgb: u32) -> (f64, f64, f64) {
    let r = ((rgb >> 16) & 0xFF) as f64 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f64 / 255.0;
    let b = (rgb & 0xFF) as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> u32 {
    let to = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u32;
    if s == 0.0 {
        let v = to(l);
        return (v << 16) | (v << 8) | v;
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let hue = |mut t: f64| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (to(hue(h + 1.0 / 3.0)) << 16) | (to(hue(h)) << 8) | to(hue(h - 1.0 / 3.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        let p = Palette::default();
        assert_eq!(p.hex(&ColorSpec { rgb: parse_hex("FF1F5FB8"), ..Default::default() }).as_deref(), Some("#1F5FB8"));
        assert_eq!(p.hex(&ColorSpec { theme: Some(1), ..Default::default() }).as_deref(), Some("#000000"));
        assert_eq!(p.hex(&ColorSpec { theme: Some(0), ..Default::default() }).as_deref(), Some("#FFFFFF"));
        assert_eq!(p.hex(&ColorSpec { theme: Some(4), ..Default::default() }).as_deref(), Some("#4472C4"));
        assert_eq!(p.hex(&ColorSpec { indexed: Some(10), ..Default::default() }).as_deref(), Some("#FF0000"));
        // White darkened by 15% is Excel's "White, Background 1, Darker 15%".
        assert_eq!(p.hex(&ColorSpec { theme: Some(0), tint: -0.1499984740745262, ..Default::default() }).as_deref(), Some("#D9D9D9"));
        // Accent 1 lighter 80%.
        assert_eq!(p.hex(&ColorSpec { theme: Some(4), tint: 0.7999816888943144, ..Default::default() }).as_deref(), Some("#DAE3F3"));
    }
}
