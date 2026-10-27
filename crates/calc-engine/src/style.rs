//! How a cell looks. Styles are stored once per workbook and cells point at them by id (0 is the default).

use serde::{Deserialize, Serialize};

/// In-cell input controls. Kept in the interned cell presentation so fills, copies,
/// snapshots and undo carry the rule with the cell. Clearing its look retains it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellControl {
    Checkbox {
        /// Absent means real boolean values, rather than the text "TRUE"/"FALSE".
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

impl CellControl {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Person | Self::File | Self::Rating => {},
            Self::Checkbox { checked, unchecked } => {
                if checked.is_none() && unchecked.is_some() {
                    return Err("Set both custom checkbox values, or use TRUE and FALSE.".into());
                }
                if checked.as_ref().is_some_and(|s| s.len() > 256 || s == unchecked.as_deref().unwrap_or(""))
                    || unchecked.as_ref().is_some_and(|s| s.len() > 256)
                {
                    return Err("Use different checkbox values, each at most 256 characters.".into());
                }
            }
            Self::Dropdown { options } => {
                let unique: std::collections::HashSet<_> = options.iter().collect();
                if options.is_empty() || options.len() > 100 || unique.len() != options.len()
                    || options.iter().any(|s| s.trim().is_empty() || s.len() > 256)
                {
                    return Err("Add 1 to 100 distinct dropdown options, each at most 256 characters.".into());
                }
            }
        }
        Ok(())
    }

    pub fn unchecked_value(&self) -> Option<crate::value::Value> {
        match self {
            Self::Checkbox { checked: None, .. } => Some(crate::value::Value::Bool(false)),
            Self::Checkbox { unchecked, .. } => Some(crate::value::Value::from(unchecked.clone().unwrap_or_default())),
            Self::Rating => Some(crate::value::Value::Number(0.0)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HAlign {
    #[default]
    General,
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    #[default]
    Bottom,
    Middle,
    Top,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BorderStyle {
    #[default]
    None,
    Thin,
    Medium,
    Thick,
    Dashed,
    Dotted,
    Double,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Border {
    pub style: BorderStyle,
    /// `#RRGGBB`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// Everything about a cell's look. Absent fields are the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Style {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    /// Font size in points (default 11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    /// Text colour `#RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Fill colour `#RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub h_align: HAlign,
    #[serde(default, skip_serializing_if = "is_default")]
    pub v_align: VAlign,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrap: bool,
    /// Clip overflowing text instead of letting it extend into empty neighbours.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clip: bool,
    /// Counterclockwise degrees (-90..90); 255 stacks characters vertically.
    #[serde(default, skip_serializing_if = "is_default")]
    pub rotation: i16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<CellControl>,
    /// An Excel number format code (`0.00`, `#,##0`, `yyyy-mm-dd`, `0%`); none is General.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_fmt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_top: Option<Border>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_bottom: Option<Border>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_left: Option<Border>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border_right: Option<Border>,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

impl Style {
    pub fn is_default(&self) -> bool {
        *self == Style::default()
    }
}

/// A partial change to a style: each `Some` field replaces that part. `Some(None)` clears it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StylePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strike: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub font_size: Option<Option<u16>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub font: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub color: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub fill: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h_align: Option<HAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v_align: Option<VAlign>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clip: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<i16>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub control: Option<Option<CellControl>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub num_fmt: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_top: Option<Option<Border>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_bottom: Option<Option<Border>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_left: Option<Option<Border>>,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]
    pub border_right: Option<Option<Border>>,
    /// Clears every part first.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear: bool,
}

impl StylePatch {
    pub fn validate(&self) -> Result<(), String> {
        if self.font_size.flatten().is_some_and(|s|s==0 || s>400) || self.font.as_ref().and_then(|f|f.as_ref()).is_some_and(|f|f.len()>100) {return Err("Use a font size from 1 to 400 and a font name up to 100 characters.".into());}
        if self.rotation.is_some_and(|r| !(-90..=90).contains(&r) && r != 255) {
            return Err("Text rotation must be between -90 and 90 degrees, or stacked.".into());
        }
        if self.num_fmt.as_ref().and_then(|s| s.as_ref()).is_some_and(|s| s.len() > 512) {
            return Err("Use a number format no longer than 512 characters.".into());
        }
        if let Some(Some(control)) = &self.control { control.validate()?; }
        Ok(())
    }

    pub fn apply(&self, base: &Style) -> Style {
        let mut s = if self.clear { Style { control: base.control.clone(), ..Default::default() } } else { base.clone() };
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = &self.$f { s.$f = v.clone(); } )* };
        }
        set!(bold, italic, underline, strike, font_size, font, color, fill, h_align, v_align, wrap, clip, rotation, control, num_fmt,
             border_top, border_bottom, border_left, border_right);
        if let Some(f) = &s.num_fmt
            && (f.is_empty() || f.eq_ignore_ascii_case("general"))
        {
            s.num_fmt = None;
        }
        s
    }
}

pub(crate) mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// The workbook's table of styles.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Styles {
    list: Vec<Style>,
    #[serde(skip)]
    index: ahash::HashMap<Style, u32>,
}

impl Styles {
    pub fn new() -> Self {
        let mut s = Self { list: vec![Style::default()], index: Default::default() };
        s.index.insert(Style::default(), 0);
        s
    }

    pub fn rebuild_index(&mut self) {
        if self.list.is_empty() {
            self.list.push(Style::default());
        }
        self.index = self.list.iter().enumerate().map(|(i, s)| (s.clone(), i as u32)).collect();
    }

    pub fn get(&self, id: u32) -> &Style {
        self.list.get(id as usize).unwrap_or(&self.list[0])
    }

    pub fn intern(&mut self, style: Style) -> u32 {
        if let Some(&id) = self.index.get(&style) {
            return id;
        }
        let id = self.list.len() as u32;
        self.index.insert(style.clone(), id);
        self.list.push(style);
        id
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.len() <= 1
    }
}
