//! Excel number format codes: `0.00`, `#,##0`, `0%`, `$#,##0.00;[Red]-$#,##0.00`, `yyyy-mm-dd h:mm`, `@`, …

use crate::{
    dates::{MONTHS, WEEKDAYS, hms_from_serial, weekday_from_serial, ymd_from_serial},
    value::{Value, format_general},
};

/// The text a value shows with a format code, and the colour the code asks for, if any.
pub fn format_value(v: &Value, code: Option<&str>) -> (String, Option<&'static str>) {
    let code = code.unwrap_or("General");
    match v {
        Value::Number(n) => format_number(*n, code),
        Value::Text(s) => {
            // A text section (the fourth) or @ formats text.
            let sections = split_sections(code);
            if let Some(sec) = sections.get(3).or_else(|| sections.iter().find(|s| s.contains('@'))) {
                if sec.contains('@') {
                    let (body, color) = take_color(sec);
                    return (render_literal_with(body, s), color);
                }
            }
            (s.to_string(), None)
        }
        other => (other.display(), None),
    }
}

/// True when a format code shows dates or times.
pub fn is_date_format(code: &str) -> bool {
    let first = split_sections(code).into_iter().next().unwrap_or_default();
    if ["[h]", "[hh]", "[m]", "[mm]", "[s]", "[ss]"].iter().any(|tag| first.to_ascii_lowercase().contains(tag)) { return true; }
    let mut in_quote = false;
    let mut prev_backslash = false;
    let mut in_bracket = false;
    for c in first.chars() {
        if prev_backslash {
            prev_backslash = false;
            continue;
        }
        match c {
            '\\' => prev_backslash = true,
            '"' => in_quote = !in_quote,
            '[' if !in_quote => in_bracket = true,
            ']' if !in_quote => in_bracket = false,
            'y' | 'Y' | 'd' | 'D' | 'h' | 'H' | 's' | 'S' | 'm' | 'M' if !in_quote && !in_bracket => return true,
            _ => {}
        }
    }
    false
}

fn split_sections(code: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut in_quote = false;
    let mut escape = false;
    for c in code.chars() {
        if escape {
            out.last_mut().expect("section").push(c);
            escape = false;
            continue;
        }
        match c {
            '\\' => {
                escape = true;
                out.last_mut().expect("section").push(c);
            }
            '"' => {
                in_quote = !in_quote;
                out.last_mut().expect("section").push(c);
            }
            ';' if !in_quote => out.push(String::new()),
            _ => out.last_mut().expect("section").push(c),
        }
    }
    out
}

fn take_color(sec: &str) -> (&str, Option<&'static str>) {
    let mut s = sec;
    let mut color = None;
    // Strip leading [Red], [Blue], [Color10], [>=100] (conditions are handled by section choice).
    while let Some(rest) = s.strip_prefix('[') {
        let Some(end) = rest.find(']') else { break };
        let tag = &rest[..end];
        let lower = tag.to_ascii_lowercase();
        let known = match lower.as_str() {
            "red" => Some("#C00000"),
            "blue" => Some("#1F5FB8"),
            "green" => Some("#2E7D32"),
            "black" => Some("#000000"),
            "white" => Some("#FFFFFF"),
            "yellow" => Some("#B8860B"),
            "magenta" => Some("#B0179C"),
            "cyan" => Some("#00838F"),
            _ => None,
        };
        if known.is_some() {
            color = known;
        } else if lower.starts_with('$') || lower.starts_with('h') || lower.starts_with('m') || lower.starts_with('s') {
            // [$-409] locale tags are dropped; [h] elapsed time stays in the body.
            if !lower.starts_with('$') {
                break;
            }
        } else if !(lower.starts_with('<') || lower.starts_with('>') || lower.starts_with('=') || lower.starts_with("color")) {
            break;
        }
        s = &rest[end + 1..];
    }
    (s, color)
}

fn condition(sec: &str) -> Option<(String, f64)> {
    let mut remaining = sec;
    while let Some(rest) = remaining.strip_prefix('[') {
        let end = rest.find(']')?;
        let tag = &rest[..end];
        for op in [">=", "<=", "<>", ">", "<", "="] {
            if let Some(v) = tag.strip_prefix(op) {
                return Some((op.to_owned(), v.trim().parse().ok()?));
            }
        }
        remaining = &rest[end + 1..];
    }
    None
}

