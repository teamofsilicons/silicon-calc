//! Small helpers over quick-xml shared by the XLSX and ODS readers and writers. Elements and attributes are
//! matched by local name (namespace prefixes differ between producers, and strict OOXML uses other URIs).

use std::{borrow::Cow, io::BufRead};

use quick_xml::{
    Reader, XmlVersion,
    events::{BytesStart, Event, attributes::Attribute},
};

/// The local part of a qualified name (`x:c` → `c`).
pub(crate) fn local(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// The local name of an element.
pub(crate) fn name_of<'a>(e: &'a BytesStart<'_>) -> &'a [u8] {
    let n = e.name().into_inner();
    local(n)
}

/// An attribute's value as XML reads it: entities and character references resolved, literal line breaks and
/// tabs normalized to spaces (producers that mean a line break write `&#10;`, which is kept).
fn value_of(a: &Attribute<'_>) -> String {
    match a.normalized_value(XmlVersion::Implicit1_0) {
        Ok(v) => v.into_owned(),
        Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
    }
}

/// An attribute's value (unescaped) by its local name.
pub(crate) fn attr(e: &BytesStart<'_>, key: &[u8]) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if local(a.key.into_inner()) == key {
            return Some(value_of(&a));
        }
    }
    None
}

/// The raw bytes of an attribute by local name (no unescaping, no allocation), for hot paths like cell refs.
pub(crate) fn attr_raw<'a>(e: &'a BytesStart<'_>, key: &[u8]) -> Option<Cow<'a, [u8]>> {
    for a in e.attributes().with_checks(false).flatten() {
        if local(a.key.into_inner()) == key {
            return Some(a.value);
        }
    }
    None
}

/// `1`, `true`, `on` (case-insensitive) are true.
pub(crate) fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on")
}

pub(crate) fn attr_u32(e: &BytesStart<'_>, key: &[u8]) -> Option<u32> {
    attr_raw(e, key).and_then(|v| std::str::from_utf8(&v).ok()?.trim().parse().ok())
}

pub(crate) fn attr_f64(e: &BytesStart<'_>, key: &[u8]) -> Option<f64> {
    attr_raw(e, key).and_then(|v| std::str::from_utf8(&v).ok()?.trim().parse().ok())
}

/// Appends the text an event carries (text, CDATA, or an entity / character reference).
pub(crate) fn push_text(out: &mut String, ev: &Event<'_>) {
    match ev {
        // Line ends are normalized to `\n`, as XML reads them.
        Event::Text(t) => match t.xml10_content() {
            Ok(s) => out.push_str(&s),
            Err(_) => out.push_str(&String::from_utf8_lossy(t)),
        },
        Event::CData(t) => match t.xml10_content() {
            Ok(s) => out.push_str(&s),
            Err(_) => out.push_str(&String::from_utf8_lossy(t)),
        },
        Event::GeneralRef(r) => {
            if let Ok(Some(c)) = r.resolve_char_ref() {
                out.push(c);
            } else {
                let name: &[u8] = r;
                match name {
                    b"amp" => out.push('&'),
                    b"lt" => out.push('<'),
                    b"gt" => out.push('>'),
                    b"quot" => out.push('"'),
                    b"apos" => out.push('\''),
                    other => {
                        out.push('&');
                        out.push_str(&String::from_utf8_lossy(other));
                        out.push(';');
                    }
                }
            }
        }
        _ => {}
    }
}

/// A reader over a part, lenient about mismatched end tags (some producers write them).
pub(crate) fn reader<R: BufRead>(r: R) -> Reader<R> {
    let mut reader = Reader::from_reader(r);
    let cfg = reader.config_mut();
    cfg.check_end_names = false;
    cfg.trim_text(false);
    reader
}

/// Reads everything up to the end of the element just started (`depth` 1), collecting its text, skipping the
/// text of children named in `skip` (like phonetic runs).
pub(crate) fn read_text<R: BufRead>(reader: &mut Reader<R>, buf: &mut Vec<u8>, skip: &[&[u8]]) -> String {
    let mut out = String::new();
    let mut depth = 1usize;
    let mut skipping = 0usize;
    loop {
        buf.clear();
        let ev = match reader.read_event_into(buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match &ev {
            Event::Start(e) => {
                depth += 1;
                if skipping > 0 || skip.contains(&name_of(e)) {
                    skipping += 1;
                }
            }
            Event::End(_) => {
                depth -= 1;
                if skipping > 0 {
                    skipping -= 1;
                }
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            other => {
                if skipping == 0 {
                    push_text(&mut out, other);
                }
            }
        }
    }
    out
}

/// Skips to the end of the element just started.
pub(crate) fn skip_element<R: BufRead>(reader: &mut Reader<R>, buf: &mut Vec<u8>) {
    let mut depth = 1usize;
    loop {
        buf.clear();
        match reader.read_event_into(buf) {
            Ok(Event::Start(_)) => depth += 1,
            Ok(Event::End(_)) => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            Ok(Event::Eof) | Err(_) => return,
            _ => {}
        }
    }
}

/// Text made safe for XML content and attribute values. Characters XML can't hold are dropped; line breaks and
/// tabs in attributes are written as character references so they survive.
#[cfg(any(test, feature = "formats-wip"))]
pub(crate) fn escape(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\'' | b'\n' | b'\r' | b'\t') || b < 0x20)
        && !s.contains(['\u{FFFE}', '\u{FFFF}'])
    {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_with_entities() {
        let xml = "<si><t>a &amp; b &#x41;&#10;</t><rPh><t>skip</t></rPh><t>c</t></si>";
        let mut r = reader(xml.as_bytes());
        let mut buf = Vec::new();
        // Into <si>.
        loop {
            buf.clear();
            if let Ok(Event::Start(e)) = r.read_event_into(&mut buf)
                && name_of(&e) == b"si"
            {
                break;
            }
        }
        let t = read_text(&mut r, &mut buf, &[b"rPh"]);
        assert_eq!(t, "a & b A\nc");
        assert_eq!(escape("a<b>&\"x\"\n"), "a&lt;b&gt;&amp;&quot;x&quot;&#10;");
        assert_eq!(escape("plain"), "plain");
    }
}
