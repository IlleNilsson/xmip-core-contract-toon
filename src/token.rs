//! The tokens of a TOON line: a key, a quoted or bare scalar, and the
//! delimited values of a row or an inline array. The document walk is in
//! `toon`; this file reads one piece of text at a time and says where it
//! stopped.

use serde_json::{Number, Value};

use crate::toon::{Line, Malformed, fail};

/// `key` and what follows it, which starts with `:` or `[`.
pub(crate) fn key<'a>(text: &'a str, line: Line<'_>) -> Result<(String, &'a str), Malformed> {
    if text.starts_with('"') {
        let end =
            closing_quote(text).ok_or_else(|| fail(line, 0, "the key's quote never closes"))?;
        let key = unquote(&text[..=end]).map_err(|message| fail(line, 0, message))?;
        let after = &text[end + 1..];
        return if after.starts_with([':', '[']) {
            Ok((key, after))
        } else {
            Err(fail(line, end + 1, "expected : or [ after the key"))
        };
    }
    let end = text
        .find([':', '['])
        .ok_or_else(|| fail(line, 0, "expected key: value"))?;
    let key = text[..end].trim();
    if key.is_empty() {
        return Err(fail(line, 0, "the key is empty"));
    }
    Ok((key.to_string(), &text[end..]))
}

pub(crate) fn is_field(text: &str) -> bool {
    if text.starts_with('"') {
        return closing_quote(text).is_some_and(|end| text[end + 1..].starts_with([':', '[']));
    }
    text.find([':', '['])
        .is_some_and(|end| !text[..end].trim().is_empty())
}

/// The index of the quote that closes the string starting at `text[0]`.
pub(crate) fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, character) in text.char_indices().skip(1) {
        match character {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some(index),
            _ => escaped = false,
        }
    }
    None
}

/// The text of a quoted string, escapes resolved.
pub(crate) fn unquote(quoted: &str) -> Result<String, String> {
    let inner = quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or_else(|| "expected a quoted string".to_string())?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        out.push(match chars.next() {
            Some('"') => '"',
            Some('\\') => '\\',
            Some('n') => '\n',
            Some('r') => '\r',
            Some('t') => '\t',
            other => return Err(format!("unknown escape \\{}", other.unwrap_or(' '))),
        });
    }
    Ok(out)
}

/// One value as written: quoted text, a keyword, a number, or bare text.
pub(crate) fn primitive(text: &str, line: Line<'_>, offset: usize) -> Result<Value, Malformed> {
    if text.starts_with('"') {
        let end =
            closing_quote(text).ok_or_else(|| fail(line, offset, "the quote never closes"))?;
        if !text[end + 1..].trim().is_empty() {
            return Err(fail(
                line,
                offset + end + 1,
                "content after the closing quote",
            ));
        }
        return unquote(&text[..=end])
            .map(Value::String)
            .map_err(|message| fail(line, offset, message));
    }
    Ok(match text {
        "null" => Value::Null,
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        number if is_number(number) => number
            .parse::<i64>()
            .map(Number::from)
            .ok()
            .or_else(|| number.parse::<u64>().map(Number::from).ok())
            .or_else(|| number.parse::<f64>().ok().and_then(Number::from_f64))
            .map_or_else(|| Value::String(number.to_string()), Value::Number),
        other => Value::String(other.to_string()),
    })
}

/// The JSON number grammar, which is what an unquoted number must satisfy.
pub(crate) fn is_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(bytes.first() == Some(&b'-'));
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    match bytes.get(at) {
        Some(b'0') => at += 1,
        Some(b'1'..=b'9') => at += digits(at),
        _ => return false,
    }
    if bytes.get(at) == Some(&b'.') {
        let fraction = digits(at + 1);
        if fraction == 0 {
            return false;
        }
        at += 1 + fraction;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let exponent = digits(at);
        if exponent == 0 {
            return false;
        }
        at += exponent;
    }
    at == bytes.len()
}

/// The delimited values of an inline array or a table row.
pub(crate) fn split(text: &str, delimiter: char, line: Line<'_>) -> Result<Vec<Value>, Malformed> {
    let mut values = Vec::new();
    let mut at = 0;
    loop {
        let rest = &text[at..];
        let skipped = rest.len() - rest.trim_start().len();
        let start = at + skipped;
        let rest = &text[start..];
        let end = if rest.starts_with('"') {
            let close =
                closing_quote(rest).ok_or_else(|| fail(line, start, "the quote never closes"))?;
            let after = rest[close + 1..].trim_start();
            if !(after.is_empty() || after.starts_with(delimiter)) {
                return Err(fail(
                    line,
                    start + close + 1,
                    "content after the closing quote",
                ));
            }
            start + rest.len() - after.len()
        } else {
            start + rest.find(delimiter).unwrap_or(rest.len())
        };
        values.push(primitive(text[start..end].trim(), line, start)?);
        match text[end..].strip_prefix(delimiter) {
            Some(_) => at = end + delimiter.len_utf8(),
            None => return Ok(values),
        }
    }
}
