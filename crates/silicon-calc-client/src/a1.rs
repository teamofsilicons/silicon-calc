//! A1 addresses, as the service takes them: cells (`B7`), ranges (`A1:C9`), whole columns (`A:C`), whole rows
//! (`3:5`), and a tab in front (`Sales!A1:B9`, `'My tab'!A1`). Positions here are zero-based.

use std::fmt;

/// The most columns a tab may have (Excel's `XFD`).
pub const MAX_COLS: u32 = 16_384;
/// The most rows a tab may have.
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

/// `A` → `0`, `AA` → `26`, case-insensitive. None for anything that isn't column letters within the limit.
pub fn col_index(name: &str) -> Option<u32> {
    let name = name.trim().trim_start_matches('$');
    if name.is_empty() || name.len() > 3 {
        return None;
    }
    let mut n: u32 = 0;
    for b in name.bytes() {
        let up = b.to_ascii_uppercase();
        if !up.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + u32::from(up - b'A') + 1;
    }
    let idx = n - 1;
    (idx < MAX_COLS).then_some(idx)
}

/// A cell by position (zero-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRef {
    /// Zero-based row.
    pub row: u32,
    /// Zero-based column.
    pub col: u32,
}

impl CellRef {
    /// A cell at zero-based `row` and `col`.
    pub fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }

    /// `B7` → row 6, column 1. `$` marks are ignored.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('$', "");
        let split = s.find(|c: char| c.is_ascii_digit())?;
        let (letters, digits) = s.split_at(split);
        let col = col_index(letters)?;
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let row: u32 = digits.parse().ok()?;
        if row == 0 || row > MAX_ROWS {
            return None;
        }
        Some(Self { row: row - 1, col })
    }
}

impl fmt::Display for CellRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", col_name(self.col), self.row + 1)
    }
}

/// What a range covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeKind {
    /// One cell or a block of cells (`B7`, `A1:C9`).
    Cells,
    /// Whole columns (`A:C`).
    Cols,
    /// Whole rows (`3:5`).
    Rows,
    /// From a cell to the end of its columns (`A2:B`).
    OpenRows,
}

/// A rectangle of cells by position, inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeRef {
    /// First row (zero-based).
    pub r1: u32,
    /// First column.
    pub c1: u32,
    /// Last row, inclusive.
    pub r2: u32,
    /// Last column, inclusive.
    pub c2: u32,
    /// The form it was written in.
    pub kind: RangeKind,
}

impl RangeRef {
    /// A block of cells.
    pub fn cells(r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        Self { r1: r1.min(r2), c1: c1.min(c2), r2: r1.max(r2), c2: c1.max(c2), kind: RangeKind::Cells }
    }

    /// `A1:C9`, `B7`, `A:C`, `3:5`, `A2:B` (open-ended). Ends may come in either order.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().replace('$', "");
        if s.is_empty() {
            return None;
        }
        let (a, b) = match s.split_once(':') {
            Some((a, b)) => (a.trim().to_owned(), b.trim().to_owned()),
            None => (s.clone(), s.clone()),
        };
        if let (Some(c1), Some(c2)) = (col_index(&a), col_index(&b)) {
            return Some(Self { r1: 0, c1: c1.min(c2), r2: MAX_ROWS - 1, c2: c1.max(c2), kind: RangeKind::Cols });
        }
        if let (Ok(r1), Ok(r2)) = (a.parse::<u32>(), b.parse::<u32>()) {
            if r1 == 0 || r2 == 0 || r1 > MAX_ROWS || r2 > MAX_ROWS {
                return None;
            }
            return Some(Self {
                r1: r1.min(r2) - 1,
                c1: 0,
                r2: r1.max(r2) - 1,
                c2: MAX_COLS - 1,
                kind: RangeKind::Rows,
            });
        }
        let p1 = CellRef::parse(&a)?;
        if let Some(c2) = col_index(&b) {
            return Some(Self {
                r1: p1.row,
                c1: p1.col.min(c2),
                r2: MAX_ROWS - 1,
                c2: p1.col.max(c2),
                kind: RangeKind::OpenRows,
            });
        }
        let p2 = CellRef::parse(&b)?;
        Some(Self::cells(p1.row, p1.col, p2.row, p2.col))
    }

    /// The top-left cell.
    pub fn start(&self) -> CellRef {
        CellRef::new(self.r1, self.c1)
    }

    /// How many rows it covers.
    pub fn rows(&self) -> u32 {
        self.r2 - self.r1 + 1
    }

    /// How many columns it covers.
    pub fn cols(&self) -> u32 {
        self.c2 - self.c1 + 1
    }

    /// True when it is one cell.
    pub fn is_cell(&self) -> bool {
        self.kind == RangeKind::Cells && self.r1 == self.r2 && self.c1 == self.c2
    }

    /// True when `cell` is inside.
    pub fn contains(&self, cell: CellRef) -> bool {
        cell.row >= self.r1 && cell.row <= self.r2 && cell.col >= self.c1 && cell.col <= self.c2
    }

    /// True when the two overlap.
    pub fn intersects(&self, other: &RangeRef) -> bool {
        self.r1 <= other.r2 && other.r1 <= self.r2 && self.c1 <= other.c2 && other.c1 <= self.c2
    }
}

