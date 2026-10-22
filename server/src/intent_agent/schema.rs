//! JSON Schema 校验（受控子集）与决策树语义校验。
//!
//! 模型输出先经过 [`validate`] 做结构校验，再经过 [`validate_decision`] 做
//! 业务语义校验；两层都通过后才允许进入执行链路。

use std::collections::HashSet;
use std::sync::LazyLock;

use serde_json::{json, Value};

use crate::intent_agent::catalog;
use crate::intent_agent::model::{Decision, DecisionStatus};

/// 决策树 JSON Schema。
pub fn decision_schema() -> &'static Value {
    static SCHEMA: LazyLock<Value> = LazyLock::new(build_decision_schema);
    &SCHEMA
}

fn build_decision_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "InTimeUI Intent Decision",
        "type": "object",
        "additionalProperties": false,
        "required": ["status", "intents", "execution_order", "clarification"],
        "properties": {
            "status": {
                "type": "string",
                "enum": ["READY", "NEEDS_CLARIFICATION", "REJECTED"]
            },
            "intents": {
                "type": "array",
                "maxItems": 10,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": [
                        "intent_id", "intent_type", "summary", "confidence",
                        "requires_confirmation", "match_mode", "conditions", "tool_calls"
                    ],
                    "properties": {
                        "intent_id": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 64,
                            "pattern": "^[A-Za-z0-9._:-]+$"
                        },
                        "intent_type": {
                            "type": "string",
                            "minLength": 2,
                            "maxLength": 64,
                            "pattern": "^[A-Z][A-Z0-9_]{1,63}$"
                        },
                        "summary": {"type": "string", "minLength": 1, "maxLength": 200},
                        "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                        "requires_confirmation": {"type": "boolean"},
                        "match_mode": {"type": "string", "enum": ["all", "any"]},
                        "conditions": {
                            "type": "array",
                            "maxItems": 10,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["field", "operator", "value"],
                                "properties": {
                                    "field": {
                                        "type": "string",
                                        "minLength": 1,
                                        "maxLength": 64,
                                        "pattern": "^[A-Za-z0-9_.:-]+$"
                                    },
                                    "operator": {
                                        "type": "string",
                                        "enum": [
                                            "eq", "ne", "gt", "gte", "lt", "lte",
                                            "contains", "in", "exists",
                                            "date_eq", "date_before", "date_after"
                                        ]
                                    },
                                    "value": {}
                                }
                            }
                        },
                        "tool_calls": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 20,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["call_id", "capability", "parameters", "depends_on", "description"],
                                "properties": {
                                    "call_id": {
                                        "type": "string",
                                        "minLength": 1,
                                        "maxLength": 64,
                                        "pattern": "^[A-Za-z0-9._:-]+$"
                                    },
                                    "capability": {
                                        "type": "string",
                                        "enum": [
                                            "create_order", "query_user_balance",
                                            "query_order_business_data", "update_order_address",
                                            "update_item_status", "delete_record"
                                        ]
                                    },
                                    "parameters": {"type": "object"},
                                    "depends_on": {
                                        "type": "array",
                                        "maxItems": 20,
                                        "items": {
                                            "type": "string",
                                            "minLength": 1,
                                            "maxLength": 64,
                                            "pattern": "^[A-Za-z0-9._:-]+$"
                                        }
                                    },
                                    "description": {"type": "string", "minLength": 1, "maxLength": 300}
                                }
                            }
                        }
                    }
                }
            },
            "execution_order": {
                "type": "array",
                "maxItems": 10,
                "items": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 64,
                    "pattern": "^[A-Za-z0-9._:-]+$"
                }
            },
            "clarification": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "required": ["question", "field", "reason", "options"],
                "properties": {
                    "question": {"type": "string", "minLength": 1, "maxLength": 300},
                    "field": {"type": "string", "minLength": 1, "maxLength": 64},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 300},
                    "options": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["label", "value"],
                            "properties": {
                                "label": {"type": "string", "minLength": 1, "maxLength": 100},
                                "value": {}
                            }
                        }
                    }
                }
            }
        }
    })
}

