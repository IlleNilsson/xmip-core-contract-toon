//! Token-Oriented Object Notation, read into the JSON model.
//!
//! TOON is JSON's data model in fewer tokens: an object is `key: value`
//! lines nested by two-space indentation; a primitive array is inline,
//! `tags[3]: a,b,c`; an array of objects of one shape is a table,
//! `items[2]{id,name}:` with one row per line below it; anything else is a
//! list, `key[N]:` with one `- item` per line. A string is quoted with `"`
//! when it would otherwise read as something else; `null`, `true`, `false`
//! and numbers read as themselves. The delimiter is a comma unless the header
//! says tab or pipe, `[3|]`, and a count may carry the `#` marker, `[#3]`.
//!
//! The reader is strict, as the specification's decoder is: a count that does
//! not match, a row with the wrong number of values, a stray indentation or an
//! unknown escape is refused with its line and column.

use std::fmt;

use serde_json::{Map, Value};

use crate::token::{is_field, key, primitive, split, unquote};

/// Where a document stopped being TOON, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Malformed {
    /// Counted from one.
    pub line: usize,
    /// Counted from one.
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {} column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for Malformed {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Line<'a> {
    pub(crate) number: usize,
    pub(crate) depth: usize,
    /// The text past the indentation, its column in the source counted from
    /// one.
    pub(crate) column: usize,
    pub(crate) text: &'a str,
}

pub(crate) fn fail(line: Line<'_>, offset: usize, message: impl Into<String>) -> Malformed {
    Malformed {
        line: line.number,
        column: line.column + offset,
        message: message.into(),
    }
}

/// Read a TOON document. An empty document is an empty object.
///
/// # Errors
/// The text is not TOON; the error names the line and column.
pub fn parse(text: &str) -> Result<Value, Malformed> {
    let lines = lines(text)?;
    let mut parser = Parser {
        lines: &lines,
        at: 0,
    };
    let value = parser.document()?;
    match parser.peek() {
        Some(extra) => Err(fail(extra, 0, "content after the document")),
        None => Ok(value),
    }
}

fn lines(text: &str) -> Result<Vec<Line<'_>>, Malformed> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let indent = raw.len() - raw.trim_start_matches(' ').len();
        let rest = raw[indent..].trim_end();
        let at = |column: usize, message: &str| Malformed {
            line: number,
            column,
            message: message.to_string(),
        };
        if rest.starts_with('\t') {
            return Err(at(
                indent + 1,
                "a tab in the indentation; TOON indents with spaces",
            ));
        }
        if indent % 2 != 0 {
            return Err(at(
                indent + 1,
                "the indentation is not a multiple of two spaces",
            ));
        }
        out.push(Line {
            number,
            depth: indent / 2,
            column: indent + 1,
            text: rest,
        });
    }
    Ok(out)
}

/// An array header: `[N]`, `[#N]`, `[N|]`, `[N]{f1,f2}`, followed by `:`.
struct Header {
    count: usize,
    delimiter: char,
    fields: Option<Vec<String>>,
}

impl Header {
    /// Read the header at the start of `text` and give back what follows the
    /// colon, trimmed.
    fn parse<'a>(
        text: &'a str,
        line: Line<'_>,
        offset: usize,
    ) -> Result<(Self, &'a str), Malformed> {
        let inner = text
            .strip_prefix('[')
            .ok_or_else(|| fail(line, offset, "expected ["))?;
        let inner = inner.strip_prefix('#').unwrap_or(inner);
        let digits = inner.len() - inner.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let count: usize = inner[..digits]
            .parse()
            .map_err(|_| fail(line, offset + 1, "expected the array's length"))?;
        let mut rest = &inner[digits..];
        let mut delimiter = ',';
        if let Some(after) = rest.strip_prefix(['\t', '|']) {
            delimiter = rest.chars().next().unwrap_or(',');
            rest = after;
        }
        let mut rest = rest
            .strip_prefix(']')
            .ok_or_else(|| fail(line, offset + digits + 1, "expected ] after the length"))?;
        let mut fields = None;
        if let Some(inside) = rest.strip_prefix('{') {
            let close = inside
                .find('}')
                .ok_or_else(|| fail(line, offset, "the field list is not closed with }"))?;
            let names = inside[..close]
                .split(delimiter)
                .map(|name| unquote(name.trim()).unwrap_or_else(|_| name.trim().to_string()))
                .collect();
            fields = Some(names);
            rest = &inside[close + 1..];
        }
        let rest = rest
            .strip_prefix(':')
            .ok_or_else(|| fail(line, offset, "expected : after the array header"))?;
        Ok((
            Self {
                count,
                delimiter,
                fields,
            },
            rest.trim(),
        ))
    }
}

