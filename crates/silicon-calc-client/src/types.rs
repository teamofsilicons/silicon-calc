//! Every shape in the service's API (`docs/API.md`), as serde types.
//!
//! Answers are read leniently: optional and list fields default when absent, so a newer service that adds a field
//! never breaks an older client. Times are RFC 3339 UTC strings.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as Json;

// ── Accounts ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A Carbon or Silicon, always shown by its current `c:`/`si:` id and stored by uuid.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    /// The permanent uuid.
    pub uuid: String,
    /// `c:ada`, `si:analyst`. Empty for a deleted account.
    pub id: String,
    /// `carbon` or `silicon`.
    pub kind: String,
    /// The name it shows.
    pub display_name: String,
    /// Its picture.
    pub pfp_url: String,
}

impl Account {
    /// The id people see (`c:ada`), or the display name when the account has none (deleted), or
    /// `a deleted account` when it has neither.
    pub fn label(&self) -> &str {
        if !self.id.is_empty() {
            &self.id
        } else if !self.display_name.is_empty() {
            &self.display_name
        } else {
            "a deleted account"
        }
    }

    /// True for a Silicon.
    pub fn is_silicon(&self) -> bool {
        self.kind == "silicon"
    }
}

/// `GET /v1/me`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Me {
    /// The signed-in account.
    pub account: Account,
}

/// An account given either in full or by its uuid (answers that carry an `accounts` map use uuids).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AccountRef {
    /// The account itself.
    Account(Account),
    /// Its uuid (look it up in the answer's `accounts`).
    Uuid(String),
}

impl Default for AccountRef {
    fn default() -> Self {
        Self::Uuid(String::new())
    }
}

impl AccountRef {
    /// The account's uuid.
    pub fn uuid(&self) -> &str {
        match self {
            Self::Account(a) => &a.uuid,
            Self::Uuid(u) => u,
        }
    }

    /// The account, from itself or from `accounts`.
    pub fn resolve<'a>(&'a self, accounts: &'a BTreeMap<String, Account>) -> Option<&'a Account> {
        match self {
            Self::Account(a) => Some(a),
            Self::Uuid(u) => accounts.get(u),
        }
    }

    /// The `c:`/`si:` id to show (`an unknown account` when the answer doesn't describe it; accounts are never
    /// shown by uuid).
    pub fn label(&self, accounts: &BTreeMap<String, Account>) -> String {
        match self.resolve(accounts) {
            Some(a) => a.label().to_owned(),
            None if self.uuid().is_empty() => "nobody".to_owned(),
            None => "an unknown account".to_owned(),
        }
    }
}

// ── Values and styles ───────────────────────────────────────────────────────────────────────────────────────

/// What a cell holds. Errors are their Excel text (`#DIV/0!`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "lowercase")]
pub enum Value {
    /// Nothing.
    #[default]
    Empty,
    /// A number (dates are serial numbers).
    Number(f64),
    /// Text.
    Text(String),
    /// `TRUE` or `FALSE`.
    Bool(bool),
    /// An error value such as `#DIV/0!`.
    Error(String),
}

impl Value {
    /// True for [`Value::Empty`].
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// The value as plain JSON: a number, string, bool or null (errors as their text).
    pub fn to_json(&self) -> Json {
        match self {
            Self::Empty => Json::Null,
            Self::Number(n) => serde_json::Number::from_f64(*n)
                .map(Json::Number)
                .unwrap_or(Json::Null),
            Self::Text(s) | Self::Error(s) => Json::String(s.clone()),
            Self::Bool(b) => Json::Bool(*b),
        }
    }

    /// The kind as a word: `empty`, `number`, `text`, `bool`, `error`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Number(_) => "number",
            Self::Text(_) => "text",
            Self::Bool(_) => "bool",
            Self::Error(_) => "error",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => Ok(()),
            Self::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    write!(f, "{}", *n as i64)
                } else {
                    write!(f, "{n}")
                }
            }
            Self::Text(s) | Self::Error(s) => f.write_str(s),
            Self::Bool(true) => f.write_str("TRUE"),
            Self::Bool(false) => f.write_str("FALSE"),
        }
    }
}

/// Horizontal alignment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HAlign {
    /// Numbers right, text left.
    #[default]
    General,
    /// Left.
    Left,
    /// Centre.
    Center,
    /// Right.
    Right,
}

/// Vertical alignment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    /// Bottom.
    #[default]
    Bottom,
    /// Middle.
    Middle,
    /// Top.
    Top,
}

/// A border line's kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    /// No line.
    #[default]
    None,
    /// Thin.
    Thin,
    /// Medium.
    Medium,
    /// Thick.
    Thick,
    /// Dashed.
    Dashed,
    /// Dotted.
    Dotted,
    /// Double.
    Double,
}

/// One side's border.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Border {
    /// The line.
    #[serde(default)]
    pub style: BorderStyle,
    /// `#RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// How a cell looks (the engine's Style JSON). Absent fields are the defaults.
/// An in-cell control; entered options are literal text, never formulas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellControl {
    Checkbox {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checked: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unchecked: Option<String>,
    },
    Dropdown { options: Vec<String> },
    Person,
    File,
    Rating,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Style {
    /// Bold.
    #[serde(skip_serializing_if = "is_false")]
    pub bold: bool,
    /// Italic.
    #[serde(skip_serializing_if = "is_false")]
    pub italic: bool,
    /// Underlined.
    #[serde(skip_serializing_if = "is_false")]
    pub underline: bool,
    /// Struck through.
    #[serde(skip_serializing_if = "is_false")]
    pub strike: bool,
    /// Font size in points (default 11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u16>,
    /// Font family.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// Text colour `#RRGGBB`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Fill colour `#RRGGBB`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    /// Horizontal alignment.
    #[serde(skip_serializing_if = "is_default")]
    pub h_align: HAlign,
    /// Vertical alignment.
    #[serde(skip_serializing_if = "is_default")]
    pub v_align: VAlign,
    /// Wrap text.
    #[serde(skip_serializing_if = "is_false")]
    pub wrap: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub clip: bool,
    #[serde(skip_serializing_if = "is_default")]
    pub rotation: i16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control: Option<CellControl>,
    /// An Excel number format (`0.00`, `#,##0`, `yyyy-mm-dd`, `0%`); none is General.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_fmt: Option<String>,
    /// Top border.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_top: Option<Border>,
    /// Bottom border.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_bottom: Option<Border>,
    /// Left border.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_left: Option<Border>,
    /// Right border.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_right: Option<Border>,
}

impl Style {
    /// True when it is the default look.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// A short description: `bold, italic, color #FF0000, fill #FFF2CC, align center, format 0.00`.
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for (on, name) in [
            (self.bold, "bold"),
            (self.italic, "italic"),
            (self.underline, "underline"),
            (self.strike, "strike"),
        ] {
            if on {
                parts.push(name.into());
            }
        }
        if let Some(f) = &self.font {
            parts.push(format!("font {f}"));
        }
        if let Some(s) = self.font_size {
            parts.push(format!("size {s}"));
        }
        if let Some(c) = &self.color {
            parts.push(format!("color {c}"));
        }
        if let Some(c) = &self.fill {
            parts.push(format!("fill {c}"));
        }
        if self.h_align != HAlign::General {
            parts.push(format!(
                "align {}",
                serde_json::to_value(self.h_align)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ));
        }
        if self.v_align != VAlign::Bottom {
            parts.push(format!(
                "valign {}",
                serde_json::to_value(self.v_align)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
            ));
        }
        if self.wrap {
            parts.push("wrap".into());
        }
        if let Some(n) = &self.num_fmt {
            parts.push(format!("format {n}"));
        }
        let sides = [
            ("top", &self.border_top),
            ("bottom", &self.border_bottom),
            ("left", &self.border_left),
            ("right", &self.border_right),
        ];
        for (side, b) in sides {
            if let Some(b) = b
                && b.style != BorderStyle::None
            {
                let style = serde_json::to_value(b.style)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                parts.push(format!(
                    "border {side} {style}{}",
                    b.color
                        .as_deref()
                        .map(|c| format!(" {c}"))
                        .unwrap_or_default()
                ));
            }
        }
        parts.join(", ")
    }
}

