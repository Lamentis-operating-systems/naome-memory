use std::collections::BTreeSet;

use serde_json::Value;

pub fn validate(instance: &Value, schema: &Value) -> Result<(), String> {
    validate_at(instance, schema, schema, "$")
}

fn validate_at(instance: &Value, schema: &Value, root: &Value, path: &str) -> Result<(), String> {
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: schema node is not an object"))?;
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let target = resolve_reference(root, reference)?;
        validate_at(instance, target, root, path)?;
    }
    if let Some(expected) = object.get("const")
        && instance != expected
    {
        return Err(format!("{path}: const mismatch"));
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array)
        && !values.contains(instance)
    {
        return Err(format!("{path}: value is outside enum"));
    }
    if let Some(branches) = object.get("oneOf").and_then(Value::as_array) {
        let matches = branches
            .iter()
            .filter(|branch| validate_at(instance, branch, root, path).is_ok())
            .count();
        if matches != 1 {
            return Err(format!("{path}: oneOf matched {matches} branches"));
        }
    }
    match object.get("type").and_then(Value::as_str) {
        Some("object") => validate_object(instance, schema, root, path)?,
        Some("array") => validate_array(instance, schema, root, path)?,
        Some("string") => validate_string(instance, schema, path)?,
        Some("integer") => validate_integer(instance, schema, path)?,
        Some("boolean") if !instance.is_boolean() => {
            return Err(format!("{path}: expected boolean"));
        }
        Some("null") if !instance.is_null() => return Err(format!("{path}: expected null")),
        Some("boolean" | "null") | None => {}
        Some(other) => return Err(format!("{path}: unsupported schema type {other}")),
    }
    Ok(())
}

fn validate_object(
    instance: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
) -> Result<(), String> {
    let values = instance
        .as_object()
        .ok_or_else(|| format!("{path}: expected object"))?;
    let schema_object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: invalid object schema"))?;
    if let Some(minimum) = schema_object.get("minProperties").and_then(Value::as_u64)
        && u64::try_from(values.len()).unwrap_or(u64::MAX) < minimum
    {
        return Err(format!("{path}: too few properties"));
    }
    if let Some(required) = schema_object.get("required").and_then(Value::as_array) {
        for key in required.iter().filter_map(Value::as_str) {
            if !values.contains_key(key) {
                return Err(format!("{path}: missing required property {key}"));
            }
        }
    }
    let properties = schema_object.get("properties").and_then(Value::as_object);
    for (key, value) in values {
        if let Some(property_schema) = properties.and_then(|known| known.get(key)) {
            validate_at(value, property_schema, root, &format!("{path}.{key}"))?;
            continue;
        }
        match schema_object.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                return Err(format!("{path}: unexpected property {key}"));
            }
            Some(additional @ Value::Object(_)) => {
                validate_at(value, additional, root, &format!("{path}.{key}"))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_array(
    instance: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
) -> Result<(), String> {
    let values = instance
        .as_array()
        .ok_or_else(|| format!("{path}: expected array"))?;
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: invalid array schema"))?;
    let length = u64::try_from(values.len()).unwrap_or(u64::MAX);
    if object
        .get("minItems")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| length < minimum)
        || object
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
    {
        return Err(format!("{path}: array length is outside bounds"));
    }
    if object.get("uniqueItems") == Some(&Value::Bool(true)) {
        let distinct = values.iter().map(Value::to_string).collect::<BTreeSet<_>>();
        if distinct.len() != values.len() {
            return Err(format!("{path}: array values are not unique"));
        }
    }
    if let Some(item_schema) = object.get("items") {
        for (index, value) in values.iter().enumerate() {
            validate_at(value, item_schema, root, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_string(instance: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let value = instance
        .as_str()
        .ok_or_else(|| format!("{path}: expected string"))?;
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: invalid string schema"))?;
    if object
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| u64::try_from(value.chars().count()).unwrap_or(u64::MAX) < minimum)
    {
        return Err(format!("{path}: string is too short"));
    }
    if let Some(pattern) = object.get("pattern").and_then(Value::as_str)
        && !known_pattern_matches(pattern, value)
    {
        return Err(format!("{path}: string does not match {pattern}"));
    }
    Ok(())
}

fn validate_integer(instance: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let value = instance
        .as_i64()
        .map(i128::from)
        .or_else(|| instance.as_u64().map(i128::from))
        .ok_or_else(|| format!("{path}: expected integer"))?;
    let object = schema
        .as_object()
        .ok_or_else(|| format!("{path}: invalid integer schema"))?;
    if object
        .get("minimum")
        .and_then(Value::as_i64)
        .is_some_and(|minimum| value < i128::from(minimum))
        || object
            .get("maximum")
            .and_then(Value::as_i64)
            .is_some_and(|maximum| value > i128::from(maximum))
    {
        return Err(format!("{path}: integer is outside bounds"));
    }
    Ok(())
}

fn known_pattern_matches(pattern: &str, value: &str) -> bool {
    match pattern {
        "^[0-9a-f]{64}$" => {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        "^(calibration|holdout)-v1/world-[0-9]{4}$" => {
            let suffix = value
                .strip_prefix("calibration-v1/world-")
                .or_else(|| value.strip_prefix("holdout-v1/world-"));
            suffix.is_some_and(|digits| {
                digits.len() == 4 && digits.bytes().all(|byte| byte.is_ascii_digit())
            })
        }
        "^golden-v1/lottery-[0-9]{2}$" => {
            value
                .strip_prefix("golden-v1/lottery-")
                .is_some_and(|digits| {
                    digits.len() == 2 && digits.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        _ => false,
    }
}

fn resolve_reference<'a>(root: &'a Value, reference: &str) -> Result<&'a Value, String> {
    let pointer = reference
        .strip_prefix('#')
        .ok_or_else(|| format!("external schema reference is not supported: {reference}"))?;
    root.pointer(pointer)
        .ok_or_else(|| format!("schema reference does not resolve: {reference}"))
}