/// 校验 JSON 值是否符合给定 Schema；返回全部错误（空表示通过）。
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    validate_at(schema, value, "output", &mut errors);
    errors
}

fn validate_at(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return;
    };

    if let Some(expected) = schema.get("type") {
        if !type_matches(expected, value) {
            errors.push(format!(
                "{path}: expected {}, got {}",
                type_label(expected),
                value_kind(value)
            ));
            return;
        }
    }

    if let Some(allowed) = schema.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            errors.push(format!(
                "{path}: value is not one of the allowed enum values"
            ));
            return;
        }
    }

    match value {
        Value::Object(map) => validate_object(schema, map, path, errors),
        Value::Array(items) => validate_array(schema, items, path, errors),
        Value::String(text) => validate_string(schema, text, path, errors),
        Value::Number(number) => validate_number(schema, number, path, errors),
        _ => {}
    }
}

fn validate_object(
    schema: &serde_json::Map<String, Value>,
    map: &serde_json::Map<String, Value>,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if !map.contains_key(field) {
                errors.push(format!("{path}: missing required field `{field}`"));
            }
        }
    }

    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (field, property_schema) in properties {
            if let Some(property_value) = map.get(field) {
                validate_at(
                    property_schema,
                    property_value,
                    &format!("{path}.{field}"),
                    errors,
                );
            }
        }
    }

    if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
        for field in map.keys() {
            let known = properties
                .map(|properties| properties.contains_key(field))
                .unwrap_or(false);
            if !known {
                errors.push(format!("{path}: unexpected field `{field}`"));
            }
        }
    }

    if let Some(min) = schema.get("minProperties").and_then(Value::as_u64) {
        if (map.len() as u64) < min {
            errors.push(format!("{path}: expected at least {min} properties"));
        }
    }
    if let Some(max) = schema.get("maxProperties").and_then(Value::as_u64) {
        if (map.len() as u64) > max {
            errors.push(format!("{path}: expected at most {max} properties"));
        }
    }
}

fn validate_array(
    schema: &serde_json::Map<String, Value>,
    items: &[Value],
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
        if (items.len() as u64) < min {
            errors.push(format!("{path}: expected at least {min} items"));
        }
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
        if (items.len() as u64) > max {
            errors.push(format!("{path}: expected at most {max} items"));
        }
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_at(item_schema, item, &format!("{path}[{index}]"), errors);
        }
    }
}

fn validate_string(
    schema: &serde_json::Map<String, Value>,
    text: &str,
    path: &str,
    errors: &mut Vec<String>,
) {
    let length = text.chars().count() as u64;
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
        if length < min {
            errors.push(format!("{path}: string is shorter than {min} characters"));
        }
    }
    if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
        if length > max {
            errors.push(format!("{path}: string is longer than {max} characters"));
        }
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        match regex::Regex::new(pattern) {
            Ok(regex) if !regex.is_match(text) => {
                errors.push(format!("{path}: string does not match pattern `{pattern}`"));
            }
            Ok(_) => {}
            Err(error) => errors.push(format!("{path}: invalid schema pattern: {error}")),
        }
    }
}

fn validate_number(
    schema: &serde_json::Map<String, Value>,
    number: &serde_json::Number,
    path: &str,
    errors: &mut Vec<String>,
) {
    let Some(value) = number.as_f64() else {
        errors.push(format!("{path}: number is not representable"));
        return;
    };
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if value < minimum {
            errors.push(format!("{path}: number is less than minimum {minimum}"));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if value > maximum {
            errors.push(format!("{path}: number is greater than maximum {maximum}"));
        }
    }
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(name) => single_type_matches(name, value),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .any(|name| single_type_matches(name, value)),
        _ => true,
    }
}

fn single_type_matches(name: &str, value: &Value) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value.as_f64().is_some_and(|number| number.fract() == 0.0)
        }
        _ => true,
    }
}