/// `Some(Some(x))` sends `x`, `Some(None)` sends `null` (clears it), `None` leaves the field out.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// A partial change to a style (the engine's StylePatch JSON): each `Some` field replaces that part, `Some(None)`
/// clears it, and `clear` resets everything first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StylePatch {
    /// Bold on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    /// Italic on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    /// Underline on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    /// Strike on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strike: Option<bool>,
    /// Font size, or `Some(None)` for the default.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub font_size: Option<Option<u16>>,
    /// Font family, or `Some(None)` for the default.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub font: Option<Option<String>>,
    /// Text colour, or `Some(None)` for the default.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub color: Option<Option<String>>,
    /// Fill colour, or `Some(None)` for none.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub fill: Option<Option<String>>,
    /// Horizontal alignment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h_align: Option<HAlign>,
    /// Vertical alignment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v_align: Option<VAlign>,
    /// Wrap on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub control: Option<Option<CellControl>>,
    /// Number format, or `Some(None)` for General.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub num_fmt: Option<Option<String>>,
    /// Top border, or `Some(None)` for none.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_top: Option<Option<Border>>,
    /// Bottom border.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_bottom: Option<Option<Border>>,
    /// Left border.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_left: Option<Option<Border>>,
    /// Right border.
    #[serde(skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_right: Option<Option<Border>>,
    /// Reset every part first.
    #[serde(skip_serializing_if = "is_false")]
    pub clear: bool,
}

impl StylePatch {
    /// True when it changes nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

// ── Sheets, folders, sharing, transfers ─────────────────────────────────────────────────────────────────────

/// A caller's role on a sheet or folder.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads cells, versions and comments, downloads, compares.
    #[default]
    Viewer,
    /// Also comments, replies, reacts and resolves.
    Commenter,
    /// Also edits, runs code, makes and merges copies, restores versions, invites.
    Editor,
    /// Everything, including protections, the trash, transfers and deleting forever.
    Owner,
}

impl Role {
    /// The role's word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Commenter => "commenter",
            Self::Editor => "editor",
            Self::Owner => "owner",
        }
    }

    /// Reads a role's word (`editor`), case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "viewer" | "view" => Self::Viewer,
            "commenter" | "comment" => Self::Commenter,
            "editor" | "edit" => Self::Editor,
            "owner" => Self::Owner,
            _ => return None,
        })
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a public link lets people do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicAccess {
    /// Anyone with the link reads it.
    #[default]
    View,
    /// Signed-in accounts with the link comment.
    Comment,
    /// Signed-in accounts with the link edit.
    Edit,
}

impl PublicAccess {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::View => "view",
            Self::Comment => "comment",
            Self::Edit => "edit",
        }
    }

    /// Reads `view`, `comment` or `edit`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "view" | "viewer" => Self::View,
            "comment" | "commenter" => Self::Comment,
            "edit" | "editor" => Self::Edit,
            _ => return None,
        })
    }
}

/// A sheet's public link setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Public {
    /// Whether the link works.
    pub enabled: bool,
    /// What it lets people do.
    pub access: PublicAccess,
}

/// A spreadsheet.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sheet {
    /// 12 characters of `[a-z0-9]`.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Who it belongs to.
    pub owner: Account,
    /// The folder it is in (none: the owner's top level).
    pub folder_id: Option<String>,
    /// The caller's role.
    pub role: Role,
    /// The public link.
    pub public: Public,
    /// The main line's latest version (0 = new).
    pub version: i64,
    /// When it was made.
    pub created_at: String,
    /// Who made it.
    pub created_by: Account,
    /// When it last changed.
    pub updated_at: String,
    /// Who changed it last.
    pub updated_by: Account,
    /// When it went to the trash, if it is there.
    pub trashed_at: Option<String>,
    /// Its tabs (only on `GET /v1/sheets/{id}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<Tab>>,
    /// Its open copies.
    pub copies: i64,
    /// Its link in the web app, when the service gives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What made a code tab.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabCode {
    /// JavaScript for browser commands; older snapshots retain their original label.
    pub language: String,
    /// The code.
    pub code: String,
    /// The ids of the tabs it reads.
    pub inputs: Vec<u32>,
    /// Who ran it.
    pub run_by: Account,
    /// When it last ran.
    pub run_at: String,
}

/// One grid of cells.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tab {
    /// Stable id.
    pub id: u32,
    /// Name.
    pub name: String,
    /// Position among the tabs (zero-based).
    pub index: usize,
    /// Rows to show: max(used + 1, 1000).
    pub rows: u32,
    /// Columns to show: max(used + 1, 26).
    pub cols: u32,
    /// The area holding anything (`A1:F120`), none when empty.
    pub used: Option<String>,
    /// Frozen rows at the top.
    pub frozen_rows: u32,
    /// Frozen columns at the left.
    pub frozen_cols: u32,
    /// Tab colour `#RRGGBB`.
    pub color: Option<String>,
    /// What made it, when code did.
    pub code: Option<TabCode>,
    /// Conditional formatting and alternating-row rules, in priority order.
    #[serde(default)]
    pub format_rules: Vec<Json>,
}

/// `GET /v1/sheets`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SheetList {
    /// The sheets.
    pub sheets: Vec<Sheet>,
    /// The folders.
    pub folders: Vec<Folder>,
    /// Pass it as `cursor` for the next page.
    pub next_cursor: Option<String>,
}

/// Which sheets to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Your own top-level items together with items shared with you.
    All,
    /// Your own (the top level without a folder).
    #[default]
    Mine,
    /// Shared with you.
    Shared,
    /// Your trash.
    Trash,
}

impl Scope {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mine => "mine",
            Self::Shared => "shared",
            Self::Trash => "trash",
        }
    }
}

/// Parameters of `GET /v1/sheets`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSheets {
    /// `all`, `mine`, `shared` or `trash`.
    pub scope: Scope,
    /// Only this folder's contents.
    pub folder: Option<String>,
    /// Only titles containing this text.
    pub q: Option<String>,
    /// From a `next_cursor`.
    pub cursor: Option<String>,
    /// At most this many.
    pub limit: Option<u32>,
}

/// `POST /v1/sheets`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSheet {
    /// Title (default "Untitled sheet").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Folder to create it in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
}

/// Parameters of `POST /v1/sheets/import`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportSheet {
    /// Title (default from the file name).
    pub title: Option<String>,
    /// Folder to create it in.
    pub folder_id: Option<String>,
}

/// `PATCH /v1/sheets/{sheet}`. `folder_id: Some(None)` moves it to the top level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetPatch {
    /// New title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New folder (`Some(None)`: the top level).
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "double_option",
        default
    )]
    pub folder_id: Option<Option<String>>,
}

/// A place for sheets and other folders.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// The folder it is in.
    pub parent_id: Option<String>,
    /// Who it belongs to.
    pub owner: Account,
    /// The caller's role.
    pub role: Role,
    /// When it was made.
    pub created_at: String,
    /// Who made it (a folder made inside someone else's shared folder belongs to that folder's owner).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Account>,
    /// When it went to the trash.
    pub trashed_at: Option<String>,
    /// Its link in the web app, when the service gives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `GET /v1/folders/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderView {
    /// The folder.
    pub folder: Folder,
    /// The folders above it, top first.
    pub path: Vec<Folder>,
    /// Sheets in it.
    pub sheets: Vec<Sheet>,
    /// Folders in it.
    pub folders: Vec<Folder>,
}

/// `POST /v1/folders`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFolder {
    /// Name.
    pub name: String,
    /// The folder to make it in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// `PATCH /v1/folders/{id}`. `parent_id: Some(None)` moves it to the top level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderPatch {
    /// New name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New parent (`Some(None)`: the top level).
    #[serde(
        skip_serializing_if = "Option::is_none",
        with = "double_option",
        default
    )]
    pub parent_id: Option<Option<String>>,
}

/// A sheet or a folder, for the routes both have (sharing, transfers, the trash).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    /// A sheet id.
    Sheet(String),
    /// A folder id.
    Folder(String),
}

impl Item {
    /// The id.
    pub fn id(&self) -> &str {
        match self {
            Self::Sheet(id) | Self::Folder(id) => id,
        }
    }