fn meets(op: &str, n: f64, v: f64) -> bool {
    match op {
        ">=" => n >= v,
        "<=" => n <= v,
        "<>" => n != v,
        ">" => n > v,
        "<" => n < v,
        _ => n == v,
    }
}

/// Formats a number with a code.
pub fn format_number(n: f64, code: &str) -> (String, Option<&'static str>) {
    if code.is_empty() || code.eq_ignore_ascii_case("general") || code == "@" {
        return (format_general(n), None);
    }
    let sections = split_sections(code);
    // Choose the section: conditions if present, else positive;negative;zero.
    let (sec, use_abs) = if sections.iter().any(|s| condition(s).is_some()) {
        let mut chosen = None;
        for s in sections.iter().take(2) {
            if let Some((op, v)) = condition(s)
                && meets(&op, n, v)
            {
                chosen = Some(s.clone());
                break;
            }
        }
        match chosen {
            Some(s) => (s, false),
            None => (sections.iter().take(3).find(|s| condition(s).is_none()).cloned().unwrap_or_default(), false),
        }
    } else if n < 0.0 && sections.len() >= 2 {
        (sections[1].clone(), true)
    } else if n == 0.0 && sections.len() >= 3 {
        (sections[2].clone(), true)
    } else {
        (sections[0].clone(), false)
    };
    let (body, color) = take_color(&sec);
    let value = if use_abs { n.abs() } else { n };
    if body.eq_ignore_ascii_case("general") {
        return (format_general(value), color);
    }
    if body.is_empty() { return (String::new(), color); }
    if is_date_format(body) {
        return (format_date(value, body), color);
    }
    (format_numeric(value, body), color)
}

/// Literal text in a code: quoted strings, backslash escapes, `_x` spacing, `*x` fill (dropped), `@` = text.
fn render_literal_with(body: &str, text: &str) -> String {
    let mut out = String::new();
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                    out.push(d);
                }
            }
            '\\' => {
                if let Some(d) = chars.next() {
                    out.push(d);
                }
            }
            '_' => {
                chars.next();
                out.push(' ');
            }
            '*' => {
                chars.next();
            }
            '@' => out.push_str(text),
            _ => out.push(c),
        }
    }
    out
}