fn type_label(expected: &Value) -> String {
    match expected {
        Value::String(name) => name.clone(),
        Value::Array(names) => names
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | "),
        _ => "any".to_string(),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// 决策树语义校验（不校验能力参数，参数需在上下文绑定后校验）。
pub fn validate_decision(decision: &Decision) -> Vec<String> {
    let mut errors = Vec::new();

    match decision.status {
        DecisionStatus::Ready => {
            if decision.intents.is_empty() {
                errors.push("READY 状态必须至少包含一个意图".to_string());
            }
            if decision.clarification.is_some() {
                errors.push("READY 状态不能包含 clarification".to_string());
            }
        }
        DecisionStatus::NeedsClarification | DecisionStatus::Rejected => {
            if decision.clarification.is_none() {
                errors.push("澄清/拒绝状态必须包含 clarification".to_string());
            }
            if !decision.intents.is_empty() {
                errors.push("澄清/拒绝状态不能包含可执行意图".to_string());
            }
            if !decision.execution_order.is_empty() {
                errors.push("澄清/拒绝状态不能包含执行顺序".to_string());
            }
        }
    }

    if decision.intents.len() > 10 {
        errors.push("一次最多解析 10 个意图".to_string());
    }

    let mut intent_ids = HashSet::new();
    for intent in &decision.intents {
        if !intent_ids.insert(intent.intent_id.as_str()) {
            errors.push(format!("重复的 intent_id `{}`", intent.intent_id));
        }
        if intent.tool_calls.is_empty() {
            errors.push(format!("意图 `{}` 至少需要一个函数调用", intent.intent_id));
        }
        if intent.tool_calls.len() > 20 {
            errors.push(format!("意图 `{}` 的函数调用超过 20 个", intent.intent_id));
        }
        for call in &intent.tool_calls {
            if catalog::capability(&call.capability).is_none() {
                errors.push(format!(
                    "调用 `{}` 使用了未知能力 `{}`",
                    call.call_id, call.capability
                ));
            }
            if catalog::requires_confirmation(&call.capability) && !intent.requires_confirmation {
                errors.push(format!(
                    "写/删类调用 `{}` 必须设置 requires_confirmation=true",
                    call.call_id
                ));
            }
        }
    }

    if decision.status == DecisionStatus::Ready {
        let mut order_set = HashSet::new();
        for intent_id in &decision.execution_order {
            if !order_set.insert(intent_id.as_str()) {
                errors.push(format!("execution_order 中重复出现 `{intent_id}`"));
            }
            if !intent_ids.contains(intent_id.as_str()) {
                errors.push(format!("execution_order 引用了不存在的意图 `{intent_id}`"));
            }
        }
        if order_set != intent_ids {
            errors.push("execution_order 必须恰好覆盖所有 intent_id".to_string());
        }
    }

    let mut seen_calls = HashSet::new();
    for intent_id in &decision.execution_order {
        let Some(intent) = decision
            .intents
            .iter()
            .find(|intent| &intent.intent_id == intent_id)
        else {
            continue;
        };
        for call in &intent.tool_calls {
            for dependency in &call.depends_on {
                if !seen_calls.contains(dependency.as_str()) {
                    errors.push(format!(
                        "调用 `{}` 依赖了不存在或非前序的调用 `{dependency}`",
                        call.call_id
                    ));
                }
            }
            if !seen_calls.insert(call.call_id.as_str()) {
                errors.push(format!("重复的 call_id `{}`", call.call_id));
            }
        }
    }

    errors
}

/// 在上下文绑定后校验所有能力参数。
pub fn validate_tool_parameters(decision: &Decision) -> Vec<String> {
    let mut errors = Vec::new();
    for intent in &decision.intents {
        for call in &intent.tool_calls {
            if let Err(mut call_errors) =
                catalog::validate_parameters(&call.capability, &call.parameters)
            {
                errors.append(&mut call_errors);
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_agent::model::{Clarification, Condition, IntentNode, MatchMode, ToolCall};

    fn ready_decision() -> Decision {
        Decision {
            status: DecisionStatus::Ready,
            intents: vec![IntentNode {
                intent_id: "intent_1".to_string(),
                intent_type: "DELETE".to_string(),
                summary: "删除当前订单".to_string(),
                confidence: 0.9,
                requires_confirmation: true,
                match_mode: MatchMode::All,
                conditions: vec![Condition {
                    field: "entity_type".to_string(),
                    operator: crate::intent_agent::model::ConditionOperator::Eq,
                    value: json!("order"),
                }],
                tool_calls: vec![ToolCall {
                    call_id: "call_1".to_string(),
                    capability: "delete_record".to_string(),
                    parameters: json!({"entity_type": "order", "entity_id": "ORD_1"}),
                    depends_on: vec![],
                    description: "删除订单".to_string(),
                }],
            }],
            execution_order: vec!["intent_1".to_string()],
            clarification: None,
        }
    }

    #[test]
    fn schema_accepts_valid_decision() {
        let value = serde_json::to_value(ready_decision()).unwrap();
        assert!(validate(decision_schema(), &value).is_empty());
        assert!(validate_decision(&ready_decision()).is_empty());
    }

    #[test]
    fn schema_rejects_unknown_capability_and_missing_fields() {
        let mut value = serde_json::to_value(ready_decision()).unwrap();
        value["intents"][0]["tool_calls"][0]["capability"] = json!("drop_database");
        value["intents"][0]["tool_calls"][0]
            .as_object_mut()
            .unwrap()
            .remove("description");
        let errors = validate(decision_schema(), &value);
        assert!(errors.iter().any(|error| error.contains("enum")));
        assert!(errors.iter().any(|error| error.contains("description")));
    }

    #[test]
    fn semantic_check_rejects_write_without_confirmation() {
        let mut decision = ready_decision();
        decision.intents[0].requires_confirmation = false;
        let errors = validate_decision(&decision);
        assert!(errors
            .iter()
            .any(|error| error.contains("requires_confirmation")));
    }

    #[test]
    fn semantic_check_requires_exact_execution_order() {
        let mut decision = ready_decision();
        decision.execution_order.clear();
        let errors = validate_decision(&decision);
        assert!(errors.iter().any(|error| error.contains("execution_order")));
    }

    #[test]
    fn semantic_check_rejects_forward_dependency() {
        let mut decision = ready_decision();
        decision.intents[0].tool_calls[0].depends_on = vec!["call_2".to_string()];
        decision.intents[0].tool_calls.push(ToolCall {
            call_id: "call_2".to_string(),
            capability: "query_user_balance".to_string(),
            parameters: json!({}),
            depends_on: vec![],
            description: "查询余额".to_string(),
        });
        let errors = validate_decision(&decision);
        assert!(errors.iter().any(|error| error.contains("非前序")));
    }

    #[test]
    fn clarification_must_be_present_when_needed() {
        let decision = Decision {
            status: DecisionStatus::NeedsClarification,
            intents: vec![],
            execution_order: vec![],
            clarification: None,
        };
        assert!(!validate_decision(&decision).is_empty());
    }

    #[test]
    fn clarification_schema_allows_null() {
        let value = json!({
            "status": "NEEDS_CLARIFICATION",
            "intents": [],
            "execution_order": [],
            "clarification": {
                "question": "请选择要删除的订单",
                "field": "entity_id",
                "reason": "存在多个候选",
                "options": [{"label": "订单 A", "value": "ORD_1"}]
            }
        });
        assert!(validate(decision_schema(), &value).is_empty());
        let decision = Decision {
            status: DecisionStatus::NeedsClarification,
            intents: vec![],
            execution_order: vec![],
            clarification: Some(Clarification {
                question: "请选择要删除的订单".to_string(),
                field: "entity_id".to_string(),
                reason: "存在多个候选".to_string(),
                options: vec![],
            }),
        };
        assert!(validate_decision(&decision).is_empty());
    }
}
