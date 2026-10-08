//! Hand-written JSON schemas for structured output.
//!
//! They follow the strict subset both CLIs accept (Codex's `--output-schema`
//! uses OpenAI strict mode): every object lists all its properties as
//! required, sets `additionalProperties: false`, and optional values are
//! nullable instead of omitted.

use serde_json::{Value, json};

fn obj(properties: Value) -> Value {
    let required: Vec<String> = properties
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    json!({ "type": "object", "additionalProperties": false, "properties": properties, "required": required })
}

fn string() -> Value {
    json!({ "type": "string" })
}

fn nullable_string() -> Value {
    json!({ "type": ["string", "null"] })
}

fn strings() -> Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

pub const VISUAL_KINDS: [&str; 7] = [
    "youtube", "article", "meme", "gif", "image", "stock", "text",
];
pub const SFX_KINDS: [&str; 11] = [
    "whoosh",
    "swoosh",
    "pop",
    "click",
    "ding",
    "riser",
    "hit",
    "typewriter",
    "glitch",
    "camera",
    "record_scratch",
];

pub fn research() -> Value {
    obj(json!({
        "web_search_ok": { "type": "boolean" },
        "summary": string(),
        "facts": { "type": "array", "items": obj(json!({
            "text": string(),
            "source_url": nullable_string(),
            "source_title": nullable_string(),
        })) },
        "sources": { "type": "array", "items": obj(json!({
            "title": string(),
            "url": string(),
            "snippet": string(),
        })) },
        "angles": strings(),
        "keywords": strings(),
    }))
}

pub fn hooks() -> Value {
    obj(json!({
        "hooks": { "type": "array", "items": obj(json!({
            "text": string(),
            "angle": string(),
            "score": { "type": "number" },
            "why": string(),
        })) },
    }))
}

pub fn script() -> Value {
    obj(json!({
        "text": string(),
        "title": string(),
        "description": string(),
        "hashtags": strings(),
    }))
}

pub fn text_only() -> Value {
    obj(json!({ "text": string() }))
}

pub fn unslop() -> Value {
    obj(json!({
        "text": string(),
        "findings": {"type": "array", "items": obj(json!({
            "source": string(), "replacement": string(), "reason": string(),
        }))},
        "protections": {"type": "array", "items": obj(json!({
            "source": string(), "reason": string(),
        }))},
    }))
}

pub fn meme_plan() -> Value {
    obj(json!({
        "template": string(),
        "caption": string(),
        "top_text": string(),
        "bottom_text": string(),
    }))
}

pub fn visual_plan() -> Value {
    let mut sfx: Vec<Value> = SFX_KINDS.iter().map(|s| json!(s)).collect();
    sfx.push(Value::Null);
    obj(json!({
        "kind": { "type": "string", "enum": VISUAL_KINDS },
        "query": string(),
        "alt_queries": strings(),
        "on_screen_text": nullable_string(),
        "icon": nullable_string(),
        "meme": { "anyOf": [meme_plan(), { "type": "null" }] },
        "quote": nullable_string(),
        "sfx": { "type": ["string", "null"], "enum": sfx },
    }))
}

pub fn beats() -> Value {
    obj(json!({
        "beats": { "type": "array", "items": obj(json!({
            "text": string(),
            "visual": visual_plan(),
        })) },
    }))
}

pub fn moment() -> Value {
    obj(json!({
        "found": { "type": "boolean" },
        "start": { "type": ["number", "null"] },
        "end": { "type": ["number", "null"] },
    }))
}

pub fn meme(templates: &[String]) -> Value {
    let template = if !templates.is_empty() && templates.len() <= 500 {
        json!({ "type": "string", "enum": templates })
    } else {
        string()
    };
    obj(json!({
        "template": template,
        "caption": string(),
        "top_text": string(),
        "bottom_text": string(),
    }))
}

pub fn candidate() -> Value {
    obj(json!({ "index": { "type": ["integer", "null"] } }))
}

