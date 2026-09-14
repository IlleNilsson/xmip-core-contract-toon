#![forbid(unsafe_code)]

//! The TOON content contract — a technology of `xmip-core-contract`.
//!
//! Two claims (ADR-0042): **well-formedness is a given** — every Stream is
//! read as Token-Oriented Object Notation and a failure names its line and
//! column — and **conformance is a given once the contract is named** — a
//! Receive or Send Location that refers to this contract with a JSON Schema
//! bound has every Stream held to it. TOON is a notation for the JSON data
//! model, so the schema is the one the `json-schema` contract reads, and that
//! sibling evaluates it against the value [`toon`] parses; this crate owns the
//! notation and nothing of the schema language (ADR-0044).

mod token;
pub mod toon;

use contract::{
    Contract, ContractDescriptor, ContractError, ContractFactory, ContractId, ValidationIssue,
    ValidationResult,
};
use contract_json_schema::schema;
use serde_json::Value;
use stream::Stream;

pub use toon::{Malformed, parse};

const REPRESENTATION: &str = "application/toon";

/// The TOON contract, bare or bound to a JSON Schema.
pub struct Toon {
    descriptor: ContractDescriptor,
    schema: Option<Value>,
}

impl Toon {
    /// Well-formedness only.
    #[must_use]
    pub fn new() -> Self {
        Self {
            descriptor: descriptor("toon"),
            schema: None,
        }
    }

    /// Well-formedness and conformance to `schema`.
    ///
    /// # Errors
    /// The schema must itself be a JSON Schema: an object or a boolean.
    pub fn with_schema(schema: Value) -> Result<Self, ContractError> {
        if !(schema.is_object() || schema.is_boolean()) {
            return Err(ContractError {
                message: "a JSON Schema is an object or a boolean".to_string(),
            });
        }
        let name = schema
            .get("$id")
            .or_else(|| schema.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("bound");
        Ok(Self {
            descriptor: descriptor(&format!("toon:{name}")),
            schema: Some(schema),
        })
    }

    /// Whether a schema is bound.
    #[must_use]
    pub const fn is_bound(&self) -> bool {
        self.schema.is_some()
    }
}

impl Default for Toon {
    fn default() -> Self {
        Self::new()
    }
}

fn descriptor(id: &str) -> ContractDescriptor {
    ContractDescriptor {
        id: ContractId(id.to_string()),
        version: "1".to_string(),
        representation: REPRESENTATION.to_string(),
    }
}

impl Contract for Toon {
    fn descriptor(&self) -> &ContractDescriptor {
        &self.descriptor
    }

    fn identify(&self, stream: &Stream) -> Result<bool, ContractError> {
        if let Some(media_type) = stream.media_type() {
            return Ok(is_toon_media_type(media_type));
        }
        Ok(std::str::from_utf8(stream.bytes()).is_ok_and(looks_like_toon))
    }

    fn validate(&self, stream: &Stream) -> Result<ValidationResult, ContractError> {
        let text = std::str::from_utf8(stream.bytes()).map_err(|error| ContractError {
            message: format!("not UTF-8 text: {error}"),
        })?;
        let instance = match parse(text) {
            Ok(instance) => instance,
            Err(malformed) => {
                return Ok(ValidationResult::of(vec![ValidationIssue::at(
                    "malformed",
                    &format!("not valid TOON: {}", malformed.message),
                    &format!("line {} column {}", malformed.line, malformed.column),
                )]));
            }
        };
        let issues = match &self.schema {
            Some(bound) => schema::check(bound, bound, &instance, ""),
            None => Vec::new(),
        };
        Ok(ValidationResult::of(issues))
    }
}

fn is_toon_media_type(media_type: &str) -> bool {
    let essence = media_type.split(';').next().unwrap_or("").trim();
    essence.eq_ignore_ascii_case("application/toon")
        || essence.eq_ignore_ascii_case("text/toon")
        || essence.ends_with("+toon")
}

/// The first non-blank line is an array header, `key[N]...:` or `[N]...:`,
/// which nothing else writes. A plain `key: value` document is YAML's shape
/// as much as TOON's and is not claimed by sniffing.
fn looks_like_toon(text: &str) -> bool {
    let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return false;
    };
    let Some(open) = line.find('[') else {
        return false;
    };
    let Some(close) = line[open..].find(']') else {
        return false;
    };
    let count = line[open + 1..open + close].trim_start_matches('#');
    let count = count.trim_end_matches(['\t', '|']);
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let after = &line[open + close + 1..];
    let after = after
        .strip_prefix('{')
        .and_then(|fields| fields.split_once('}'))
        .map_or(after, |(_, rest)| rest);
    after.starts_with(':')
}

/// Loads the contract a Location names: `toon` or an empty reference is the
/// bare contract, anything else is the path of a JSON Schema file.
pub struct ToonFactory;