    /// `sheet` or `folder`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sheet(_) => "sheet",
            Self::Folder(_) => "folder",
        }
    }

    pub(crate) fn segments(&self) -> [&str; 2] {
        match self {
            Self::Sheet(id) => ["sheets", id],
            Self::Folder(id) => ["folders", id],
        }
    }
}

/// How a member has their role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// Invited to the item itself.
    Direct,
    /// Through a folder it is in.
    Folder(Folder),
}

impl Default for Via {
    fn default() -> Self {
        Self::Direct
    }
}

impl Serialize for Via {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Direct => s.serialize_str("direct"),
            Self::Folder(f) => {
                use serde::ser::SerializeMap;
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("folder", f)?;
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Via {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Json::deserialize(d)?;
        match v {
            Json::Object(mut m) => match m.remove("folder") {
                // A folder the answer couldn't describe is still a folder.
                Some(Json::Null) => Ok(Self::Folder(Folder::default())),
                Some(f) => serde_json::from_value(f)
                    .map(Self::Folder)
                    .map_err(serde::de::Error::custom),
                None => Ok(Self::Direct),
            },
            _ => Ok(Self::Direct),
        }
    }
}

/// Someone a sheet or folder is shared with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Member {
    /// Who.
    pub account: Account,
    /// Their role.
    pub role: Role,
    /// Directly or through a folder.
    pub via: Via,
    /// The role they were given on the item itself, when they were (a folder may give a higher one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_role: Option<Role>,
    /// Who gave them their role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<Account>,
    /// Since when.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// An invite to an email address that has no account yet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Invite {
    /// The address.
    pub email: String,
    /// The role it will get.
    pub role: Role,
    /// Who invited.
    pub invited_by: Account,
    /// When.
    pub created_at: String,
}

/// `GET {item}/sharing`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharing {
    /// The owner.
    pub owner: Account,
    /// The caller's own role on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Everyone else with a role.
    pub members: Vec<Member>,
    /// Invites waiting for an account.
    pub invites: Vec<Invite>,
    /// The public link.
    pub public: Public,
}

/// `POST {item}/sharing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareRequest {
    /// `c:` ids, `si:` ids and email addresses.
    pub invite: Vec<String>,
    /// The role they get.
    pub role: Role,
    /// Whether they are told (through Silicon Ting).
    pub notify: bool,
}

/// `PUT {sheet}/public`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicSetting {
    /// On or off.
    pub enabled: bool,
    /// What it lets people do.
    pub access: PublicAccess,
}

/// The answer of `PUT {sheet}/public`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicLink {
    /// On or off.
    pub enabled: bool,
    /// What it lets people do.
    pub access: PublicAccess,
    /// The link.
    pub url: String,
}

/// Where a transfer stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferStatus {
    /// Waiting for the recipient.
    #[default]
    Pending,
    /// Accepted: the recipient owns it.
    Accepted,
    /// Declined by the recipient.
    Declined,
    /// Cancelled by the sender.
    Cancelled,
}

impl TransferStatus {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A change of owner, waiting for the recipient.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transfer {
    /// Its id.
    pub id: String,
    /// `sheet` or `folder`.
    pub kind: String,
    /// The sheet's or folder's id.
    pub item_id: String,
    /// Its title or name.
    pub title: String,
    /// The current owner.
    pub from: Account,
    /// The new owner.
    pub to: Account,
    /// Where it stands.
    pub status: TransferStatus,
    /// When it was offered.
    pub created_at: String,
    /// When it was accepted, declined or cancelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
}

/// `GET /v1/transfers`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transfers {
    /// Offered to you.
    pub incoming: Vec<Transfer>,
    /// Offered by you.
    pub outgoing: Vec<Transfer>,
}

/// What to do with a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAction {
    /// Take ownership (the recipient).
    Accept,
    /// Refuse it (the recipient).
    Decline,
    /// Withdraw it (the sender).
    Cancel,
}

impl TransferAction {
    /// The route's last segment.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
        }
    }
}

// ── Lines: tabs and cells ───────────────────────────────────────────────────────────────────────────────────

/// A tab named by id or by name, as requests take it. Serializes as a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TabRef {
    /// The tab's id.
    Id(u32),
    /// The tab's name.
    Name(String),
}

impl TabRef {
    /// How it goes into a path.
    pub fn path(&self) -> String {
        match self {
            Self::Id(id) => id.to_string(),
            Self::Name(n) => n.clone(),
        }
    }
}

impl From<u32> for TabRef {
    fn from(id: u32) -> Self {
        Self::Id(id)
    }
}

impl From<&str> for TabRef {
    fn from(name: &str) -> Self {
        Self::Name(name.to_owned())
    }
}

impl From<String> for TabRef {
    fn from(name: String) -> Self {
        Self::Name(name)
    }
}

impl From<&Tab> for TabRef {
    fn from(t: &Tab) -> Self {
        Self::Id(t.id)
    }
}

impl fmt::Display for TabRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(id) => write!(f, "{id}"),
            Self::Name(n) => f.write_str(n),
        }
    }
}

impl Serialize for TabRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Id(id) => s.serialize_u32(*id),
            Self::Name(n) => s.serialize_str(n),
        }
    }
}

impl<'de> Deserialize<'de> for TabRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Json::deserialize(d)? {
            Json::Number(n) => n
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .map(Self::Id)
                .ok_or_else(|| serde::de::Error::custom("a tab id is a whole number")),
            Json::String(s) => Ok(Self::Name(s)),
            other => Err(serde::de::Error::custom(format!(
                "a tab is an id or a name, not {other}"
            ))),
        }
    }
}

/// `GET {line}/tabs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabList {
    /// The line's version.
    pub version: i64,
    /// Its tabs, in order.
    pub tabs: Vec<Tab>,
    /// Defined names, when the service lists them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<NameDef>>,
}

/// One non-empty cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cell {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cs: Option<Style>,
    /// Its address (`B2`).
    pub a: String,
    /// Zero-based row.
    pub r: u32,
    /// Zero-based column.
    pub c: u32,
    /// What it holds (a formula's result).
    pub v: Value,
    /// What it shows, formatted.
    pub t: String,
    /// Its formula, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
    /// Its style id, when not the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<u32>,
    /// True when a spilling formula elsewhere wrote it.
    #[serde(skip_serializing_if = "is_false")]
    pub spill: bool,
    /// The uuid of who last changed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// When.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// A chart drawn from a range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chart {
    /// Its id.
    pub id: String,
    /// The tab it sits on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabRef>,
    /// `line`, `bar`, `column`, `area`, `pie`, `scatter`.
    pub kind: String,
    /// Title.
    pub title: String,
    /// The data (`A1:C20` or `Sales!A1:C20`).
    pub range: String,
    /// The top-left cell it sits at (`E2`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Or the anchor by position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_row: Option<u32>,
    /// Or the anchor by position.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_col: Option<u32>,
    /// Width in pixels.
    pub width: f64,
    /// Height in pixels.
    pub height: f64,
}

impl Chart {
    /// The anchor as an address.
    pub fn anchor_a1(&self) -> String {
        match (&self.anchor, self.anchor_row, self.anchor_col) {
            (Some(a), _, _) => a.clone(),
            (None, Some(r), Some(c)) => crate::a1::CellRef::new(r, c).to_string(),
            _ => String::new(),
        }
    }
}

/// A tab or range only chosen accounts may change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Protection {
    /// Its id.
    pub id: String,
    /// The tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabRef>,
    /// The range (none: the whole tab).
    pub range: Option<String>,
    /// Who may change it besides the owner.
    pub editors: Vec<AccountRef>,
    /// What it is for.
    pub description: String,
}

/// `GET {line}/tabs/{tab}/cells`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cells {
    /// The version read.
    pub version: i64,
    /// The tab.
    pub tab: Tab,
    /// The range read (none when the tab is empty).
    pub range: Option<String>,
    /// The non-empty cells, row by row.
    pub cells: Vec<Cell>,
    /// Every style the cells use, by id.
    pub styles: BTreeMap<String, Style>,
    /// Every account in `by`, by uuid.
    pub accounts: BTreeMap<String, Account>,
    /// Non-default column widths, by column letters.
    pub col_widths: BTreeMap<String, f64>,
    /// Non-default row heights, by 1-based row number.
    pub row_heights: BTreeMap<String, f64>,
    /// Hidden rows (1-based).
    pub hidden_rows: Vec<u32>,
    /// Hidden columns (letters).
    pub hidden_cols: Vec<String>,
    /// Merged areas.
    pub merges: Vec<String>,
    /// Frozen rows.
    pub frozen_rows: u32,
    /// Frozen columns.
    pub frozen_cols: u32,
    /// Charts on the tab.
    pub charts: Vec<Chart>,
    /// Protected areas on the tab.
    pub protections: Vec<Protection>,
}

