//! Strict decoding of a reviewer's reply (spec §5): the whole payload is decoded against its
//! declared shape; only a surrounding ```json fence (with prose around it) is stripped.

use anyhow::{Result, bail};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// The JSON a reply carries: the whole trimmed reply when it starts with `{` or `[`, else the
/// text between its first ``` fence (an optional `json` tag cut) and its last ```.
fn payload(raw: &str) -> &str {
    let t = raw.trim();
    if t.starts_with(['{', '[']) {
        return t;
    }
    // The last ```: a body may itself hold a ``` block.
    if let Some(s) = t.find("```") {
        let rest = &t[s + 3..];
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        if let Some(e) = rest.rfind("```") {
            return rest[..e].trim();
        }
    }
    t
}

/// Decodes `raw` as a `T` whose top level is a JSON object (`array` false) or array. `objects`
/// names the places that must hold JSON objects, as `/`-separated keys with `*` for every
/// array item (`findings/*/code_location`): serde's derived decoder also takes a struct as a
/// positional array, which only a check on the parsed value catches. The typed decode then
/// reads the original text, so a repeated key is still an error (a `Value` keeps the last).
pub fn decode<T: DeserializeOwned>(raw: &str, array: bool, objects: &[&str]) -> Result<T> {
    let body = payload(raw);
    let want = if array {
        "a JSON array"
    } else {
        "a JSON object"
    };
    if body.is_empty() {
        bail!("the reply is empty; want {want}");
    }
    let value: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) if body.starts_with(['{', '[']) => bail!("malformed JSON: {e}"),
        Err(_) => bail!("no JSON in the reply: want {want}, bare or in one ```json fence"),
    };
    if value.is_array() != array || !(value.is_array() || value.is_object()) {
        bail!("the reply is not {want}");
    }
    for path in objects {
        let keys: Vec<_> = path.split('/').collect();
        if let Some(at) = not_object(&value, &keys, String::new()) {
            bail!("{at} is not a JSON object");
        }
    }
    Ok(serde_json::from_str(body)?)
}

/// The first place along `keys` that holds something other than an object; a missing key is
/// left to the typed decode.
fn not_object(v: &Value, keys: &[&str], at: String) -> Option<String> {
    let Some((key, rest)) = keys.split_first() else {
        return (!v.is_object()).then_some(at);
    };
    if *key == "*" {
        let items = v.as_array()?;
        return items
            .iter()
            .enumerate()
            .find_map(|(i, item)| not_object(item, rest, format!("{at}[{i}]")));
    }
    let next = v.as_object()?.get(*key)?;
    not_object(next, rest, format!("{at}/{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Pair {
        a: u8,
        b: String,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Outer {
        pairs: Vec<Pair>,
    }

    fn outer(raw: &str) -> Result<Outer> {
        decode(raw, false, &["pairs/*"])
    }

    #[test]
    fn takes_a_bare_or_fenced_payload_only() {
        assert!(outer(r#"{"pairs": [{"a": 1, "b": "```x```"}]}"#).is_ok());
        assert!(outer("Done.\n```json\n{\"pairs\": []}\n```\nBye.").is_ok());
        assert!(outer("```\n{\"pairs\": []}\n```").is_ok());
        for (raw, want) in [
            ("", "the reply is empty"),
            ("I found nothing.", "no JSON in the reply"),
            ("Here: {\"pairs\": []}", "no JSON in the reply"),
            ("[{\"pairs\": []}]", "the reply is not a JSON object"),
            ("{\"pairs\": [] ", "malformed JSON"),
            (
                "{\"pairs\": [[1, \"x\"]]}",
                "/pairs[0] is not a JSON object",
            ),
            (r#"{"pairs": [], "pairs": []}"#, "duplicate field `pairs`"),
            (r#"{"pairs": [], "extra": 1}"#, "unknown field `extra`"),
        ] {
            let e = format!("{:#}", outer(raw).unwrap_err());
            assert!(e.contains(want), "{raw:?}: {e}");
        }
        let e = decode::<Vec<Pair>>(r#"{"unexpected": [{"a": 1, "b": ""}]}"#, true, &["*"]);
        assert!(format!("{:#}", e.unwrap_err()).contains("not a JSON array"));
        assert!(decode::<Vec<Pair>>(r#"[{"a": 1, "b": ""}]"#, true, &["*"]).is_ok());
    }
}