fn format_numeric(n: f64, body: &str) -> String {
    if let Some(fraction) = format_fraction(n, body) { return fraction; }
    // Split the body into the numeric pattern and the literal text around it.
    let mut pattern = String::new();
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut seen_digit = false;
    let mut after = false;
    let mut chars = body.chars().peekable();
    let mut percent = 0;
    while let Some(c) = chars.next() {
        let lit = match c {
            '"' => {
                let mut s = String::new();
                for d in chars.by_ref() {
                    if d == '"' {
                        break;
                    }
                    s.push(d);
                }
                Some(s)
            }
            '\\' => chars.next().map(|d| d.to_string()),
            '_' => {
                chars.next();
                Some(" ".to_owned())
            }
            '*' => {
                chars.next();
                Some(String::new())
            }
            '%' => {
                percent += 1;
                Some("%".to_owned())
            }
            '0' | '#' | '?' | '.' | ',' | 'E' | 'e' => {
                if after && !matches!(c, 'E' | 'e') {
                    // Digits after literal text continue the pattern only in rare codes; treat as literal.
                    Some(c.to_string())
                } else {
                    if matches!(c, 'E' | 'e') {
                        // E+00 / E-00
                        pattern.push('E');
                        if let Some(&s) = chars.peek()
                            && (s == '+' || s == '-')
                        {
                            pattern.push(s);
                            chars.next();
                        }
                        while let Some(&d) = chars.peek() {
                            if d == '0' || d == '#' {
                                pattern.push(d);
                                chars.next();
                            } else {
                                break;
                            }
                        }
                    } else {
                        pattern.push(c);
                    }
                    seen_digit = true;
                    None
                }
            }
            '/' if seen_digit => {
                // Fractions are rendered as decimals.
                pattern.push('.');
                None
            }
            _ => Some(c.to_string()),
        };
        if let Some(l) = lit {
            if seen_digit {
                after = true;
                suffix.push_str(&l);
            } else {
                prefix.push_str(&l);
            }
        }
    }
    if pattern.is_empty() {
        // A section of only text ("big") shows the text, not the number.
        return format!("{prefix}{suffix}");
    }
    let mut v = n * 100f64.powi(percent);
    // Trailing commas scale by thousands: 0.0,, = millions.
    let trimmed = pattern.trim_end_matches(',');
    let scale = pattern.len() - trimmed.len();
    v /= 1000f64.powi(scale as i32);
    let pattern = trimmed;
    let negative = v < 0.0;
    let v = v.abs();

    let digits = if let Some(epos) = pattern.find('E') {
        let mant = &pattern[..epos];
        let exp_part = &pattern[epos + 1..];
        let decimals = mant.split_once('.').map(|(_, f)| f.len()).unwrap_or(0);
        let exp_digits = exp_part.trim_start_matches(['+', '-']).len().max(1);
        let mut exp = if v == 0.0 { 0 } else { v.log10().floor() as i32 };
        let mut m = if v == 0.0 { 0.0 } else { v / 10f64.powi(exp) };
        let rounded: f64 = format!("{:.*}", decimals, m).parse().unwrap_or(m);
        if rounded >= 10.0 {
            exp += 1;
            m = v / 10f64.powi(exp);
        }
        let sign = if exp < 0 { "-" } else if exp_part.starts_with('+') { "+" } else { "" };
        format!("{:.*}E{}{:0width$}", decimals, m, sign, exp.abs(), width = exp_digits)
    } else {
        let (int_pat, frac_pat) = match pattern.split_once('.') {
            Some((a, b)) => (a.to_owned(), b.to_owned()),
            None => (pattern.to_owned(), String::new()),
        };
        let grouping = int_pat.contains(',');
        let int_pat: String = int_pat.chars().filter(|&c| c != ',').collect();
        let max_dec = frac_pat.len();
        let min_dec = frac_pat.rfind('0').map_or(0, |i| i + 1);
        let s = format!("{:.*}", max_dec, v);
        let (int_s, frac_s) = match s.split_once('.') {
            Some((a, b)) => (a.to_owned(), b.to_owned()),
            None => (s.clone(), String::new()),
        };
        let mut frac = frac_s;
        while frac.len() > min_dec && frac.ends_with('0') {
            frac.pop();
        }
        let padding = frac_pat.chars().skip(frac.len()).filter(|&c| c == '?').count();
        frac.push_str(&" ".repeat(padding));
        let min_int = int_pat.chars().filter(|&c| c == '0').count();
        let mut int_digits = if int_s == "0" && min_int == 0 { String::new() } else { int_s };
        while int_digits.len() < min_int {
            int_digits.insert(0, '0');
        }
        let spaces = int_pat.chars().filter(|&c| c == '?').count().saturating_sub(int_digits.len());
        int_digits.insert_str(0, &" ".repeat(spaces));
        if grouping && int_digits.len() > 3 {
            let mut g = String::new();
            for (i, ch) in int_digits.chars().enumerate() {
                if i > 0 && (int_digits.len() - i) % 3 == 0 {
                    g.push(',');
                }
                g.push(ch);
            }
            int_digits = g;
        }
        if frac.is_empty() && !frac_pat.is_empty() && min_dec == 0 && pattern.ends_with('.') {
            format!("{int_digits}.")
        } else if frac.is_empty() {
            int_digits
        } else {
            format!("{int_digits}.{frac}")
        }
    };
    let all_zero = !digits.chars().any(|c| c.is_ascii_digit() && c != '0');
    let sign = if negative && !all_zero { "-" } else { "" };
    format!("{sign}{prefix}{digits}{suffix}")
}

