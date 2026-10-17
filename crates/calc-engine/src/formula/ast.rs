//! A formula's tree. References are stored relative to the cell that holds the formula (R1C1 style), so a formula
//! filled down a column is one tree shared by every cell.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::value::ErrorKind;

/// A tab's stable id.
pub type TabId = u32;

/// One axis of a reference: an absolute position (`$B`, `$7`) or an offset from the formula's own cell (`B`, `7`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Coord {
    Abs(u32),
    Rel(i32),
}

impl Coord {
    /// The position this axis points at from a formula at `at`, or `None` when it falls off the grid.
    pub fn resolve(self, at: u32) -> Option<u32> {
        match self {
            Self::Abs(p) => Some(p),
            Self::Rel(d) => {
                let p = at as i64 + d as i64;
                (p >= 0 && p <= u32::MAX as i64).then_some(p as u32)
            }
        }
    }

    pub fn is_abs(self) -> bool {
        matches!(self, Self::Abs(_))
    }
}

/// A single cell reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellRef {
    pub tab: Option<TabId>,
    pub row: Coord,
    pub col: Coord,
}

/// A rectangular reference. `None` on an axis means the whole axis (`A:C` has no rows, `3:5` no columns).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RangeRef {
    pub tab: Option<TabId>,
    pub rows: Option<(Coord, Coord)>,
    pub cols: Option<(Coord, Coord)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Neg,
    Plus,
    Percent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// `A1:B2` when either side is not a plain reference (`A1:INDEX(…)`).
    Range,
    /// The space operator.
    Intersect,
}

impl BinaryOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Pow => "^",
            Self::Concat => "&",
            Self::Eq => "=",
            Self::Ne => "<>",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Range => ":",
            Self::Intersect => " ",
        }
    }

    pub fn precedence(self) -> u8 {
        match self {
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge => 1,
            Self::Concat => 2,
            Self::Add | Self::Sub => 3,
            Self::Mul | Self::Div => 4,
            Self::Pow => 5,
            Self::Intersect => 8,
            Self::Range => 9,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(ErrorKind),
    /// An argument left out: `IF(A1,,2)`.
    Missing,
    Cell(CellRef),
    Range(RangeRef),
    /// `A1#`: the whole spill of the formula in A1.
    Spill(CellRef),
    /// A defined name, or a name nothing defines (which evaluates to `#NAME?`).
    Name(Arc<str>),
    Unary(UnaryOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// A function call. The name is upper case without Excel's `_xlfn.` prefix.
    Call(Arc<str>, Vec<Expr>),
    Array(Vec<Vec<Expr>>),
    Paren(Box<Expr>),
}

impl Expr {
    /// Walks every node, depth first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Expr)) {
        f(self);
        match self {
            Self::Unary(_, e) | Self::Paren(e) => e.walk(f),
            Self::Binary(_, a, b) => {
                a.walk(f);
                b.walk(f);
            }
            Self::Call(_, args) => args.iter().for_each(|a| a.walk(f)),
            Self::Array(rows) => rows.iter().flatten().for_each(|a| a.walk(f)),
            _ => {}
        }
    }

    /// Rebuilds the tree with `f` applied to every reference node (bottom up). `f` returns the replacement.
    pub fn map_refs(&self, f: &mut impl FnMut(&Expr) -> Option<Expr>) -> Expr {
        match self {
            Self::Cell(_) | Self::Range(_) | Self::Spill(_) => f(self).unwrap_or_else(|| self.clone()),
            Self::Unary(op, e) => Self::Unary(*op, Box::new(e.map_refs(f))),
            Self::Paren(e) => Self::Paren(Box::new(e.map_refs(f))),
            Self::Binary(op, a, b) => Self::Binary(*op, Box::new(a.map_refs(f)), Box::new(b.map_refs(f))),
            Self::Call(n, args) => Self::Call(n.clone(), args.iter().map(|a| a.map_refs(f)).collect()),
            Self::Array(rows) => Self::Array(rows.iter().map(|r| r.iter().map(|a| a.map_refs(f)).collect()).collect()),
            other => other.clone(),
        }
    }

    /// Whether evaluating it can give a different answer with nothing changed (NOW, RAND, INDIRECT, OFFSET…).
    pub fn is_volatile(&self) -> bool {
        let mut volatile = false;
        self.walk(&mut |e| {
            if let Expr::Call(name, _) = e
                && matches!(
                    name.as_ref(),
                    "NOW" | "TODAY" | "RAND" | "RANDBETWEEN" | "RANDARRAY" | "INDIRECT" | "OFFSET" | "CELL" | "INFO"
                )
            {
                volatile = true;
            }
            if let Expr::Name(_) = e {
                // A name can be redefined, so treat it as volatile.
                volatile = true;
            }
        });
        volatile
    }
}