impl Cells {
    /// The style of a cell (the default when it has none).
    pub fn style_of(&self, cell: &Cell) -> Option<&Style> {
        cell.s.and_then(|s| self.styles.get(&s.to_string()))
    }

    /// Who last changed a cell.
    pub fn author_of(&self, cell: &Cell) -> Option<&Account> {
        cell.by.as_ref().and_then(|u| self.accounts.get(u))
    }
}

/// `GET {line}/tabs/{tab}/cells?format=values`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellValues {
    /// The version read.
    pub version: i64,
    /// The range read.
    pub range: Option<String>,
    /// Row by row: numbers, strings, bools, nulls (errors as their text, formulas as results).
    pub values: Vec<Vec<Json>>,
}

/// What a cell held before or after a change: a value, a formula, or both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    /// A value.
    Value(Value),
    /// A formula (`=A1*2`).
    Formula(String),
    /// A value, formula and look together.
    Summary(CellSummary),
    /// Anything else the service sent.
    Raw(Json),
}

impl Default for Content {
    fn default() -> Self {
        Self::Value(Value::Empty)
    }
}

impl fmt::Display for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(Value::Empty) => f.write_str("(empty)"),
            Self::Value(v) => write!(f, "{v}"),
            Self::Formula(s) => f.write_str(s),
            Self::Summary(s) => write!(f, "{s}"),
            Self::Raw(Json::Null) => f.write_str("(empty)"),
            Self::Raw(Json::String(s)) => f.write_str(s),
            Self::Raw(v) => write!(f, "{v}"),
        }
    }
}

/// One change in a cell's history.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellChange {
    /// The version it was made in.
    pub version: i64,
    /// Who.
    pub by: AccountRef,
    /// When.
    pub at: String,
    /// What it held before.
    pub before: Content,
    /// What it held after.
    pub after: Content,
    /// The change it was part of, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// True when it was part of a change to many cells at once (a sort, an import, a paste).
    #[serde(skip_serializing_if = "is_false")]
    pub bulk: bool,
}

/// `GET {line}/tabs/{tab}/cells/{a1}`: one cell with everything known about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellDetail {
    /// The version read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    /// The tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<Tab>,
    /// Its address.
    pub a: String,
    /// Zero-based row.
    pub r: u32,
    /// Zero-based column.
    pub c: u32,
    /// What it holds.
    pub v: Value,
    /// What it shows.
    pub t: String,
    /// Its formula.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub f: Option<String>,
    /// Its style id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub s: Option<u32>,
    /// Its style, when the service gives it inline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<Style>,
    /// True when a spilling formula elsewhere wrote it.
    #[serde(skip_serializing_if = "is_false")]
    pub spill: bool,
    /// Who last changed it (a uuid, or the account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<AccountRef>,
    /// When.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// Styles by id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub styles: BTreeMap<String, Style>,
    /// Accounts by uuid.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, Account>,
    /// The last 20 changes, newest first.
    pub history: Vec<CellChange>,
}

impl CellDetail {
    /// Its style, inline or by id.
    pub fn style_resolved(&self) -> Option<&Style> {
        self.style
            .as_ref()
            .or_else(|| self.s.and_then(|s| self.styles.get(&s.to_string())))
    }

    /// Who last changed it.
    pub fn author(&self) -> Option<&Account> {
        self.by.as_ref().and_then(|b| b.resolve(&self.accounts))
    }
}

// ── Editing ─────────────────────────────────────────────────────────────────────────────────────────────────

/// One cell's input, typed as a person would (`=A1*2`, `12%`, `2026-10-17`, `hello`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellInput {
    /// The address.
    pub a: String,
    /// What to type.
    pub input: String,
}

/// What `clear` removes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClearWhat {
    /// Values and formulas, keeping the look.
    #[default]
    Contents,
    /// The look, keeping values.
    Formats,
    /// Both.
    All,
}

/// One sort key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    /// The column's letters.
    pub col: String,
    /// Ascending.
    pub asc: bool,
}

/// A defined name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NameDef {
    /// The name (`Revenue`).
    pub name: String,
    /// What it refers to (`Sales!$B$2:$B$100`).
    #[serde(rename = "ref")]
    pub reference: String,
}

/// A chart to add or replace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSpec {
    /// The id of a chart to replace; none adds one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tab it sits on.
    pub tab: TabRef,
    /// `line`, `bar`, `column`, `area`, `pie` or `scatter`.
    pub kind: String,
    /// Title.
    pub title: String,
    /// The data range.
    pub range: String,
    /// The cell it sits at.
    pub anchor: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// A protection to add or replace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionSpec {
    /// The id of a protection to replace; none adds one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The tab.
    pub tab: TabRef,
    /// The range; none protects the whole tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// `c:`/`si:` ids who may still change it.
    pub editors: Vec<String>,
    /// What it is for.
    pub description: String,
}

/// One edit. A request's edits apply in order as one version, all or none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Edit {
    /// Adds or updates a conditional-formatting or alternating-row rule.
    FormatRule {
        /// The tab.
        tab: TabRef,
        /// Stable rule identifier; omit to create one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// Bounded A1 range.
        range: String,
        /// Condition, colour scale or alternating-row configuration.
        rule: Json,
    },
    /// Removes a rule without changing the cells' underlying styles.
    DeleteFormatRule {
        /// The tab.
        tab: TabRef,
        /// The rule identifier.
        id: String,
    },
    /// Types inputs into cells: either `cells` by address, or a block of `rows` starting `at`.
    Set {
        /// The tab.
        tab: TabRef,
        /// Inputs by address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cells: Option<Vec<CellInput>>,
        /// The block's top-left cell.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        /// The block, row by row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rows: Option<Vec<Vec<String>>>,
    },
    /// Writes raw values, without reading them as inputs.
    Values {
        /// The tab.
        tab: TabRef,
        /// The block's top-left cell.
        at: String,
        /// Row by row: JSON numbers, strings, bools, nulls, or Value objects.
        rows: Vec<Vec<Json>>,
    },
    /// Changes the look of a range.
    Style {
        /// The tab.
        tab: TabRef,
        /// The range.
        range: String,
        /// What to change.
        patch: StylePatch,
    },
    /// Clears a range.
    Clear {
        /// The tab.
        tab: TabRef,
        /// The range.
        range: String,
        /// What to clear.
        what: ClearWhat,
    },
    /// Inserts rows before zero-based position `at`.
    InsertRows {
        /// The tab.
        tab: TabRef,
        /// Zero-based position.
        at: u32,
        /// How many.
        count: u32,
    },
    /// Inserts columns before zero-based position `at`.
    InsertCols {
        /// The tab.
        tab: TabRef,
        /// Zero-based position.
        at: u32,
        /// How many.
        count: u32,
    },
    /// Deletes rows from zero-based position `at`.
    DeleteRows {
        /// The tab.
        tab: TabRef,
        /// Zero-based position.
        at: u32,
        /// How many.
        count: u32,
    },
    /// Deletes columns from zero-based position `at`.
    DeleteCols {
        /// The tab.
        tab: TabRef,
        /// Zero-based position.
        at: u32,
        /// How many.
        count: u32,
    },
    /// Moves rows.
    MoveRows {
        /// The tab.
        tab: TabRef,
        /// Zero-based position of the first row to move.
        from: u32,
        /// How many.
        count: u32,
        /// Zero-based position to move them to.
        to: u32,
    },
    /// Moves columns.
    MoveCols {
        /// The tab.
        tab: TabRef,
        /// Zero-based position of the first column to move.
        from: u32,
        /// How many.
        count: u32,
        /// Zero-based position to move them to.
        to: u32,
    },
    /// Sorts a range's rows.
    Sort {
        /// The tab.
        tab: TabRef,
        /// The range.
        range: String,
        /// The keys, first wins.
        by: Vec<SortKey>,
        /// True when the first row is a header that stays.
        header: bool,
    },
    /// Fills a range from a cell, as dragging its corner would.
    Fill {
        /// The tab.
        tab: TabRef,
        /// The source cell or range.
        from: String,
        /// The range to fill.
        to: String,
    },
    /// Adds a tab.
    AddTab {
        /// Its name.
        name: String,
        /// Where (zero-based); none puts it last.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Renames a tab.
    RenameTab {
        /// The tab.
        tab: TabRef,
        /// The new name.
        name: String,
    },
    /// Moves a tab.
    MoveTab {
        /// The tab.
        tab: TabRef,
        /// Where (zero-based).
        index: usize,
    },
    /// Deletes a tab.
    DeleteTab {
        /// The tab.
        tab: TabRef,
    },
    /// Changes frozen panes or the tab's colour.
    TabProps {
        /// The tab.
        tab: TabRef,
        /// Frozen rows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frozen_rows: Option<u32>,
        /// Frozen columns.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frozen_cols: Option<u32>,
        /// Colour, or `Some(None)` to remove it.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        color: Option<Option<String>>,
    },
    /// Sets column widths (`None`: the default width).
    ColWidth {
        /// The tab.
        tab: TabRef,
        /// The columns (`A:C`).
        cols: String,
        /// Pixels.
        width: Option<f64>,
    },
    /// Sets row heights (`None`: the default height).
    RowHeight {
        /// The tab.
        tab: TabRef,
        /// The rows (`2:4`).
        rows: String,
        /// Pixels.
        height: Option<f64>,
    },
    /// Hides rows.
    HideRows {
        /// The tab.
        tab: TabRef,
        /// The rows (`3:5`).
        range: String,
    },
    /// Shows hidden rows.
    ShowRows {
        /// The tab.
        tab: TabRef,
        /// The rows.
        range: String,
    },
    /// Hides columns.
    HideCols {
        /// The tab.
        tab: TabRef,
        /// The columns (`C:D`).
        range: String,
    },
    /// Shows hidden columns.
    ShowCols {
        /// The tab.
        tab: TabRef,
        /// The columns.
        range: String,
    },
    /// Merges a range into one cell.
    Merge {
        /// The tab.
        tab: TabRef,
        /// The range.
        range: String,
    },
    /// Splits merged cells.
    Unmerge {
        /// The tab.
        tab: TabRef,
        /// The range.
        range: String,
    },
    /// Replaces every defined name.
    Names {
        /// The names.
        names: Vec<NameDef>,
    },
    /// Adds or replaces a chart.
    Chart {
        /// The chart.
        chart: ChartSpec,
    },
    /// Deletes a chart.
    DeleteChart {
        /// Its id.
        id: String,
    },
    /// Protects a tab or range (owner only).
    Protect {
        /// The protection.
        protection: ProtectionSpec,
    },
    /// Removes a protection (owner only).
    Unprotect {
        /// Its id.
        id: String,
    },
    /// Undoes the caller's own last change on this line (alone in a request).
    Undo,
    /// Redoes the caller's last undone change (alone in a request).
    Redo,
}