impl fmt::Display for RangeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RangeKind::Cols => write!(f, "{}:{}", col_name(self.c1), col_name(self.c2)),
            RangeKind::Rows => write!(f, "{}:{}", self.r1 + 1, self.r2 + 1),
            RangeKind::OpenRows => write!(f, "{}:{}", self.start(), col_name(self.c2)),
            RangeKind::Cells if self.is_cell() => write!(f, "{}", self.start()),
            RangeKind::Cells => write!(f, "{}:{}", self.start(), CellRef::new(self.r2, self.c2)),
        }
    }
}

/// Splits `Sales!A1:B2` or `'My tab'!A1` into the tab name and the rest. `'It''s'` unquotes to `It's`.
pub fn split_tab(s: &str) -> (Option<String>, &str) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('\'') {
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

/// A tab name as a formula or range writes it: quoted when it has to be.
pub fn quote_tab(name: &str) -> String {
    let plain = !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && CellRef::parse(name).is_none()
        && !name.eq_ignore_ascii_case("true")
        && !name.eq_ignore_ascii_case("false");
    if plain { name.to_owned() } else { format!("'{}'", name.replace('\'', "''")) }
}

/// `Sales!A1:B2` from a tab name and a range.
pub fn with_tab(tab: &str, range: &str) -> String {
    format!("{}!{range}", quote_tab(tab))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
        assert_eq!(col_name(16383), "XFD");
        assert_eq!(col_index("xfd"), Some(16383));
        assert_eq!(col_index("XFE"), None);
        assert_eq!(col_index("A1"), None);
        assert_eq!(col_index(""), None);
        for i in [0, 1, 25, 26, 27, 701, 702, 9999, 16383] {
            assert_eq!(col_index(&col_name(i)), Some(i));
        }
    }

    #[test]
    fn cells() {
        assert_eq!(CellRef::parse("B7"), Some(CellRef::new(6, 1)));
        assert_eq!(CellRef::parse("$b$7"), Some(CellRef::new(6, 1)));
        assert_eq!(CellRef::parse("A0"), None);
        assert_eq!(CellRef::parse("7"), None);
        assert_eq!(CellRef::parse("B7x"), None);
        assert_eq!(CellRef::new(0, 27).to_string(), "AB1");
    }

    #[test]
    fn ranges() {
        let r = RangeRef::parse("B2:A1").unwrap();
        assert_eq!((r.r1, r.c1, r.r2, r.c2, r.kind), (0, 0, 1, 1, RangeKind::Cells));
        assert_eq!(r.to_string(), "A1:B2");
        let r = RangeRef::parse("A:C").unwrap();
        assert_eq!((r.c1, r.c2, r.kind), (0, 2, RangeKind::Cols));
        assert_eq!(r.to_string(), "A:C");
        let r = RangeRef::parse("5:3").unwrap();
        assert_eq!((r.r1, r.r2, r.kind), (2, 4, RangeKind::Rows));
        assert_eq!(r.to_string(), "3:5");
        let r = RangeRef::parse("B7").unwrap();
        assert!(r.is_cell());
        assert_eq!(r.to_string(), "B7");
        let r = RangeRef::parse("A2:B").unwrap();
        assert_eq!((r.r1, r.c1, r.c2, r.kind), (1, 0, 1, RangeKind::OpenRows));
        assert_eq!(r.to_string(), "A2:B");
        assert!(RangeRef::parse("").is_none());
        assert!(RangeRef::parse("0:3").is_none());
        assert!(RangeRef::parse("hello").is_none());
        assert_eq!(RangeRef::parse("A1:C3").unwrap().rows(), 3);
        assert!(RangeRef::parse("A1:C3").unwrap().contains(CellRef::new(2, 2)));
        assert!(!RangeRef::parse("A1:C3").unwrap().contains(CellRef::new(3, 2)));
        assert!(RangeRef::parse("A1:C3").unwrap().intersects(&RangeRef::parse("C3:D9").unwrap()));
    }

    #[test]
    fn tabs_in_front() {
        assert_eq!(split_tab("'It''s'!A1"), (Some("It's".into()), "A1"));
        assert_eq!(split_tab("Sales!A1:B2"), (Some("Sales".into()), "A1:B2"));
        assert_eq!(split_tab("'My tab'!B:B"), (Some("My tab".into()), "B:B"));
        assert_eq!(split_tab("A1"), (None, "A1"));
        assert_eq!(quote_tab("Sales"), "Sales");
        assert_eq!(quote_tab("My tab"), "'My tab'");
        assert_eq!(quote_tab("A1"), "'A1'");
        assert_eq!(quote_tab("2024"), "'2024'");
        assert_eq!(with_tab("It's", "A1"), "'It''s'!A1");
    }
}
