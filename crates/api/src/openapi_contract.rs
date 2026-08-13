#[cfg(test)]
use std::collections::BTreeSet;

use serde_json::{Map, Value, json};

mod success;

const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];
const ERROR_RESPONSE_REF: &str = "#/components/schemas/ErrorResponse";

pub(crate) fn finalize(document: &mut Value) {
    register_components(document);
    finalize_operations(document);
}

fn register_components(document: &mut Value) {
    let components = document
        .get_mut("components")
        .and_then(Value::as_object_mut)
        .expect("static OpenAPI components object");
    components
        .entry("headers")
        .or_insert_with(|| Value::Object(Map::new()));
    let headers = components["headers"]
        .as_object_mut()
        .expect("static OpenAPI headers object");
    headers.insert(
        "XRequestId".to_owned(),
        json!({
            "description": "Opaque request correlation identifier assigned or propagated by Asterism.",
            "schema": {"type": "string", "format": "uuid"}
        }),
    );
    headers.insert(
        "RetryAfter".to_owned(),
        json!({
            "description": "Minimum number of seconds before this operation should be retried.",
            "schema": {"type": "integer", "minimum": 0}
        }),
    );

    let schemas = components
        .get_mut("schemas")
        .and_then(Value::as_object_mut)
        .expect("static OpenAPI schemas object");
    schemas.insert(
        "ErrorBody".to_owned(),
        json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": {"type": "string", "minLength": 1},
                "message": {"type": "string", "minLength": 1}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ErrorResponse".to_owned(),
        json!({
            "type": "object",
            "required": ["error"],
            "properties": {
                "error": {"$ref": "#/components/schemas/ErrorBody"}
            },
            "additionalProperties": false
        }),
    );
    success::register(schemas);
}