impl Edit {
    /// Inputs into cells by address.
    pub fn set_cells(tab: impl Into<TabRef>, cells: Vec<CellInput>) -> Self {
        Self::Set {
            tab: tab.into(),
            cells: Some(cells),
            at: None,
            rows: None,
        }
    }

    /// A block of inputs starting at `at`.
    pub fn set_block(
        tab: impl Into<TabRef>,
        at: impl Into<String>,
        rows: Vec<Vec<String>>,
    ) -> Self {
        Self::Set {
            tab: tab.into(),
            cells: None,
            at: Some(at.into()),
            rows: Some(rows),
        }
    }
}

/// `POST {line}/edits`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EditRequest {
    /// The version the caller was looking at (overwrites of later changes are reported).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_version: Option<i64>,
    /// The edits, applied in order as one version.
    pub edits: Vec<Edit>,
    /// The caller's connection, so its own live events can be told apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// What changed, for viewers to update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Change {
    /// The tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<u32>,
    /// The range that changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The whole tab has to be reloaded.
    #[serde(skip_serializing_if = "is_false")]
    pub reload: bool,
    /// The list of tabs changed.
    #[serde(skip_serializing_if = "is_false")]
    pub tabs: bool,
    /// Names, charts, protections or settings changed.
    #[serde(skip_serializing_if = "is_false")]
    pub meta: bool,
}

/// Another account's change that this edit replaced since `base_version`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overwrite {
    /// The tab.
    pub tab: u32,
    /// The cell.
    pub a: String,
    /// Who had changed it.
    pub by: Account,
    /// When.
    pub at: String,
    /// What it held (kept in history).
    pub before: Value,
    /// The version of their change.
    pub version: i64,
}

/// The answer of `POST {line}/edits`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditAnswer {
    /// The new version.
    pub version: i64,
    /// What changed.
    pub changes: Vec<Change>,
    /// Other accounts' changes replaced.
    pub overwrote: Vec<Overwrite>,
    /// Cell writes skipped because their row or column no longer exists.
    pub skipped: u64,
}

impl EditAnswer {
    /// The id of a tab the edit added (`add_tab`), when it says.
    pub fn added_tab(&self) -> Option<u32> {
        self.changes
            .iter()
            .find(|c| c.tabs && c.tab.is_some())
            .and_then(|c| c.tab)
    }
}

// ── Live ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Someone viewing a line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Viewer {
    /// Who.
    pub account: Account,
    /// The tab they are on.
    pub tab: Option<u32>,
    /// Their selection.
    pub range: Option<String>,
    /// Their connection.
    pub client_id: Option<String>,
    /// When they were last seen.
    pub seen_at: String,
}

/// `POST {line}/presence`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PresenceUpdate {
    /// The tab.
    pub tab: TabRef,
    /// The selection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
    /// The connection.
    pub client_id: String,
}

/// A `change` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChangeEvent {
    /// The new version.
    pub version: i64,
    /// Who changed it.
    pub by: Account,
    /// When.
    pub at: String,
    /// What changed.
    pub changes: Vec<Change>,
    /// The changed cells' new contents (up to 2,000; none means reload the ranges).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<Cell>>,
    /// Accounts by uuid, when the cells name any.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, Account>,
    /// The connection that made it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

/// A `presence` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceEvent {
    /// Everyone viewing now.
    pub viewers: Vec<Viewer>,
}

/// A `comment` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommentEvent {
    /// The thread as it is now.
    pub thread: Thread,
}

/// A `copy` event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyEvent {
    /// The copy as it is now.
    pub copy: SheetCopy,
}

// ── Code ────────────────────────────────────────────────────────────────────────────────────────────────────

/// Where a run's result goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandInto {
    /// A new tab made by this code.
    NewTab {
        /// Its name.
        new_tab: String,
    },
    /// Cells of an existing tab, from `at`.
    Cells {
        /// The tab.
        tab: TabRef,
        /// The top-left cell.
        at: String,
        /// Replace the whole command-output tab when rerunning it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        replace: bool,
    },
}

/// Browser-computed output to validate and store; the server never executes `code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResultRequest {
    /// JavaScript source retained with the output.
    pub code: String,
    /// Exact input version; stale results are rejected.
    pub base_version: i64,
    /// Input tab identities.
    pub inputs: Vec<u32>,
    /// Output column names.
    pub columns: Vec<String>,
    /// Rectangular scalar JSON values computed in the browser.
    pub rows: Vec<Vec<Json>>,
    /// Destination for the result.
    pub into: CommandInto,
}

/// What a run gave.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CommandResultAnswer {
    /// Column names.
    pub columns: Vec<String>,
    /// Up to 1,000 rows.
    pub rows: Vec<Vec<Json>>,
    /// How many rows there were in all.
    pub row_count: u64,
    /// True when `rows` holds fewer than `row_count`.
    pub truncated: bool,
    /// What the code printed.
    pub stdout: String,
    /// What it printed as errors.
    pub stderr: String,
    /// How long it ran.
    pub duration_ms: u64,
    /// The version the written result made, when it was written.
    pub version: Option<i64>,
    /// The tab written, when it was.
    pub tab: Option<Tab>,
}

// ── Files ───────────────────────────────────────────────────────────────────────────────────────────────────