impl ContractFactory for ToonFactory {
    fn technology(&self) -> &'static str {
        "toon"
    }

    fn load(&self, reference: &str) -> Result<Box<dyn Contract>, ContractError> {
        let reference = reference.trim();
        if reference.is_empty() || reference == self.technology() {
            return Ok(Box::new(Toon::new()));
        }
        let bytes = std::fs::read(reference).map_err(|error| ContractError {
            message: format!("cannot read schema {reference}: {error}"),
        })?;
        let schema = serde_json::from_slice::<Value>(&bytes).map_err(|error| ContractError {
            message: format!("schema {reference} is not valid JSON: {error}"),
        })?;
        Ok(Box::new(Toon::with_schema(schema)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::fixture::stream_as as stream;
    use serde_json::json;
    use xcore::StreamId;

    fn order_schema() -> Value {
        json!({
            "$id": "order",
            "type": "object",
            "required": ["id", "lines"],
            "properties": {
                "id": { "type": "string", "minLength": 1 },
                "lines": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["sku", "qty"],
                        "properties": {
                            "sku": { "type": "string" },
                            "qty": { "type": "integer", "minimum": 1 }
                        }
                    }
                }
            },
            "additionalProperties": false
        })
    }

    const ORDER: &str = "id: A1\nlines[2]{sku,qty}:\n  X,2\n  Y,1";

    #[test]
    fn the_bare_contract_holds_well_formed_toon_and_names_where_it_breaks() {
        let bare = Toon::new();
        assert!(!bare.is_bound());
        assert_eq!(bare.descriptor().id.0, "toon");
        let held = bare.validate(&stream(ORDER, None)).expect("validates");
        assert!(held.valid, "{:?}", held.issues);
        let ragged = bare
            .validate(&stream("lines[2]{sku,qty}:\n  X,2\n  Y", None))
            .expect("validates");
        assert!(!ragged.valid);
        assert_eq!(ragged.issues[0].code, "malformed");
        assert_eq!(ragged.issues[0].path.as_deref(), Some("line 3 column 3"));
        assert!(ragged.issues[0].message.contains("the row has 1 values"));
        let bytes = Stream::new(StreamId::new(2), vec![0xff, 0xfe], None);
        assert!(
            bare.validate(&bytes).is_err(),
            "not text is an error, not an issue"
        );
    }

    #[test]
    fn the_bound_contract_holds_a_conforming_order_and_names_every_departure() {
        let bound = Toon::with_schema(order_schema()).expect("a schema");
        assert!(bound.is_bound());
        assert_eq!(bound.descriptor().id.0, "toon:order");
        let held = bound.validate(&stream(ORDER, None)).expect("validates");
        assert!(held.valid, "{:?}", held.issues);
        let departed = bound
            .validate(&stream(
                "id: \"\"\nlines[1]{sku,qty}:\n  1,0\nextra: true",
                None,
            ))
            .expect("validates");
        assert!(!departed.valid);
        let paths: Vec<&str> = departed
            .issues
            .iter()
            .filter_map(|issue| issue.path.as_deref())
            .collect();
        assert!(paths.contains(&"/id"), "{paths:?}");
        assert!(paths.contains(&"/lines/0/sku"), "{paths:?}");
        assert!(paths.contains(&"/lines/0/qty"), "{paths:?}");
        assert!(paths.contains(&"/extra"), "{paths:?}");
        assert!(Toon::with_schema(json!("no")).is_err());
    }

    #[test]
    fn identifies_by_media_type_or_by_an_array_header() {
        let bare = Toon::new();
        let is = |text: &str, media: Option<&str>| bare.identify(&stream(text, media)).expect("ok");
        assert!(is("x", Some("application/toon")));
        assert!(is("x", Some("text/toon; charset=utf-8")));
        assert!(!is("a[1]: b", Some("application/yaml")));
        assert!(is("tags[3]: a,b,c", None));
        assert!(is("  items[#2|]{id|name}:", None));
        assert!(is("[2]: 1,2", None));
        assert!(!is("name: a", None));
        assert!(!is("a[b]: c", None));
        assert!(!is("[1, 2]", None));
        assert!(!is("", None));
    }

    #[test]
    fn the_factory_loads_bare_and_bound() {
        let factory = ToonFactory;
        assert_eq!(factory.technology(), "toon");
        assert_eq!(factory.load("").expect("bare").descriptor().id.0, "toon");
        assert_eq!(
            factory.load("toon").expect("bare").descriptor().id.0,
            "toon"
        );
        let dir = std::env::temp_dir().join("xmip-contract-toon-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("order.schema.json");
        std::fs::write(&file, order_schema().to_string()).expect("write schema");
        let bound = factory.load(file.to_str().expect("path")).expect("bound");
        assert_eq!(bound.descriptor().id.0, "toon:order");
        let held = bound.validate(&stream(ORDER, None)).expect("validates");
        assert!(held.valid, "{:?}", held.issues);
        assert!(
            factory
                .load(dir.join("missing.json").to_str().expect("path"))
                .is_err()
        );
    }
}
