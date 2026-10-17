//! Turning the bytes of a text file (CSV, TSV, JSON) into text, whatever it was saved as: UTF-8 with or without
//! a byte order mark, UTF-16 in either byte order (with a mark, or recognised by its zero bytes), and
//! Windows-1252 for anything that isn't valid UTF-8 (what Excel on Windows writes as "CSV").

use std::borrow::Cow;

/// The text and the name of the encoding it was read as.
pub(crate) fn decode_text(bytes: &[u8]) -> (Cow<'_, str>, &'static str) {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF".as_slice()) {
        return (String::from_utf8_lossy(rest), "UTF-8");
    }
    if bytes.starts_with(&[0xFF, 0xFE]) && !bytes.starts_with(&[0xFF, 0xFE, 0, 0]) {
        let (t, _) = encoding_rs::UTF_16LE.decode_without_bom_handling(&bytes[2..]);
        return (Cow::Owned(t.into_owned()), "UTF-16LE");
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (t, _) = encoding_rs::UTF_16BE.decode_without_bom_handling(&bytes[2..]);
        return (Cow::Owned(t.into_owned()), "UTF-16BE");
    }
    if let Some(le) = looks_utf16(bytes) {
        let enc = if le { encoding_rs::UTF_16LE } else { encoding_rs::UTF_16BE };
        let (t, _) = enc.decode_without_bom_handling(bytes);
        return (Cow::Owned(t.into_owned()), if le { "UTF-16LE" } else { "UTF-16BE" });
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (Cow::Borrowed(s), "UTF-8"),
        Err(e) if e.error_len().is_none() && e.valid_up_to() + 4 > bytes.len() => {
            // Only the very end is cut off mid-character (a truncated sample): still UTF-8.
            (String::from_utf8_lossy(bytes), "UTF-8")
        }
        Err(_) => {
            let (t, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
            (Cow::Owned(t.into_owned()), "Windows-1252")
        }
    }
}

/// UTF-16 without a byte order mark: most of the first bytes' odd (LE) or even (BE) positions are zero.
fn looks_utf16(bytes: &[u8]) -> Option<bool> {
    let n = bytes.len().min(2048) & !1;
    if n < 8 {
        return None;
    }
    let (mut even, mut odd) = (0usize, 0usize);
    for (i, &b) in bytes[..n].iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 {
                even += 1;
            } else {
                odd += 1;
            }
        }
    }
    let half = n / 2;
    if odd * 10 >= half * 7 && even * 10 < half {
        Some(true)
    } else if even * 10 >= half * 7 && odd * 10 < half {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str, le: bool, bom: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if bom {
            out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
        }
        for u in s.encode_utf16() {
            out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
        }
        out
    }

    #[test]
    fn encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFa,b").0, "a,b");
        assert_eq!(decode_text(&utf16("na\u{ef}ve,1", true, true)).0, "na\u{ef}ve,1");
        assert_eq!(decode_text(&utf16("na\u{ef}ve,1", false, true)).0, "na\u{ef}ve,1");
        assert_eq!(decode_text(&utf16("name,amount\nada,1", true, false)), (Cow::Owned("name,amount\nada,1".into()), "UTF-16LE"));
        assert_eq!(decode_text(b"caf\xE9,\x80"), (Cow::Owned("caf\u{e9},\u{20ac}".into()), "Windows-1252"));
        assert_eq!(decode_text("caf\u{e9}".as_bytes()).1, "UTF-8");
    }
}
