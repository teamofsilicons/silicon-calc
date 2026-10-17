//! Cell addresses by position: column letters, `B7`, ranges like `A1:C9`, `A:C`, `3:5`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The most columns a tab may have (Excel's XFD).
pub const MAX_COLS: u32 = 16_384;
/// The most rows a tab may have. Excel stops at 1,048,576; Calc holds ten times that.
pub const MAX_ROWS: u32 = 10_485_760;

/// `0` → `A`, `25` → `Z`, `26` → `AA`.
pub fn col_name(mut col: u32) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// `A` → `0`, `AA` → `26`. Case-insensitive.
pub fn col_index(name: &str) -> Option<u32> {
    if name.is_empty() || name.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for b in name.bytes() {
        let up = b.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + (up - b'A') as u32 + 1;
    }
    let idx = n - 1;
    (idx < MAX_COLS).then_some(idx)
}

/// A cell by position (zero-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellPos {
    pub row: u32,
    pub col: u32,
}

impl CellPos {
    pub fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }

    /// `B7` → row 6, col 1.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('$', "");
        let split = s.find(|c: char| c.is_ascii_digit())?;
        let (letters, digits) = s.split_at(split);
        let col = col_index(letters)?;
        let row: u32 = digits.parse().ok()?;
        if row == 0 || row > MAX_ROWS {
            return None;
        }
        Some(Self { row: row - 1, col })
    }
}

impl fmt::Display for CellPos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", col_name(self.col), self.row + 1)
    }
}

/// A rectangle of cells by position, inclusive. Whole columns and rows use the tab's limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl Rect {
    pub fn new(r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        Self { r1: r1.min(r2), c1: c1.min(c2), r2: r1.max(r2), c2: c1.max(c2) }
    }

    pub fn cell(p: CellPos) -> Self {
        Self { r1: p.row, c1: p.col, r2: p.row, c2: p.col }
    }

    pub fn rows(&self) -> u32 {
        self.r2 - self.r1 + 1
    }

    pub fn cols(&self) -> u32 {
        self.c2 - self.c1 + 1
    }

    pub fn contains(&self, p: CellPos) -> bool {
        p.row >= self.r1 && p.row <= self.r2 && p.col >= self.c1 && p.col <= self.c2
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.r1 <= o.r2 && o.r1 <= self.r2 && self.c1 <= o.c2 && o.c1 <= self.c2
    }

    pub fn intersection(&self, o: &Rect) -> Option<Rect> {
        self.intersects(o).then(|| Rect {
            r1: self.r1.max(o.r1),
            c1: self.c1.max(o.c1),
            r2: self.r2.min(o.r2),
            c2: self.c2.min(o.c2),
        })
    }

    /// `A1:C9`, `B7`, `A:C`, `3:5`, `A1:A` (open-ended). `max_rows`/`max_cols` bound whole columns and rows.
    pub fn parse(s: &str, max_rows: u32, max_cols: u32) -> Option<Self> {
        let s = s.trim().replace('$', "");
        let (a, b) = match s.split_once(':') {
            Some((a, b)) => (a.to_owned(), b.to_owned()),
            None => (s.clone(), s.clone()),
        };
        let end_row = max_rows.saturating_sub(1);
        let end_col = max_cols.saturating_sub(1);
        // Whole columns: A:C
        if let (Some(c1), Some(c2)) = (col_index(&a), col_index(&b)) {
            return Some(Rect::new(0, c1, end_row, c2));
        }
        // Whole rows: 3:5
        if let (Ok(r1), Ok(r2)) = (a.parse::<u32>(), b.parse::<u32>()) {
            if r1 == 0 || r2 == 0 {
                return None;
            }
            return Some(Rect::new(r1 - 1, 0, r2 - 1, end_col));
        }
        let p1 = CellPos::parse(&a)?;
        // A1:A means from A1 down the whole column.
        if let Some(c2) = col_index(&b) {
            return Some(Rect::new(p1.row, p1.col, end_row, c2));
        }
        let p2 = CellPos::parse(&b)?;
        Some(Rect::new(p1.row, p1.col, p2.row, p2.col))
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let a = CellPos::new(self.r1, self.c1);
        if self.r1 == self.r2 && self.c1 == self.c2 {
            write!(f, "{a}")
        } else {
            write!(f, "{a}:{}", CellPos::new(self.r2, self.c2))
        }
    }
}

/// Splits `Sales!A1:B2` or `'My tab'!A1` into the tab name and the rest.
pub fn split_tab(s: &str) -> (Option<String>, &str) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('\'') {
        // 'It''s'!A1
        let mut name = String::new();
        let mut chars = rest.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if matches!(chars.peek(), Some((_, '\''))) {
                    name.push('\'');
                    chars.next();
                    continue;
                }
                let after = &rest[i + 1..];
                if let Some(r) = after.strip_prefix('!') {
                    return (Some(name), r);
                }
                return (None, s);
            }
            name.push(c);
        }
        return (None, s);
    }
    match s.rsplit_once('!') {
        Some((tab, r)) if !tab.is_empty() => (Some(tab.to_owned()), r),
        _ => (None, s),
    }
}

/// A tab name as a formula writes it: quoted when it needs to be.
pub fn quote_tab(name: &str) -> String {
    let plain = !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && !looks_like_ref(name);
    if plain { name.to_owned() } else { format!("'{}'", name.replace('\'', "''")) }
}

fn looks_like_ref(s: &str) -> bool {
    CellPos::parse(s).is_some() || s.eq_ignore_ascii_case("TRUE") || s.eq_ignore_ascii_case("FALSE")
        || s.to_ascii_uppercase().starts_with('R') && s[1..].chars().all(|c| c.is_ascii_digit() || c == 'C' || c == 'c')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(16383), "XFD");
        assert_eq!(col_index("xfd"), Some(16383));
        assert_eq!(col_index("XFE"), None);
        for i in [0, 1, 27, 701, 702, 9999] {
            assert_eq!(col_index(&col_name(i)), Some(i));
        }
    }

    #[test]
    fn ranges() {
        assert_eq!(Rect::parse("B2:A1", 100, 10), Some(Rect::new(0, 0, 1, 1)));
        assert_eq!(Rect::parse("A:C", 100, 10), Some(Rect::new(0, 0, 99, 2)));
        assert_eq!(Rect::parse("3:5", 100, 10), Some(Rect::new(2, 0, 4, 9)));
        assert_eq!(Rect::parse("$B$7", 100, 10), Some(Rect::new(6, 1, 6, 1)));
        assert_eq!(split_tab("'It''s'!A1"), (Some("It's".into()), "A1"));
        assert_eq!(split_tab("Sales!A1:B2"), (Some("Sales".into()), "A1:B2"));
        assert_eq!(quote_tab("Sales"), "Sales");
        assert_eq!(quote_tab("My tab"), "'My tab'");
        assert_eq!(quote_tab("A1"), "'A1'");
    }
}