/// Mixed/improper fractions, with a fixed denominator or a bounded approximation.
fn format_fraction(n: f64, body: &str) -> Option<String> {
    let slash = body.find('/')?;
    let before = &body[..slash];
    let num_start = before.rfind(|c: char| !matches!(c, '0' | '#' | '?')).map_or(0, |i| i + 1);
    if num_start == before.len() { return None; }
    let den_end = body[slash + 1..].find(|c: char| !matches!(c, '0'..='9' | '#' | '?')).map_or(body.len(), |i| slash + 1 + i);
    let denominator = &body[slash + 1..den_end];
    if denominator.is_empty() { return None; }
    let first = body[..num_start].find(['0', '#', '?']).unwrap_or(num_start);
    let mixed = first < num_start;
    let prefix = render_literal_with(&body[..first], "");
    let suffix = render_literal_with(&body[den_end..], "");
    let abs = n.abs();
    if !abs.is_finite() || abs > 1e12 { return None; }
    let mut whole = if mixed { abs.floor() as u64 } else { 0 };
    let value = if mixed { abs.fract() } else { abs };
    let fixed = denominator.parse::<u64>().ok().filter(|&v| v > 0);
    let (mut numerator, divisor) = if let Some(d) = fixed {
        ((value * d as f64).round() as u64, d)
    } else {
        let limit = 10u64.pow(denominator.len().min(4) as u32) - 1;
        // Continued fractions plus the best final semiconvergent.
        let (mut p0, mut q0, mut p1, mut q1) = (0u64, 1u64, 1u64, 0u64);
        let mut x = value;
        loop {
            let a = x.floor() as u64;
            let q2 = q0.saturating_add(a.saturating_mul(q1));
            if q2 > limit { break; }
            let p2 = p0.saturating_add(a.saturating_mul(p1));
            (p0, q0, p1, q1) = (p1, q1, p2, q2);
            let rem = x - a as f64;
            if rem.abs() < 1e-12 { break; }
            x = 1.0 / rem;
        }
        if q1 == 0 { (value.round() as u64, 1) } else {
            let k = (limit - q0) / q1;
            let (p, q) = (p0 + k * p1, q0 + k * q1);
            if q > 0 && (p as f64 / q as f64 - value).abs() < (p1 as f64 / q1 as f64 - value).abs() { (p, q) } else { (p1, q1) }
        }
    };
    if mixed && numerator >= divisor { whole += numerator / divisor; numerator %= divisor; }
    let digits = if mixed {
        if numerator == 0 { whole.to_string() }
        else if whole == 0 { format!("{numerator}/{divisor}") }
        else { format!("{whole} {numerator}/{divisor}") }
    } else { format!("{numerator}/{divisor}") };
    Some(format!("{}{prefix}{digits}{suffix}", if n < 0.0 && (whole > 0 || numerator > 0) { "-" } else { "" }))
}