/// Validate the strict subset emitted by this module, rather than relying on
/// core's permissive/defaulted serde models. Unknown schema constructs fail
/// closed. Every stage uses the same validation on its first and retry attempts.
pub fn validate(value: &Value, schema: &Value) -> Result<(), String> {
    fn check(value: &Value, schema: &Value, path: &str, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err(format!("{path}: structure is too deeply nested"));
        }
        if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
            return if options
                .iter()
                .any(|s| check(value, s, path, depth + 1).is_ok())
            {
                Ok(())
            } else {
                Err(format!("{path}: value does not match any allowed shape"))
            };
        }
        let matches_type = |kind: &str| match kind {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        let valid_type = match schema.get("type") {
            Some(Value::String(t)) => matches_type(t),
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).any(matches_type),
            _ => false,
        };
        if !valid_type {
            return Err(format!("{path}: incorrect or missing value type"));
        }
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
            && !allowed.contains(value)
        {
            return Err(format!("{path}: value is not in the allowed list"));
        }
        if let Some(object) = value.as_object() {
            let props = schema
                .get("properties")
                .and_then(Value::as_object)
                .ok_or_else(|| format!("{path}: missing object schema"))?;
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{path}: missing required fields"))?;
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(format!("{path}.{name}: required field is missing"));
                }
            }
            if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
                return Err(format!("{path}: non-strict object schema"));
            }
            for (name, item) in object {
                let child = props
                    .get(name)
                    .ok_or_else(|| format!("{path}.{name}: unexpected field"))?;
                check(item, child, &format!("{path}.{name}"), depth + 1)?;
            }
        }
        if let Some(items) = value.as_array() {
            if items.len() > 1000 {
                return Err(format!("{path}: too many items"));
            }
            let item_schema = schema
                .get("items")
                .ok_or_else(|| format!("{path}: missing array schema"))?;
            for (i, item) in items.iter().enumerate() {
                check(item, item_schema, &format!("{path}[{i}]"), depth + 1)?;
            }
        }
        Ok(())
    }
    check(value, schema, "$", 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipcut_core::{SfxKind, VisualKind, VisualPlan};

    /// Every object must be strict: all properties required, no extras.
    fn assert_strict(v: &Value) {
        match v {
            Value::Object(m) => {
                if m.get("type") == Some(&json!("object")) {
                    assert_eq!(m.get("additionalProperties"), Some(&json!(false)));
                    let props: Vec<&String> = m["properties"].as_object().unwrap().keys().collect();
                    let req: Vec<&str> = m["required"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|r| r.as_str().unwrap())
                        .collect();
                    for p in props {
                        assert!(req.contains(&p.as_str()), "{p} not required");
                    }
                }
                m.values().for_each(assert_strict);
            }
            Value::Array(a) => a.iter().for_each(assert_strict),
            _ => {}
        }
    }

    #[test]
    fn schemas_are_strict() {
        for s in [
            research(),
            hooks(),
            script(),
            text_only(),
            unslop(),
            beats(),
            moment(),
            meme(&["Drake".into()]),
            candidate(),
        ] {
            assert_strict(&s);
        }
    }

    #[test]
    fn runtime_validation_rejects_defaults_extras_and_wrong_types() {
        assert!(validate(&json!({"hooks":[{"text":"Hook"}]}), &hooks()).is_err());
        assert!(validate(&json!({"text":"x","debug":true}), &text_only()).is_err());
        assert!(validate(&json!({"index":1.2}), &candidate()).is_err());
        assert!(validate(&json!({"index":null}), &candidate()).is_ok());
        assert!(validate(&json!({"found":false,"start":null,"end":null}), &moment()).is_ok());
        assert!(validate(&json!({"found":false}), &moment()).is_err());
    }

    #[test]
    fn enums_match_core_serde_names() {
        for k in VISUAL_KINDS {
            serde_json::from_value::<VisualKind>(json!(k)).unwrap();
        }
        for k in SFX_KINDS {
            serde_json::from_value::<SfxKind>(json!(k)).unwrap();
        }
        // A schema-shaped visual deserializes into the core type.
        let v: VisualPlan = serde_json::from_value(json!({
            "kind": "meme", "query": "Two Buttons", "alt_queries": [], "on_screen_text": null, "icon": null,
            "meme": {"template": "Two Buttons", "caption": "swim or crawl", "top_text": "", "bottom_text": ""},
            "quote": null, "sfx": "record_scratch"
        }))
        .unwrap();
        assert_eq!(v.kind, VisualKind::Meme);
        assert_eq!(v.sfx, Some(SfxKind::RecordScratch));
    }
}