struct Parser<'a, 'b> {
    lines: &'b [Line<'a>],
    at: usize,
}

impl<'a> Parser<'a, '_> {
    fn peek(&self) -> Option<Line<'a>> {
        self.lines.get(self.at).copied()
    }

    fn peek_at(&self, depth: usize) -> Option<Line<'a>> {
        self.peek().filter(|line| line.depth == depth)
    }

    fn document(&mut self) -> Result<Value, Malformed> {
        let Some(first) = self.peek() else {
            return Ok(Value::Object(Map::new()));
        };
        if first.depth != 0 {
            return Err(fail(first, 0, "the document starts indented"));
        }
        if first.text.starts_with('[') {
            self.at += 1;
            let (header, inline) = Header::parse(first.text, first, 0)?;
            return self.array(&header, inline, first, 1);
        }
        if is_field(first.text) {
            return self.object(0).map(Value::Object);
        }
        self.at += 1;
        if let Some(extra) = self.peek() {
            return Err(fail(extra, 0, "a root value takes one line"));
        }
        primitive(first.text, first, 0)
    }

    /// The fields at `depth`, until the indentation goes back out.
    fn object(&mut self, depth: usize) -> Result<Map<String, Value>, Malformed> {
        let mut object = Map::new();
        while let Some(line) = self.peek() {
            if line.depth < depth {
                break;
            }
            if line.depth > depth {
                return Err(fail(line, 0, "unexpected indentation"));
            }
            self.at += 1;
            let (key, value) = self.field(line.text, line, depth + 1)?;
            object.insert(key, value);
        }
        Ok(object)
    }

    /// One field, whose nested content lives at `child_depth`.
    fn field(
        &mut self,
        text: &str,
        line: Line<'a>,
        child_depth: usize,
    ) -> Result<(String, Value), Malformed> {
        let (key, after) = key(text, line)?;
        let offset = text.len() - after.len();
        if after.starts_with('[') {
            let (header, inline) = Header::parse(after, line, offset)?;
            return Ok((key, self.array(&header, inline, line, child_depth)?));
        }
        let rest = after[1..].trim();
        if rest.is_empty() {
            let nested = match self.peek() {
                Some(next) if next.depth >= child_depth => self.object(child_depth)?,
                _ => Map::new(),
            };
            return Ok((key, Value::Object(nested)));
        }
        Ok((key, primitive(rest, line, text.len() - rest.len())?))
    }

    /// The array a header declares: a table's rows or a list's items at
    /// `item_depth`, or the inline values after the header.
    fn array(
        &mut self,
        header: &Header,
        inline: &str,
        line: Line<'a>,
        item_depth: usize,
    ) -> Result<Value, Malformed> {
        let short = |found: usize| {
            fail(
                line,
                0,
                format!("the array declares {} items, {found} found", header.count),
            )
        };
        if let Some(fields) = &header.fields {
            if !inline.is_empty() {
                return Err(fail(line, 0, "a table's rows go below its header"));
            }
            let mut rows = Vec::with_capacity(header.count);
            for _ in 0..header.count {
                let row = self.peek_at(item_depth).ok_or_else(|| short(rows.len()))?;
                self.at += 1;
                let values = split(row.text, header.delimiter, row)?;
                if values.len() != fields.len() {
                    return Err(fail(
                        row,
                        0,
                        format!(
                            "the row has {} values, the header names {} fields",
                            values.len(),
                            fields.len()
                        ),
                    ));
                }
                rows.push(Value::Object(fields.iter().cloned().zip(values).collect()));
            }
            return self.closed(rows, item_depth);
        }
        if !inline.is_empty() {
            let values = split(inline, header.delimiter, line)?;
            if values.len() != header.count {
                return Err(short(values.len()));
            }
            return Ok(Value::Array(values));
        }
        let mut items = Vec::with_capacity(header.count);
        for _ in 0..header.count {
            let item = self.peek_at(item_depth).ok_or_else(|| short(items.len()))?;
            self.at += 1;
            let body = item
                .text
                .strip_prefix('-')
                .ok_or_else(|| fail(item, 0, "a list item starts with -"))?;
            let offset = item.text.len() - body.trim_start().len();
            items.push(self.item(body.trim_start(), item, offset, item_depth + 1)?);
        }
        self.closed(items, item_depth)
    }

    /// The array is complete; one more line at its items' depth is one too
    /// many.
    fn closed(&self, items: Vec<Value>, item_depth: usize) -> Result<Value, Malformed> {
        match self.peek_at(item_depth) {
            Some(extra) => Err(fail(
                extra,
                0,
                format!("the array declares {} items, more follow", items.len()),
            )),
            None => Ok(Value::Array(items)),
        }
    }

    /// A list item: a nested array, an object whose first field shares the
    /// dash's line and whose others sit at `fields_depth`, or a primitive.
    fn item(
        &mut self,
        body: &str,
        line: Line<'a>,
        offset: usize,
        fields_depth: usize,
    ) -> Result<Value, Malformed> {
        if body.starts_with('[') {
            let (header, inline) = Header::parse(body, line, offset)?;
            return self.array(&header, inline, line, fields_depth);
        }
        if is_field(body) {
            let (key, value) = self.field(body, line, fields_depth + 1)?;
            let mut object = Map::new();
            object.insert(key, value);
            if self.peek().is_some_and(|next| next.depth >= fields_depth) {
                object.extend(self.object(fields_depth)?);
            }
            return Ok(Value::Object(object));
        }
        primitive(body, line, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_table_parses_to_one_object_per_row_with_the_header_fields() {
        let text = "items[2]{id,name,price}:\n  1,Widget,9.5\n  2,\"Gadget, large\",12\ntotal: 2";
        let value = parse(text).expect("toon");
        assert_eq!(
            value,
            json!({
                "items": [
                    {"id": 1, "name": "Widget", "price": 9.5},
                    {"id": 2, "name": "Gadget, large", "price": 12}
                ],
                "total": 2
            })
        );
    }

    #[test]
    fn objects_nest_by_indentation_and_arrays_are_inline_or_listed() {
        let text = "service:\n  name: edge\n  tags[3]: a,b,c\n  empty[0]:\n  none:\nitems[3]:\n  \
                    - 1\n  - name: x\n    kind: y\n  - [2|]: p|q\nnote: \"quoted: yes\"\nflag: true\n\
                    nothing: null\nnegative: -1.5e3";
        let value = parse(text).expect("toon");
        assert_eq!(
            value,
            json!({
                "service": {"name": "edge", "tags": ["a", "b", "c"], "empty": [], "none": {}},
                "items": [1, {"name": "x", "kind": "y"}, ["p", "q"]],
                "note": "quoted: yes",
                "flag": true,
                "nothing": null,
                "negative": -1500.0
            })
        );
        assert_eq!(parse("[#2]: 1,2").expect("root array"), json!([1, 2]));
        assert_eq!(parse("42").expect("root primitive"), json!(42));
        assert_eq!(parse("").expect("empty"), json!({}));
        assert_eq!(
            parse("\"a\\\"b\": \"c\\nd\"").expect("escapes"),
            json!({"a\"b": "c\nd"})
        );
    }

    #[test]
    fn a_ragged_row_is_refused_with_its_line() {
        let refused = parse("items[2]{id,name}:\n  1,Widget\n  2\n").expect_err("ragged");
        assert_eq!(refused.line, 3);
        assert_eq!(refused.column, 3);
        assert!(
            refused
                .message
                .contains("the row has 1 values, the header names 2 fields")
        );
        assert_eq!(
            refused.to_string(),
            format!("line 3 column 3: {}", refused.message)
        );
    }

    #[test]
    fn a_count_that_does_not_match_a_bad_indent_or_a_bad_escape_is_refused() {
        let few = parse("tags[3]: a,b").expect_err("too few");
        assert!(few.message.contains("declares 3 items, 2 found"), "{few}");
        let many = parse("items[1]:\n  - a\n  - b").expect_err("too many");
        assert_eq!(many.line, 3);
        let odd = parse("a:\n   b: 1").expect_err("odd indent");
        assert_eq!((odd.line, odd.column), (2, 4));
        let tab = parse("a:\n\tb: 1").expect_err("tab");
        assert_eq!(tab.line, 2);
        let escape = parse("a: \"\\q\"").expect_err("escape");
        assert!(escape.message.contains("unknown escape"), "{escape}");
        let stray = parse("a: 1\n  b: 2").expect_err("stray indent");
        assert_eq!((stray.line, stray.column), (2, 3));
        let open = parse("a: \"never").expect_err("open quote");
        assert_eq!((open.line, open.column), (1, 4));
    }

    #[test]
    fn numbers_follow_the_json_grammar_and_the_rest_is_text() {
        for (text, expected) in [
            ("1", json!(1)),
            ("-0", json!(0)),
            ("1.25", json!(1.25)),
            ("1e2", json!(100.0)),
            (
                "18446744073709551615",
                json!(18_446_744_073_709_551_615_u64),
            ),
            ("01", json!("01")),
            ("1.", json!("1.")),
            ("0x10", json!("0x10")),
            ("hello world", json!("hello world")),
        ] {
            assert_eq!(
                parse(&format!("v: {text}")).expect("toon")["v"],
                expected,
                "{text}"
            );
        }
    }
}