/// A file format Silicon Calc reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// Excel.
    Xlsx,
    /// Excel 97–2003.
    Xls,
    /// OpenDocument.
    Ods,
    /// Apple Numbers.
    Numbers,
    /// Comma-separated values (one tab).
    Csv,
    /// Tab-separated values (one tab).
    Tsv,
    /// `{"tabs": [{"name", "rows"}]}`.
    Json,
    /// Apache Parquet (one tab).
    Parquet,
    /// PDF (download only).
    Pdf,
    /// Self-contained web page (download only).
    Html,
}

impl Format {
    /// Every format.
    pub const ALL: [Format; 10] = [
        Self::Xlsx,
        Self::Xls,
        Self::Ods,
        Self::Numbers,
        Self::Csv,
        Self::Tsv,
        Self::Json,
        Self::Parquet,
        Self::Pdf,
        Self::Html,
    ];

    /// The extension without the dot.
    pub fn ext(self) -> &'static str {
        match self {
            Self::Xlsx => "xlsx",
            Self::Xls => "xls",
            Self::Ods => "ods",
            Self::Numbers => "numbers",
            Self::Csv => "csv",
            Self::Tsv => "tsv",
            Self::Json => "json",
            Self::Parquet => "parquet",
            Self::Pdf => "pdf",
            Self::Html => "html",
        }
    }

    /// Reads a format's name or extension (`xlsx`, `.csv`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('.').to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.ext() == s)
    }

    /// The format a file name's extension names.
    pub fn from_file_name(name: &str) -> Option<Self> {
        name.rsplit_once('.').and_then(|(_, ext)| Self::parse(ext))
    }

    /// True for formats that can be opened (all but PDF).
    pub fn can_import(self) -> bool {
        !matches!(self, Self::Pdf | Self::Html)
    }

    /// True for formats that hold one tab.
    pub fn single_tab(self) -> bool {
        matches!(self, Self::Csv | Self::Tsv | Self::Parquet)
    }

    /// The media type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Xls => "application/vnd.ms-excel",
            Self::Ods => "application/vnd.oasis.opendocument.spreadsheet",
            Self::Numbers => "application/vnd.apple.numbers",
            Self::Csv => "text/csv",
            Self::Tsv => "text/tab-separated-values",
            Self::Json => "application/json",
            Self::Parquet => "application/vnd.apache.parquet",
            Self::Pdf => "application/pdf",
            Self::Html => "text/html",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.ext())
    }
}

/// How an imported file joins a sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportMode {
    /// Every tab of the file becomes a new tab.
    NewTab,
    /// The file's first tab replaces this tab's contents.
    Replace(TabRef),
}

/// The answer of `POST {line}/tabs/import`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportedTabs {
    /// The new version.
    pub version: i64,
    /// The tabs made or replaced.
    pub tabs: Vec<Tab>,
}

/// Parameters of `GET {line}/download`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadQuery {
    /// The format.
    pub format: Format,
    /// The tab, for formats holding one (default the first).
    pub tab: Option<TabRef>,
    /// A past version.
    pub version: Option<i64>,
    /// JSON only: cells as `{"v", "f"}` with their formulas.
    pub formulas: bool,
}

impl DownloadQuery {
    /// The latest version in `format`.
    pub fn new(format: Format) -> Self {
        Self {
            format,
            tab: None,
            version: None,
            formulas: false,
        }
    }
}

// ── Versions ────────────────────────────────────────────────────────────────────────────────────────────────

/// What kind of change a version is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionKind {
    /// Edits.
    #[default]
    Edit,
    /// A file opened into it.
    Import,
    /// Code results.
    Code,
    /// A copy merged in.
    Merge,
    /// A version restored.
    Restore,
    /// An account's changes undone.
    Undo,
    /// The sheet being made.
    Create,
    /// Anything newer than this client knows.
    #[serde(other)]
    Other,
}

impl VersionKind {
    /// The word.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Import => "import",
            Self::Code => "code",
            Self::Merge => "merge",
            Self::Restore => "restore",
            Self::Undo => "undo",
            Self::Create => "create",
            Self::Other => "other",
        }
    }
}

/// A run of changes by one account close together, or a named version.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Version {
    /// Its last operation's number.
    pub version: i64,
    /// Its first operation's number.
    pub from_version: i64,
    /// Who.
    pub by: Account,
    /// When it started.
    pub started_at: String,
    /// When it ended.
    pub ended_at: String,
    /// What it did (`si:analyst changed 340 cells in Sales`).
    pub summary: String,
    /// Its name, when it has one.
    pub name: Option<String>,
    /// What kind of change.
    pub kind: VersionKind,
}

/// `GET {line}/versions`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionList {
    /// Newest first.
    pub versions: Vec<Version>,
    /// For the next page.
    pub next_cursor: Option<String>,
}

/// A cell as a person sees it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellSummary {
    /// Its value (empty for formulas).
    #[serde(skip_serializing_if = "Value::is_empty")]
    pub value: Value,
    /// Its formula.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// Its look.
    #[serde(skip_serializing_if = "Style::is_default")]
    pub style: Style,
}

impl CellSummary {
    /// True when it holds nothing and looks default.
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none() && self.style.is_default()
    }
}

impl fmt::Display for CellSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let content = match (&self.formula, &self.value) {
            (Some(formula), _) => formula.clone(),
            (None, Value::Empty) => "(empty)".to_owned(),
            (None, v) => v.to_string(),
        };
        if self.style.is_default() {
            f.write_str(&content)
        } else {
            write!(f, "{content} [{}]", self.style.describe())
        }
    }
}

/// How one cell differs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CellDiff {
    /// The tab's id.
    pub tab: u32,
    /// The row's stable id.
    pub row: u32,
    /// The column's stable id.
    pub col: u32,
    /// Where it is now (or was, when removed).
    pub address: String,
    /// Before.
    pub before: CellSummary,
    /// After.
    pub after: CellSummary,
    /// Who changed it last between the two (a uuid or the account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<AccountRef>,
}

/// How one tab differs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabDiff {
    /// The tab's id.
    pub tab: u32,
    /// Its name.
    pub name: String,
    /// Its old name, when renamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
    /// Added.
    #[serde(skip_serializing_if = "is_false")]
    pub added: bool,
    /// Removed.
    #[serde(skip_serializing_if = "is_false")]
    pub removed: bool,
    /// Rows added.
    pub rows_added: u32,
    /// Rows removed.
    pub rows_removed: u32,
    /// Columns added.
    pub cols_added: u32,
    /// Columns removed.
    pub cols_removed: u32,
    /// Moved among the tabs.
    #[serde(skip_serializing_if = "is_false")]
    pub reordered: bool,
    /// Cells changed.
    pub cells_changed: u64,
}

/// Two states of a sheet compared, cell by cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkbookDiff {
    /// Tab by tab.
    pub tabs: Vec<TabDiff>,
    /// Changed cells, up to the limit.
    pub cells: Vec<CellDiff>,
    /// How many cells changed in all.
    pub cells_changed: u64,
    /// True when `cells` holds fewer than `cells_changed`.
    pub truncated: bool,
    /// Defined names changed.
    #[serde(skip_serializing_if = "is_false")]
    pub names_changed: bool,
    /// Charts changed.
    #[serde(skip_serializing_if = "is_false")]
    pub charts_changed: bool,
    /// Accounts by uuid.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub accounts: BTreeMap<String, Account>,
}

impl WorkbookDiff {
    /// True when nothing differs.
    pub fn is_empty(&self) -> bool {
        self.cells_changed == 0
            && !self.names_changed
            && !self.charts_changed
            && self.tabs.iter().all(|t| {
                !t.added
                    && !t.removed
                    && t.renamed_from.is_none()
                    && t.rows_added == 0
                    && t.rows_removed == 0
                    && t.cols_added == 0
                    && t.cols_removed == 0
                    && !t.reordered
            })
    }

    /// A tab's name by id.
    pub fn tab_name(&self, id: u32) -> Option<&str> {
        self.tabs
            .iter()
            .find(|t| t.tab == id)
            .map(|t| t.name.as_str())
    }
}

/// Parameters of `GET {line}/compare`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompareQuery {
    /// The older version.
    pub from: i64,
    /// The newer version; none is the latest.
    pub to: Option<i64>,
    /// At most this many cells (default 500).
    pub limit: Option<u32>,
    /// Only this tab.
    pub tab: Option<TabRef>,
    /// Only this range (with `tab`).
    pub range: Option<String>,
}