fn format_date(serial: f64, body: &str) -> String {
    if serial < 0.0 && body.contains('[') { return format!("-{}", format_date(-serial, body)); }
    let Some((y, mo, d)) = ymd_from_serial(serial) else {
        return "#".repeat(8);
    };
    let has_ampm = {
        let l = body.to_ascii_lowercase();
        l.contains("am/pm") || l.contains("a/p")
    };
    // Round to the precision shown: seconds unless fractional seconds are in the code.
    let (mut h, mi, s, ms) = hms_from_serial(serial);
    let wd = weekday_from_serial(serial) as usize;
    let mut out = String::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    // Whether the previous date/time token was an hour, so `mm` after it means minutes.
    let mut last_was_hour = false;
    let lower = |c: char| c.to_ascii_lowercase();
    let run = |i: usize, c: char| chars[i..].iter().take_while(|&&x| lower(x) == c).count();
    let next_is_seconds = |from: usize| -> bool {
        let mut j = from;
        while j < chars.len() {
            let c = lower(chars[j]);
            if c == 's' {
                return true;
            }
            if c.is_ascii_alphabetic() {
                return false;
            }
            j += 1;
        }
        false
    };
    if has_ampm {
        h %= 12;
        if h == 0 {
            h = 12;
        }
    }
    let (h24, _) = (hms_from_serial(serial).0, ());
    while i < chars.len() {
        let c = chars[i];
        let lc = lower(c);
        match lc {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if i + 1 < chars.len() {
                    out.push(chars[i + 1]);
                }
                i += 2;
            }
            '[' => {
                // [h] elapsed hours, [mm] elapsed minutes
                let end = chars[i..].iter().position(|&x| x == ']').map(|p| i + p).unwrap_or(chars.len() - 1);
                let tag: String = chars[i + 1..end].iter().collect::<String>().to_ascii_lowercase();
                let total_secs = (serial * 86400.0).round() as i64;
                match tag.chars().next() {
                    Some('h') => out.push_str(&format!("{:0w$}", total_secs / 3600, w = tag.len())),
                    Some('m') => out.push_str(&format!("{:0w$}", total_secs / 60, w = tag.len())),
                    Some('s') => out.push_str(&format!("{:0w$}", total_secs, w = tag.len())),
                    _ => {}
                }
                last_was_hour = tag.starts_with('h');
                i = end + 1;
            }
            'y' => {
                let n = run(i, 'y');
                if n <= 2 {
                    out.push_str(&format!("{:02}", y % 100));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
                i += n;
                last_was_hour = false;
            }
            'm' => {
                let n = run(i, 'm');
                let minutes = (last_was_hour || next_is_seconds(i + n)) && n <= 2;
                if minutes {
                    out.push_str(&if n == 1 { mi.to_string() } else { format!("{mi:02}") });
                } else {
                    match n {
                        1 => out.push_str(&mo.to_string()),
                        2 => out.push_str(&format!("{mo:02}")),
                        3 => out.push_str(&MONTHS[mo as usize - 1][..3]),
                        5 => out.push_str(&MONTHS[mo as usize - 1][..1]),
                        _ => out.push_str(MONTHS[mo as usize - 1]),
                    }
                }
                i += n;
                last_was_hour = false;
            }
            'd' => {
                let n = run(i, 'd');
                match n {
                    1 => out.push_str(&d.to_string()),
                    2 => out.push_str(&format!("{d:02}")),
                    3 => out.push_str(&WEEKDAYS[wd][..3]),
                    _ => out.push_str(WEEKDAYS[wd]),
                }
                i += n;
                last_was_hour = false;
            }
            'h' => {
                let n = run(i, 'h');
                out.push_str(&if n == 1 { h.to_string() } else { format!("{h:02}") });
                i += n;
                last_was_hour = true;
            }
            's' => {
                let n = run(i, 's');
                out.push_str(&if n == 1 { s.to_string() } else { format!("{s:02}") });
                i += n;
                // .0, .00 fractional seconds
                if i < chars.len() && chars[i] == '.' {
                    let zeros = chars[i + 1..].iter().take_while(|&&x| x == '0').count();
                    if zeros > 0 {
                        let frac = format!("{:03}", ms);
                        out.push('.');
                        out.push_str(&frac[..zeros.min(3)]);
                        i += 1 + zeros;
                    }
                }
                last_was_hour = false;
            }
            'a' if has_ampm => {
                let rest: String = chars[i..].iter().collect::<String>().to_ascii_lowercase();
                if rest.starts_with("am/pm") {
                    out.push_str(if h24 < 12 { "AM" } else { "PM" });
                    i += 5;
                } else if rest.starts_with("a/p") {
                    out.push_str(if h24 < 12 { "A" } else { "P" });
                    i += 3;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            '_' => {
                out.push(' ');
                i += 2;
            }
            '*' => i += 2,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: f64, code: &str) -> String {
        format_number(n, code).0
    }

    #[test]
    fn numbers() {
        assert_eq!(f(1234.567, "0.00"), "1234.57");
        assert_eq!(f(1234.567, "#,##0"), "1,235");
        assert_eq!(f(1234.567, "#,##0.00"), "1,234.57");
        assert_eq!(f(0.256, "0%"), "26%");
        assert_eq!(f(0.256, "0.0%"), "25.6%");
        assert_eq!(f(-5.0, "$#,##0.00"), "-$5.00");
        assert_eq!(f(-5.0, "$#,##0.00;($#,##0.00)"), "($5.00)");
        assert_eq!(f(0.0, "0;-0;\"zero\""), "zero");
        assert_eq!(f(12345.0, "0.00E+00"), "1.23E+04");
        assert_eq!(f(0.5, "#.##"), ".5");
        assert_eq!(f(7.0, "000"), "007");
        assert_eq!(f(1500000.0, "0.0,,\"M\""), "1.5M");
        assert_eq!(format_number(-3.0, "0;[Red]-0").1, Some("#C00000"));
        assert_eq!(f(150.0, "[>=100]\"big\";\"small\""), "big");
    }

    #[test]
    fn dates() {
        let s = 46312.75; // 2026-10-17 18:00
        assert_eq!(f(s, "yyyy-mm-dd"), "2026-10-17");
        assert_eq!(f(s, "m/d/yyyy"), "10/17/2026");
        assert_eq!(f(s, "d mmm yyyy"), "17 Oct 2026");
        assert_eq!(f(s, "dddd, mmmm d"), "Saturday, October 17");
        assert_eq!(f(s, "h:mm"), "18:00");
        assert_eq!(f(s, "h:mm AM/PM"), "6:00 PM");
        assert_eq!(f(s, "yyyy-mm-dd hh:mm:ss"), "2026-10-17 18:00:00");
        assert_eq!(f(1.5, "[h]:mm"), "36:00");
        assert!(is_date_format("yyyy-mm-dd"));
        assert!(!is_date_format("0.00"));
        assert!(!is_date_format("\"day\" 0"));
    }

    #[test]
    fn text_section() {
        assert_eq!(format_value(&Value::from("x"), Some("0;0;0;\"[\"@\"]\"")).0, "[x]");
    }
}