fn finalize_operations(document: &mut Value) {
    let paths = document
        .get_mut("paths")
        .and_then(Value::as_object_mut)
        .expect("static OpenAPI paths object");
    for path_item in paths.values_mut().filter_map(Value::as_object_mut) {
        for method in HTTP_METHODS {
            let Some(operation) = path_item.get_mut(method).and_then(Value::as_object_mut) else {
                continue;
            };
            let operation_id = operation.get("operationId").and_then(Value::as_str);
            let success_schema = operation_id.and_then(success::schema_for);
            let is_execution_stream = operation_id == Some("streamExecution");
            let responses = operation
                .get_mut("responses")
                .and_then(Value::as_object_mut)
                .expect("every static OpenAPI operation has responses");
            for (status, response) in responses {
                let response = response
                    .as_object_mut()
                    .expect("every static OpenAPI response is an object");
                let response_headers = response
                    .entry("headers")
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .expect("static OpenAPI response headers object");
                response_headers.insert(
                    "X-Request-ID".to_owned(),
                    json!({"$ref": "#/components/headers/XRequestId"}),
                );
                if status == "429" {
                    response_headers.insert(
                        "Retry-After".to_owned(),
                        json!({"$ref": "#/components/headers/RetryAfter"}),
                    );
                }
                if status.starts_with('4') || status.starts_with('5') {
                    response.insert(
                        "content".to_owned(),
                        json!({
                            "application/json": {
                                "schema": {"$ref": ERROR_RESPONSE_REF}
                            }
                        }),
                    );
                } else if status.starts_with('2')
                    && status != "204"
                    && let Some(schema) = success_schema
                {
                    response.insert(
                        "content".to_owned(),
                        json!({
                            "application/json": {
                                "schema": {"$ref": format!("#/components/schemas/{schema}")}
                            }
                        }),
                    );
                } else if status.starts_with('2') && status != "204" && is_execution_stream {
                    response.insert(
                        "content".to_owned(),
                        json!({
                            "text/event-stream": {
                                "schema": {"type": "string"}
                            }
                        }),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn validate(document: &Value) -> Result<(), Vec<String>> {
    let mut failures = Vec::new();
    if document.get("openapi").and_then(Value::as_str) != Some("3.1.0") {
        failures.push("document must declare OpenAPI 3.1.0".to_owned());
    }
    let Some(paths) = document.get("paths").and_then(Value::as_object) else {
        return Err(vec!["document paths must be an object".to_owned()]);
    };
    let mut operation_ids = BTreeSet::new();
    for (path, path_item) in paths {
        let Some(path_item) = path_item.as_object() else {
            failures.push(format!("path item {path} must be an object"));
            continue;
        };
        for method in HTTP_METHODS {
            let Some(operation) = path_item.get(method).and_then(Value::as_object) else {
                continue;
            };
            let label = format!("{method} {path}");
            match operation.get("operationId").and_then(Value::as_str) {
                Some(operation_id) if operation_ids.insert(operation_id.to_owned()) => {}
                Some(operation_id) => {
                    failures.push(format!("duplicate operationId {operation_id}"));
                }
                None => failures.push(format!("{label} has no operationId")),
            }
            validate_path_parameters(path, operation, &label, &mut failures);
            validate_request_body(operation, &label, &mut failures);
            validate_responses(operation, &label, &mut failures);
        }
    }
    validate_references(document, document, "#", &mut failures);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

#[cfg(test)]
fn validate_path_parameters(
    path: &str,
    operation: &Map<String, Value>,
    label: &str,
    failures: &mut Vec<String>,
) {
    let expected = path
        .split('{')
        .skip(1)
        .filter_map(|part| part.split_once('}').map(|(name, _)| name))
        .collect::<BTreeSet<_>>();
    let actual = operation
        .get("parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter(|parameter| parameter.get("in").and_then(Value::as_str) == Some("path"))
        .filter_map(|parameter| {
            if parameter.get("required").and_then(Value::as_bool) != Some(true) {
                failures.push(format!("{label} has an optional path parameter"));
            }
            parameter.get("name").and_then(Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    if expected != actual {
        failures.push(format!(
            "{label} path parameters differ: expected {expected:?}, got {actual:?}"
        ));
    }
}

#[cfg(test)]
fn validate_request_body(operation: &Map<String, Value>, label: &str, failures: &mut Vec<String>) {
    let Some(body) = operation.get("requestBody").and_then(Value::as_object) else {
        return;
    };
    if body.get("required").and_then(Value::as_bool) != Some(true) {
        failures.push(format!("{label} requestBody must be required"));
    }
    if nested_value(body, &["content", "application/json", "schema"]).is_none() {
        failures.push(format!("{label} JSON requestBody has no schema"));
    }
}

#[cfg(test)]
fn validate_responses(operation: &Map<String, Value>, label: &str, failures: &mut Vec<String>) {
    let Some(responses) = operation.get("responses").and_then(Value::as_object) else {
        failures.push(format!("{label} has no responses"));
        return;
    };
    if responses.is_empty() {
        failures.push(format!("{label} has no response entries"));
    }
    for (status, response) in responses {
        let Some(response) = response.as_object() else {
            failures.push(format!("{label} response {status} must be an object"));
            continue;
        };
        if response
            .get("description")
            .and_then(Value::as_str)
            .is_none()
        {
            failures.push(format!("{label} response {status} has no description"));
        }
        if nested_value(response, &["headers", "X-Request-ID", "$ref"]).and_then(Value::as_str)
            != Some("#/components/headers/XRequestId")
        {
            failures.push(format!(
                "{label} response {status} has no request ID header"
            ));
        }
        if (status.starts_with('4') || status.starts_with('5'))
            && nested_value(response, &["content", "application/json", "schema", "$ref"])
                .and_then(Value::as_str)
                != Some(ERROR_RESPONSE_REF)
        {
            failures.push(format!(
                "{label} error response {status} does not use ErrorResponse"
            ));
        }
        if status == "429"
            && nested_value(response, &["headers", "Retry-After", "$ref"]).and_then(Value::as_str)
                != Some("#/components/headers/RetryAfter")
        {
            failures.push(format!("{label} response 429 has no Retry-After header"));
        }
        if status == "204" && response.contains_key("content") {
            failures.push(format!("{label} response 204 must not declare content"));
        }
        if status.starts_with('2') && status != "204" {
            let operation_id = operation.get("operationId").and_then(Value::as_str);
            if let Some(expected_schema) = operation_id.and_then(success::schema_for) {
                let expected_reference = format!("#/components/schemas/{expected_schema}");
                if nested_value(response, &["content", "application/json", "schema", "$ref"])
                    .and_then(Value::as_str)
                    != Some(expected_reference.as_str())
                {
                    failures.push(format!(
                        "{label} response {status} does not use {expected_schema}"
                    ));
                }
            } else if operation_id == Some("streamExecution") {
                if nested_value(
                    response,
                    &["content", "text/event-stream", "schema", "type"],
                )
                .and_then(Value::as_str)
                    != Some("string")
                {
                    failures.push(format!(
                        "{label} response {status} does not declare text/event-stream"
                    ));
                }
            } else {
                failures.push(format!(
                    "{label} response {status} has no typed success content"
                ));
            }
        }
    }
}

#[cfg(test)]
fn nested_value<'a>(object: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut value = object.get(*first)?;
    for key in rest {
        value = value.as_object()?.get(*key)?;
    }
    Some(value)
}

#[cfg(test)]
fn validate_references(root: &Value, value: &Value, location: &str, failures: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && (reference
                    .strip_prefix('#')
                    .and_then(|pointer| root.pointer(pointer)))
                .is_none()
            {
                failures.push(format!("{location} has unresolved reference {reference}"));
            }
            for (key, child) in object {
                validate_references(root, child, &format!("{location}/{key}"), failures);
            }
        }
        Value::Array(array) => {
            for (index, child) in array.iter().enumerate() {
                validate_references(root, child, &format!("{location}/{index}"), failures);
            }
        }
        _ => {}
    }
}