/// `POST {line}/versions/{v}/restore`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RestoreRequest {
    /// Only this tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<TabRef>,
    /// Only this range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,
}

/// An answer that only says the new version.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionAnswer {
    /// The new version.
    pub version: i64,
}

/// The answer of `POST {line}/versions/{v}/undo-account`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UndoAccountAnswer {
    /// The new version.
    pub version: i64,
    /// Cells put back.
    pub cells_changed: u64,
}

// ── Copies ──────────────────────────────────────────────────────────────────────────────────────────────────

/// Where a copy stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CopyStatus {
    /// In use.
    #[default]
    Open,
    /// Merged back (and closed).
    Merged,
    /// Deleted; its history stays readable.
    Deleted,
}

impl CopyStatus {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Deleted => "deleted",
        }
    }
}

/// A copy of a sheet kept inside it (a line other than the main one).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SheetCopy {
    /// Its id.
    pub id: String,
    /// The sheet.
    pub sheet_id: String,
    /// Its name.
    pub name: String,
    /// The copy it was made from; none is the main sheet.
    pub parent_id: Option<String>,
    /// The parent's version it was made at.
    pub forked_at: i64,
    /// Its own latest version.
    pub version: i64,
    /// Who made it.
    pub created_by: Account,
    /// When.
    pub created_at: String,
    /// Where it stands.
    pub status: CopyStatus,
    /// When it was merged.
    pub merged_at: Option<String>,
    /// Who merged it.
    pub merged_by: Option<Account>,
}

/// `GET /v1/sheets/{sheet}/copies`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyList {
    /// The copies (the tree is given by `parent_id`).
    pub copies: Vec<SheetCopy>,
}

/// Which side a merge takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// What the copy has.
    #[default]
    Copy,
    /// What the sheet (the copy's parent) has.
    Sheet,
}

impl Side {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Sheet => "sheet",
        }
    }

    /// Reads `copy` or `sheet`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "copy" | "theirs" => Some(Self::Copy),
            "sheet" | "main" | "ours" => Some(Self::Sheet),
            _ => None,
        }
    }
}

/// A cell changed differently in the copy and in the sheet since the copy was made.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Conflict {
    /// The tab (by id).
    pub tab: u32,
    /// Its name.
    pub tab_name: String,
    /// The cell.
    pub a: String,
    /// What it held when the copy was made.
    pub base: CellSummary,
    /// What the sheet has.
    pub sheet: CellSummary,
    /// What the copy has.
    pub copy: CellSummary,
    /// The side taken (or that would be).
    pub taken: Side,
}

/// `GET /v1/sheets/{sheet}/copies/{copy}/compare`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CopyCompare {
    /// The parent at the fork compared with the copy now.
    pub diff: WorkbookDiff,
    /// Cells changed on both sides differently.
    pub conflicts: Vec<Conflict>,
    /// True when the parent changed since the copy was made.
    pub parent_changed: bool,
}

/// Merge only one range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeRange {
    /// The tab.
    pub tab: TabRef,
    /// The range.
    pub range: String,
}

/// Which side one conflicting cell takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    /// The tab.
    pub tab: TabRef,
    /// The cell.
    pub a: String,
    /// The side.
    pub take: Side,
}

/// `POST /v1/sheets/{sheet}/copies/{copy}/merge`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MergeRequest {
    /// Only these tabs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<Vec<TabRef>>,
    /// Only this range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<MergeRange>,
    /// Sides for single conflicting cells.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolutions: Vec<Resolution>,
    /// The side every other conflict takes (default `copy`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Side>,
    /// Only say what would happen.
    #[serde(skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// Close the copy after merging.
    #[serde(skip_serializing_if = "is_false")]
    pub close: bool,
}

/// Tab names, or a count of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NamesOrCount {
    /// The names.
    Names(Vec<String>),
    /// How many.
    Count(u64),
}

impl Default for NamesOrCount {
    fn default() -> Self {
        Self::Count(0)
    }
}

impl NamesOrCount {
    /// How many.
    pub fn count(&self) -> u64 {
        match self {
            Self::Names(n) => n.len() as u64,
            Self::Count(c) => *c,
        }
    }
}

/// What a merge did (or would do).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MergeAnswer {
    /// The parent's new version (none for a dry run).
    pub version: Option<i64>,
    /// Cells taken from the copy.
    pub cells_taken: u64,
    /// The conflicts and the side each took.
    pub conflicts: Vec<Conflict>,
    /// Tabs added to the parent.
    pub tabs_added: NamesOrCount,
    /// Tabs removed from the parent.
    pub tabs_removed: NamesOrCount,
    /// Rows added.
    pub rows_added: u64,
    /// Rows removed.
    pub rows_removed: u64,
}

// ── Comments ────────────────────────────────────────────────────────────────────────────────────────────────

/// Open or resolved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    /// Open.
    #[default]
    Open,
    /// Resolved.
    Resolved,
}

impl ThreadStatus {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
        }
    }
}

/// A reaction and who gave it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    /// The emoji.
    pub emoji: String,
    /// Who.
    pub accounts: Vec<Account>,
}

/// One comment in a thread.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comment {
    /// Its id.
    pub id: String,
    /// The thread it is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Who wrote it.
    pub author: Account,
    /// The text.
    pub body: String,
    /// Who it mentions.
    pub mentions: Vec<Account>,
    /// Reactions.
    pub reactions: Vec<Reaction>,
    /// When.
    pub created_at: String,
    /// When it was last edited.
    pub edited_at: Option<String>,
    /// On the answer to writing it: who it mentions but can't open the sheet, so wasn't told.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentioned_without_access: Vec<Account>,
}

/// A comment and its replies, on a cell, range, row, column or tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thread {
    /// Its id.
    pub id: String,
    /// The sheet.
    pub sheet_id: String,
    /// The copy it is on, if any.
    pub copy_id: Option<String>,
    /// The tab's id.
    pub tab: u32,
    /// The tab's name.
    pub tab_name: String,
    /// Where it is now (moves with the cells); none is the whole tab.
    pub range: Option<String>,
    /// Where it was made.
    pub original_range: Option<String>,
    /// The cells' shown text when it was made.
    pub quote: Option<String>,
    /// True when its cells were deleted.
    pub detached: bool,
    /// Open or resolved.
    pub status: ThreadStatus,
    /// Who resolved it.
    pub resolved_by: Option<Account>,
    /// When.
    pub resolved_at: Option<String>,
    /// The comments, first one first.
    pub comments: Vec<Comment>,
    /// When it was started.
    pub created_at: String,
    /// Who started it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Account>,
    /// Its link in the web app, when the service gives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// On the answer to starting it: who it mentions but can't open the sheet, so wasn't told.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mentioned_without_access: Vec<Account>,
}

impl Thread {
    /// Who started it.
    pub fn author(&self) -> Option<&Account> {
        self.created_by
            .as_ref()
            .or_else(|| self.comments.first().map(|c| &c.author))
    }

    /// Where it is, as `Tab!B2:B9` (or `Tab` for the whole tab).
    pub fn place(&self) -> String {
        match &self.range {
            Some(r) => crate::a1::with_tab(&self.tab_name, r),
            None => crate::a1::quote_tab(&self.tab_name),
        }
    }
}

/// `GET {line}/comments`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadList {
    /// The threads.
    pub threads: Vec<Thread>,
}

/// Which threads to list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThreadFilter {
    /// Open ones.
    #[default]
    Open,
    /// Resolved ones.
    Resolved,
    /// Every one.
    All,
}

impl ThreadFilter {
    /// The word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::All => "all",
        }
    }
}

/// `POST {line}/comments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewThread {
    /// The tab.
    pub tab: TabRef,
    /// `B2:B9`, `B:B`, `3:3`; none is the whole tab.
    pub range: Option<String>,
    /// The text; `@c:ada` and `@si:analyst` mention.
    pub body: String,
}

// ── Search and meta ─────────────────────────────────────────────────────────────────────────────────────────

/// A cell whose text matched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchMatch {
    /// The tab's id.
    pub tab: u32,
    /// The tab's name.
    pub tab_name: String,
    /// The cell.
    pub a: String,
    /// Its text.
    pub text: String,
}

/// A sheet that matched, by title or cells.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchResult {
    /// The sheet.
    pub sheet: Sheet,
    /// The cells that matched.
    pub matches: Vec<SearchMatch>,
}

/// `GET /v1/search`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    /// The sheets.
    pub results: Vec<SearchResult>,
}

/// The service's limits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Rows per tab.
    pub max_rows: u64,
    /// Columns per tab.
    pub max_cols: u64,
    /// Bytes per upload.
    pub max_upload_bytes: u64,
    /// Cells per edit request.
    pub max_cells_per_edit: u64,
    /// Time limit enforced by the browser command worker.
    pub browser_command_timeout_seconds: u64,
    /// Time limit for trusted server file conversion.
    pub conversion_timeout_seconds: u64,
}

/// `GET /v1/meta`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Meta {
    /// `calc`.
    pub app_id: String,
    /// The service's version.
    pub version: String,
    /// The Silicon Accounts it trusts.
    pub accounts_url: String,
    /// Its limits.
    pub limits: Limits,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_round_trip() {
        for (v, j) in [
            (Value::Empty, json!({"t": "empty"})),
            (Value::Number(12.5), json!({"t": "number", "v": 12.5})),
            (Value::Text("hi".into()), json!({"t": "text", "v": "hi"})),
            (Value::Bool(true), json!({"t": "bool", "v": true})),
            (
                Value::Error("#DIV/0!".into()),
                json!({"t": "error", "v": "#DIV/0!"}),
            ),
        ] {
            assert_eq!(serde_json::to_value(&v).unwrap(), j);
            assert_eq!(serde_json::from_value::<Value>(j).unwrap(), v);
        }
        assert_eq!(Value::Number(3.0).to_string(), "3");
        assert_eq!(Value::Number(0.25).to_string(), "0.25");
        assert_eq!(Value::Number(3.0).to_json(), json!(3.0));
    }

    #[test]
    fn edits_serialize_as_the_api_says() {
        let e = Edit::set_cells(
            1,
            vec![CellInput {
                a: "B2".into(),
                input: "=A1*2".into(),
            }],
        );
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "set", "tab": 1, "cells": [{"a": "B2", "input": "=A1*2"}]})
        );
        let e = Edit::set_block("Sales", "A1", vec![vec!["name".into(), "amount".into()]]);
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "set", "tab": "Sales", "at": "A1", "rows": [["name", "amount"]]})
        );
        let e = Edit::InsertRows {
            tab: 1.into(),
            at: 4,
            count: 2,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "insert_rows", "tab": 1, "at": 4, "count": 2})
        );
        let e = Edit::TabProps {
            tab: 1.into(),
            frozen_rows: Some(1),
            frozen_cols: None,
            color: Some(None),
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "tab_props", "tab": 1, "frozen_rows": 1, "color": null})
        );
        let e = Edit::ColWidth {
            tab: 1.into(),
            cols: "A:C".into(),
            width: None,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "col_width", "tab": 1, "cols": "A:C", "width": null})
        );
        assert_eq!(
            serde_json::to_value(Edit::Undo).unwrap(),
            json!({"type": "undo"})
        );
        let e = Edit::Names {
            names: vec![NameDef {
                name: "Revenue".into(),
                reference: "Sales!$B$2:$B$100".into(),
            }],
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "names", "names": [{"name": "Revenue", "ref": "Sales!$B$2:$B$100"}]})
        );
        let p = StylePatch {
            bold: Some(true),
            fill: Some(None),
            num_fmt: Some(Some("0.00".into())),
            ..Default::default()
        };
        let e = Edit::Style {
            tab: 1.into(),
            range: "A1:C1".into(),
            patch: p,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({"type": "style", "tab": 1, "range": "A1:C1", "patch": {"bold": true, "fill": null, "num_fmt": "0.00"}})
        );
        let back: Edit = serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn run_into_shapes() {
        assert_eq!(
            serde_json::to_value(CommandInto::NewTab {
                new_tab: "By region".into()
            })
            .unwrap(),
            json!({"new_tab": "By region"})
        );
        assert_eq!(
            serde_json::to_value(CommandInto::Cells {
                tab: 3.into(),
                at: "A1".into(),
                replace: false
            })
            .unwrap(),
            json!({"tab": 3, "at": "A1"})
        );
        let r = CommandResultRequest {
            code: "return [[1]]".into(),
            base_version: 7,
            inputs: vec![1],
            columns: vec!["value".into()],
            rows: vec![vec![json!(1)]],
            into: CommandInto::NewTab {
                new_tab: "Result".into(),
            },
        };
        let body = serde_json::to_value(&r).unwrap();
        assert_eq!(body["base_version"], 7);
        assert_eq!(body["rows"], json!([[1]]));
        assert!(body.get("language").is_none());
    }

    #[test]
    fn via_reads_both_forms() {
        let m: Member = serde_json::from_value(
            json!({"account": {"id": "c:ada"}, "role": "editor", "via": "direct"}),
        )
        .unwrap();
        assert_eq!(m.via, Via::Direct);
        let m: Member =
            serde_json::from_value(json!({"account": {"id": "c:ada"}, "role": "viewer", "via": {"folder": {"id": "f1", "name": "Finance"}}}))
                .unwrap();
        assert!(matches!(m.via, Via::Folder(ref f) if f.name == "Finance"));
        assert_eq!(
            serde_json::to_value(&m.via).unwrap()["folder"]["name"],
            "Finance"
        );
    }

    #[test]
    fn lenient_answers() {
        let s: Sheet = serde_json::from_value(
            json!({"id": "k3j9xq2mpa7d", "title": "Q3", "role": "editor", "brand_new_field": 1}),
        )
        .unwrap();
        assert_eq!(s.role, Role::Editor);
        assert!(s.tabs.is_none());
        let v: Version =
            serde_json::from_value(json!({"version": 3, "kind": "something-new"})).unwrap();
        assert_eq!(v.kind, VersionKind::Other);
        let c: CellChange = serde_json::from_value(json!({"version": 3, "by": "6667", "before": {"t": "number", "v": 1.0}, "after": "=A1*2"})).unwrap();
        assert_eq!(c.by.uuid(), "6667");
        assert_eq!(c.after, Content::Formula("=A1*2".into()));
        assert_eq!(c.before.to_string(), "1");
        let c: CellChange = serde_json::from_value(json!({"by": {"uuid": "u", "id": "c:ada"}, "before": {"value": {"t": "text", "v": "x"}}})).unwrap();
        assert_eq!(c.by.label(&BTreeMap::new()), "c:ada");
        assert!(matches!(c.before, Content::Summary(_)));
        let m: MergeAnswer = serde_json::from_value(
            json!({"version": 9, "tabs_added": ["Notes"], "tabs_removed": 0}),
        )
        .unwrap();
        assert_eq!(m.tabs_added.count(), 1);
        assert_eq!(m.tabs_removed.count(), 0);
    }

    #[test]
    fn tab_refs() {
        assert_eq!(serde_json::to_value(TabRef::Id(2)).unwrap(), json!(2));
        assert_eq!(
            serde_json::to_value(TabRef::from("Sales")).unwrap(),
            json!("Sales")
        );
        assert_eq!(
            serde_json::from_value::<TabRef>(json!(2)).unwrap(),
            TabRef::Id(2)
        );
        assert_eq!(
            serde_json::from_value::<TabRef>(json!("x")).unwrap(),
            TabRef::Name("x".into())
        );
    }

    #[test]
    fn formats() {
        assert_eq!(Format::parse(".XLSX"), Some(Format::Xlsx));
        assert_eq!(Format::from_file_name("sales.2024.csv"), Some(Format::Csv));
        assert_eq!(Format::from_file_name("noext"), None);
        assert!(!Format::Pdf.can_import());
        assert!(Format::Parquet.single_tab());
    }

    #[test]
    fn styles_describe_themselves() {
        let s = Style {
            bold: true,
            fill: Some("#FFF2CC".into()),
            h_align: HAlign::Center,
            num_fmt: Some("0.00".into()),
            ..Default::default()
        };
        assert_eq!(
            s.describe(),
            "bold, fill #FFF2CC, align center, format 0.00"
        );
        assert!(Style::default().is_default());
    }
}
