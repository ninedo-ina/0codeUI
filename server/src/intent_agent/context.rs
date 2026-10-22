//! 上下文解析与指代消解。
//!
//! 支持三类上下文来源：
//! 1. 页面上下文（`page_context.entity_id` / `entity_type`）；
//! 2. 页面 HTML 中的 `data-entity-*` 属性与 `<script type="application/json" data-intimeui-context>`；
//! 3. 业务数据上下文（`orders` / `items` / `products` / PRD §5.1 `business_data`）。
//!
//! 解析顺序为「页面 > 会话 > 数据」，并优先使用页面当前实体解释
//! 「它 / 这个 / 那个 / 该 / 此」等指代词。

use chrono::{DateTime, Days, NaiveDate, Utc};
use serde_json::{Map, Value};

use crate::intent_agent::catalog;
use crate::intent_agent::model::{
    Clarification, ClarificationOption, Condition, ConditionOperator, EntityRef, IntentNode,
    MatchMode, PageContext, ResolvedContext, SessionContext, ToolCall, UnderstandRequest,
};

/// 上下文解析器。
pub struct ContextResolver {
    now: DateTime<Utc>,
}

impl ContextResolver {
    /// 创建解析器；`now` 用于解析「今天 / 昨天」等相对日期。
    pub fn new(now: DateTime<Utc>) -> Self {
        Self { now }
    }

    /// 解析请求上下文，得到实体候选与当前聚焦实体。
    pub fn resolve(&self, request: &UnderstandRequest) -> ResolvedContext {
        let active_page = request
            .page_context
            .as_ref()
            .and_then(|page| page.scene_code.clone())
            .or_else(|| {
                request
                    .session_context
                    .as_ref()
                    .and_then(|session| session.active_page.clone())
            });

        let mut entities = Vec::new();
        if let Some(page) = &request.page_context {
            if let Some(entity) = page_entity(page, active_page.as_deref()) {
                entities.push(entity);
            }
            if let Some(html) = &page.html {
                entities.extend(extract_html_entities(html));
            }
        }
        if let Some(session) = &request.session_context {
            if let Some(entity) = session_entity(session) {
                entities.push(entity);
            }
        }
        if let Some(data) = &request.data_context {
            entities.extend(extract_data_entities(data));
        }

        let candidates = dedupe_entities(entities);
        let entity = candidates
            .iter()
            .find(|candidate| candidate.source == "page")
            .or_else(|| {
                candidates
                    .iter()
                    .find(|candidate| candidate.source == "session")
            })
            .cloned();
        let source = entity.as_ref().map(|entity| entity.source.clone());

        ResolvedContext {
            active_page,
            entity,
            candidates,
            source,
            data_context_present: request.data_context.is_some(),
        }
    }

    /// 将意图中的空实体参数绑定到上下文实体。
    ///
    /// 返回 [`ResolutionIssue::Ambiguous`] 时调用方应立即生成澄清追问；
    /// 返回 [`ResolutionIssue::Missing`] 时可让模型重试或最终澄清。
    pub fn bind_intent(
        &self,
        intent: &mut IntentNode,
        context: &ResolvedContext,
        text: &str,
        user_id: Option<&str>,
    ) -> Result<(), ResolutionIssue> {
        let conditions = intent.conditions.clone();
        let match_mode = intent.match_mode;
        let mut missing = None;
        for call in &mut intent.tool_calls {
            if call.capability == "query_user_balance" {
                if let Some(user_id) = user_id {
                    if let Some(parameters) = call.parameters.as_object_mut() {
                        insert_if_empty(parameters, "user_id", user_id.to_string());
                        continue;
                    }
                }
            }
            let Some(required_type) = required_entity_type(call, context) else {
                continue;
            };
            if has_entity_id(call) {
                fill_entity_type_from_id(call);
                continue;
            }
            match self.select_entity(&conditions, match_mode, context, text, &required_type) {
                Selection::One(entity) => apply_entity(call, &entity),
                Selection::Many(entities) => {
                    return Err(ambiguous_issue(&required_type, &entities))
                }
                Selection::None => {
                    missing = Some(missing_issue(&required_type));
                }
            }
        }
        match missing {
            Some(issue) => Err(issue),
            None => Ok(()),
        }
    }

    fn select_entity(
        &self,
        conditions: &[Condition],
        match_mode: MatchMode,
        context: &ResolvedContext,
        text: &str,
        required_type: &str,
    ) -> Selection {
        let mut candidates: Vec<EntityRef> = context
            .candidates
            .iter()
            .filter(|candidate| required_type == "any" || candidate.entity_type == required_type)
            .cloned()
            .collect();

        if !conditions.is_empty() {
            candidates.retain(|candidate| {
                evaluate_conditions(conditions, match_mode, candidate, self.now)
            });
        }
        if candidates.is_empty() {
            return Selection::None;
        }
        if candidates.len() == 1 {
            return Selection::One(candidates.remove(0));
        }

        if contains_pronoun(text) {
            if let Some(primary) = &context.entity {
                let type_matches = required_type == "any" || primary.entity_type == required_type;
                let in_candidates = candidates
                    .iter()
                    .any(|candidate| candidate.id == primary.id);
                if type_matches && in_candidates {
                    return Selection::One(primary.clone());
                }
            }
        }

        Selection::Many(candidates)
    }
}

enum Selection {
    One(EntityRef),
    Many(Vec<EntityRef>),
    None,
}

/// 上下文无法唯一确定实体。
#[derive(Debug, Clone, PartialEq)]
pub enum ResolutionIssue {
    /// 没有候选。
    Missing {
        /// 缺失字段。
        field: String,
        /// 追问文案。
        question: String,
    },
    /// 多个候选。
    Ambiguous {
        /// 缺失字段。
        field: String,
        /// 追问文案。
        question: String,
        /// 候选选项。
        options: Vec<ClarificationOption>,
    },
}

impl ResolutionIssue {
    /// 转为统一澄清结构。
    pub fn to_clarification(&self) -> Clarification {
        match self {
            Self::Missing { field, question } => Clarification {
                question: question.clone(),
                field: field.clone(),
                reason: "上下文缺少唯一可指代实体".to_string(),
                options: vec![],
            },
            Self::Ambiguous {
                field,
                question,
                options,
            } => Clarification {
                question: question.clone(),
                field: field.clone(),
                reason: "上下文中存在多个候选实体".to_string(),
                options: options.clone(),
            },
        }
    }
}

fn page_entity(page: &PageContext, active_page: Option<&str>) -> Option<EntityRef> {
    let id = page.entity_id.as_ref()?.trim();
    if id.is_empty() {
        return None;
    }
    let entity_type = page
        .entity_type
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| infer_entity_type(id, active_page));
    Some(EntityRef {
        id: id.to_string(),
        entity_type,
        label: None,
        attributes: Value::Object(Map::new()),
        source: "page".to_string(),
    })
}

fn session_entity(session: &SessionContext) -> Option<EntityRef> {
    let id = session.entity_id.as_ref()?.trim();
    if id.is_empty() {
        return None;
    }
    let entity_type = session
        .entity_type
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| infer_entity_type(id, session.active_page.as_deref()));
    Some(EntityRef {
        id: id.to_string(),
        entity_type,
        label: None,
        attributes: Value::Object(Map::new()),
        source: "session".to_string(),
    })
}

fn infer_entity_type(id: &str, scene_code: Option<&str>) -> String {
    let upper = id.trim().to_ascii_uppercase();
    if upper.starts_with("ORD_") || upper.starts_with("ORDER_") {
        return "order".to_string();
    }
    if upper.starts_with("ITM_") || upper.starts_with("ITEM_") {
        return "order_item".to_string();
    }
    if upper.starts_with("PRD_") || upper.starts_with("PRODUCT_") {
        return "product".to_string();
    }
    if upper.starts_with("USR_") || upper.starts_with("USER_") {
        return "user".to_string();
    }
    if let Some(scene) = scene_code {
        let scene = scene.to_ascii_uppercase();
        if scene.contains("ORDER") && !scene.contains("ITEM") {
            return "order".to_string();
        }
        if scene.contains("ITEM") {
            return "order_item".to_string();
        }
        if scene.contains("PRODUCT") {
            return "product".to_string();
        }
        if scene.contains("USER") || scene.contains("BALANCE") {
            return "user".to_string();
        }
    }
    "unknown".to_string()
}

fn dedupe_entities(entities: Vec<EntityRef>) -> Vec<EntityRef> {
    let mut result: Vec<EntityRef> = Vec::new();
    for entity in entities {
        if let Some(existing) = result
            .iter_mut()
            .find(|item| item.id == entity.id && item.entity_type == entity.entity_type)
        {
            if existing.label.is_none() {
                existing.label = entity.label;
            }
            merge_attributes(&mut existing.attributes, entity.attributes);
            if existing.source == "data" && entity.source != "data" {
                existing.source = entity.source;
            }
        } else {
            result.push(entity);
        }
    }
    result.truncate(200);
    result
}

fn merge_attributes(existing: &mut Value, incoming: Value) {
    match (existing.as_object_mut(), incoming) {
        (Some(existing), Value::Object(incoming)) => {
            for (key, value) in incoming {
                existing.entry(key).or_insert(value);
            }
        }
        (Some(_), other) => *existing = other,
        (None, other) => *existing = other,
    }
}

fn extract_html_entities(html: &str) -> Vec<EntityRef> {
    let mut entities = extract_script_entities(html);
    entities.extend(extract_tag_entities(html));
    entities
}

fn extract_script_entities(html: &str) -> Vec<EntityRef> {
    let mut entities = Vec::new();
    let mut cursor = 0usize;
    let mut count = 0usize;
    while count < 20 {
        let Some(relative_start) = html[cursor..].find("<script") else {
            break;
        };
        let start = cursor + relative_start;
        let Some(relative_tag_end) = html[start..].find('>') else {
            break;
        };
        let tag_end = start + relative_tag_end;
        let Some(relative_close) = html[tag_end + 1..].find("</script") else {
            break;
        };
        let close = tag_end + 1 + relative_close;
        let open_tag = &html[start..tag_end];
        if is_context_script(open_tag) {
            if let Ok(value) = serde_json::from_str::<Value>(&html[tag_end + 1..close]) {
                entities.extend(extract_data_entities(&value));
            }
        }
        cursor = close + "</script".len();
        count += 1;
    }
    entities
}

fn is_context_script(open_tag: &str) -> bool {
    let lower = open_tag.to_ascii_lowercase();
    lower.contains("application/json")
        && (lower.contains("intimeui-context") || lower.contains("data-intimeui-context"))
}

fn extract_tag_entities(html: &str) -> Vec<EntityRef> {
    let mut entities = Vec::new();
    let mut cursor = 0usize;
    let mut count = 0usize;
    while count < 200 {
        let Some(relative_start) = html[cursor..].find('<') else {
            break;
        };
        let start = cursor + relative_start;
        let Some(relative_end) = html[start..].find('>') else {
            break;
        };
        let end = start + relative_end;
        let tag = &html[start + 1..end];
        cursor = end + 1;
        count += 1;
        if tag.starts_with('/') || tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let attributes = parse_attributes(tag);
        if let Some(entity) = entity_from_attributes(&attributes) {
            entities.push(entity);
        }
        if let Some(raw) = attribute_value(&attributes, "data-intimeui-entity") {
            if let Ok(value) = serde_json::from_str::<Value>(raw) {
                if let Some(entity) = entity_from_value(&value, Some("unknown")) {
                    entities.push(entity);
                }
            }
        }
    }
    entities
}

fn parse_attributes(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut attributes = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        while index < bytes.len() && !is_attribute_name_start(bytes[index]) {
            index += 1;
        }
        let name_start = index;
        while index < bytes.len() && is_attribute_name_char(bytes[index]) {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let name = tag[name_start..index].to_ascii_lowercase();
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] != b'=' {
            continue;
        }
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let value = if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            index += 1;
            let value_start = index;
            while index < bytes.len() && bytes[index] != quote {
                index += 1;
            }
            let value = tag[value_start..index].to_string();
            if index < bytes.len() {
                index += 1;
            }
            value
        } else {
            let value_start = index;
            while index < bytes.len() && !bytes[index].is_ascii_whitespace() && bytes[index] != b'>'
            {
                index += 1;
            }
            tag[value_start..index].to_string()
        };
        attributes.push((name, decode_html_entities(&value)));
    }
    attributes
}

fn is_attribute_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b':' || byte == b'-'
}

fn is_attribute_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':' || byte == b'-' || byte == b'.'
}

fn decode_html_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn attribute_value<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn entity_from_attributes(attributes: &[(String, String)]) -> Option<EntityRef> {
    let id = attribute_value(attributes, "data-entity-id")?.trim();
    if id.is_empty() {
        return None;
    }
    let entity_type = attribute_value(attributes, "data-entity-type")
        .map(ToString::to_string)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| infer_entity_type(id, None));
    let label = attribute_value(attributes, "data-entity-name")
        .or_else(|| attribute_value(attributes, "data-label"))
        .or_else(|| attribute_value(attributes, "data-name"))
        .map(ToString::to_string);
    let mut attributes_object = Map::new();
    for key in [
        "data-price",
        "data-amount",
        "data-created-at",
        "data-status",
        "data-order-id",
        "data-qty",
        "data-product-id",
    ] {
        if let Some(value) = attribute_value(attributes, key) {
            let normalized = key.trim_start_matches("data-").replace('-', "_");
            attributes_object.insert(normalized, parse_scalar(value));
        }
    }
    Some(EntityRef {
        id: id.to_string(),
        entity_type,
        label,
        attributes: Value::Object(attributes_object),
        source: "html".to_string(),
    })
}

fn parse_scalar(value: &str) -> Value {
    let trimmed = value.trim();
    if let Ok(integer) = trimmed.parse::<i64>() {
        return Value::Number(integer.into());
    }
    if let Ok(number) = trimmed.parse::<f64>() {
        if let Some(number) = serde_json::Number::from_f64(number) {
            return Value::Number(number);
        }
    }
    Value::String(trimmed.to_string())
}

fn extract_data_entities(value: &Value) -> Vec<EntityRef> {
    let mut entities = Vec::new();
    collect_entities(value, None, &mut entities);
    entities
}

fn collect_entities(value: &Value, default_type: Option<&str>, entities: &mut Vec<EntityRef>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_entities(item, default_type, entities);
            }
        }
        Value::Object(object) => {
            collect_named_collection(object, "entities", None, entities);
            collect_named_collection(object, "orders", Some("order"), entities);
            collect_named_collection(object, "products", Some("product"), entities);
            collect_items_collection(object, entities);
            if is_entity_object(object) {
                if let Some(entity) = entity_from_value(value, default_type) {
                    entities.push(entity);
                }
            } else if let Some(default_type) = default_type {
                if let Some(entity) = entity_from_value(value, Some(default_type)) {
                    entities.push(entity);
                }
            }
        }
        _ => {}
    }
}

fn collect_named_collection(
    object: &Map<String, Value>,
    key: &str,
    default_type: Option<&str>,
    entities: &mut Vec<EntityRef>,
) {
    let Some(items) = object.get(key).and_then(Value::as_array) else {
        return;
    };
    for item in items {
        collect_entities(item, default_type, entities);
    }
}

fn collect_items_collection(object: &Map<String, Value>, entities: &mut Vec<EntityRef>) {
    let Some(items) = object.get("items").and_then(Value::as_array) else {
        return;
    };
    let parent_order_id = object
        .get("order_id")
        .or_else(|| object.get("id"))
        .and_then(value_to_string);
    for item in items {
        if let Some(mut entity) = entity_from_value(item, Some("order_item")) {
            if let Some(order_id) = &parent_order_id {
                if let Some(attributes) = entity.attributes.as_object_mut() {
                    attributes
                        .entry("order_id".to_string())
                        .or_insert_with(|| Value::String(order_id.clone()));
                }
            }
            entities.push(entity);
        }
    }
}

fn is_entity_object(object: &Map<String, Value>) -> bool {
    object.contains_key("id")
        || object.contains_key("entity_id")
        || object.contains_key("order_id")
        || object.contains_key("item_id")
        || object.contains_key("product_id")
}

fn entity_from_value(value: &Value, default_type: Option<&str>) -> Option<EntityRef> {
    let object = value.as_object()?;
    let id = first_string(
        object,
        &["id", "entity_id", "order_id", "item_id", "product_id"],
    )?;
    if id.len() > 128 {
        return None;
    }
    let entity_type = first_string(object, &["entity_type", "type"])
        .map(|value| normalize_entity_type(&value))
        .or_else(|| default_type.map(normalize_entity_type))
        .unwrap_or_else(|| infer_entity_type(&id, None));
    let label = first_string(object, &["label", "name", "title", "product_name"]);
    let mut attributes = Map::new();
    if let Some(extra) = object.get("attributes").and_then(Value::as_object) {
        attributes.extend(extra.clone());
    }
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "id" | "entity_id"
                | "entity_type"
                | "type"
                | "label"
                | "name"
                | "title"
                | "product_name"
                | "attributes"
        ) {
            continue;
        }
        attributes.insert(normalize_key(key), value.clone());
    }
    Some(EntityRef {
        id,
        entity_type,
        label,
        attributes: Value::Object(attributes),
        source: "data".to_string(),
    })
}

fn first_string(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(value_to_string)
            .filter(|value| !value.trim().is_empty())
    })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn normalize_entity_type(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "order" | "orders" => "order".to_string(),
        "order_item" | "order-item" | "orderitem" | "item" | "items" => "order_item".to_string(),
        "product" | "products" => "product".to_string(),
        "user" | "users" => "user".to_string(),
        other => other.to_string(),
    }
}

fn normalize_key(key: &str) -> String {
    match key {
        "createdAt" => "created_at".to_string(),
        "orderId" => "order_id".to_string(),
        "itemId" => "item_id".to_string(),
        "productId" => "product_id".to_string(),
        "unitPrice" => "unit_price".to_string(),
        other => other.to_string(),
    }
}

fn required_entity_type(call: &ToolCall, context: &ResolvedContext) -> Option<String> {
    if call.capability == "delete_record" {
        let explicit = call
            .parameters
            .get("entity_type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty());
        if let Some(explicit) = explicit {
            return Some(explicit.to_string());
        }
        if let Some(entity) = &context.entity {
            if entity.entity_type != "unknown" {
                return Some(entity.entity_type.clone());
            }
        }
        return Some("any".to_string());
    }
    catalog::entity_type_for(&call.capability, &call.parameters)
}

fn has_entity_id(call: &ToolCall) -> bool {
    let key = match call.capability.as_str() {
        "delete_record" => "entity_id",
        "update_item_status" => "item_id",
        "update_order_address" | "query_order_business_data" => "order_id",
        _ => return false,
    };
    call.parameters
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn fill_entity_type_from_id(call: &mut ToolCall) {
    if call.capability != "delete_record" {
        return;
    }
    let Some(parameters) = call.parameters.as_object_mut() else {
        return;
    };
    let missing = parameters
        .get("entity_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::is_empty)
        .unwrap_or(true);
    if !missing {
        return;
    }
    let Some(id) = parameters.get("entity_id").and_then(Value::as_str) else {
        return;
    };
    parameters.insert(
        "entity_type".to_string(),
        Value::String(infer_entity_type(id, None)),
    );
}

fn apply_entity(call: &mut ToolCall, entity: &EntityRef) {
    let Some(parameters) = call.parameters.as_object_mut() else {
        return;
    };
    match call.capability.as_str() {
        "delete_record" => {
            insert_if_empty(parameters, "entity_type", entity.entity_type.clone());
            insert_if_empty(parameters, "entity_id", entity.id.clone());
        }
        "update_item_status" => {
            insert_if_empty(parameters, "item_id", entity.id.clone());
        }
        "update_order_address" | "query_order_business_data" => {
            insert_if_empty(parameters, "order_id", entity.id.clone());
        }
        "query_user_balance" if entity.entity_type == "user" => {
            insert_if_empty(parameters, "user_id", entity.id.clone());
        }
        _ => {}
    }
}

fn insert_if_empty(parameters: &mut Map<String, Value>, key: &str, value: String) {
    let empty = parameters
        .get(key)
        .and_then(Value::as_str)
        .map(|existing| existing.trim().is_empty())
        .unwrap_or_else(|| !parameters.contains_key(key));
    if empty {
        parameters.insert(key.to_string(), Value::String(value));
    }
}

fn ambiguous_issue(required_type: &str, entities: &[EntityRef]) -> ResolutionIssue {
    let options = entities
        .iter()
        .map(|entity| ClarificationOption {
            label: entity.display_label(),
            value: Value::String(entity.id.clone()),
        })
        .collect();
    ResolutionIssue::Ambiguous {
        field: "entity_id".to_string(),
        question: format!(
            "找到多个可能的{}，请选择要操作的一个。",
            entity_type_label(required_type)
        ),
        options,
    }
}

fn missing_issue(required_type: &str) -> ResolutionIssue {
    ResolutionIssue::Missing {
        field: "entity_id".to_string(),
        question: format!(
            "没有找到符合条件的{}，请补充订单号/商品名，或先在页面中选择目标。",
            entity_type_label(required_type)
        ),
    }
}

fn entity_type_label(entity_type: &str) -> &'static str {
    match entity_type {
        "order" => "订单",
        "order_item" => "订单项/商品",
        "product" => "商品",
        "user" => "用户",
        _ => "业务对象",
    }
}

fn contains_pronoun(text: &str) -> bool {
    const PRONOUNS: &[&str] = &[
        "它", "这个", "那个", "该", "此", "这件", "那件", "这批", "那批", "将其", "把它", "上面",
        "刚才", "刚刚", "当前", " it ", " this ", " that ",
    ];
    let lower = text.to_ascii_lowercase();
    PRONOUNS.iter().any(|pronoun| lower.contains(pronoun))
}

fn evaluate_conditions(
    conditions: &[Condition],
    match_mode: MatchMode,
    entity: &EntityRef,
    now: DateTime<Utc>,
) -> bool {
    if conditions.is_empty() {
        return true;
    }
    match match_mode {
        MatchMode::All => conditions
            .iter()
            .all(|condition| evaluate_condition(condition, entity, now)),
        MatchMode::Any => conditions
            .iter()
            .any(|condition| evaluate_condition(condition, entity, now)),
    }
}

fn evaluate_condition(condition: &Condition, entity: &EntityRef, now: DateTime<Utc>) -> bool {
    let actual = match condition.field.as_str() {
        "entity_type" => Some(Value::String(entity.entity_type.clone())),
        "id" => Some(Value::String(entity.id.clone())),
        field => entity.attribute(field).cloned(),
    };
    match condition.operator {
        ConditionOperator::Exists => actual.is_some_and(|value| !value.is_null()),
        ConditionOperator::Eq => actual
            .as_ref()
            .is_some_and(|value| values_equal(value, &condition.value)),
        ConditionOperator::Ne => actual
            .as_ref()
            .map(|value| !values_equal(value, &condition.value))
            .unwrap_or(true),
        ConditionOperator::Gt => compare_numbers(&actual, &condition.value, |a, b| a > b),
        ConditionOperator::Gte => compare_numbers(&actual, &condition.value, |a, b| a >= b),
        ConditionOperator::Lt => compare_numbers(&actual, &condition.value, |a, b| a < b),
        ConditionOperator::Lte => compare_numbers(&actual, &condition.value, |a, b| a <= b),
        ConditionOperator::Contains => actual
            .as_ref()
            .is_some_and(|value| value_contains(value, &condition.value)),
        ConditionOperator::In => condition.value.as_array().is_some_and(|allowed| {
            actual
                .as_ref()
                .is_some_and(|value| allowed.iter().any(|item| values_equal(value, item)))
        }),
        ConditionOperator::DateEq => date_compare(&actual, &condition.value, now, |a, b| a == b),
        ConditionOperator::DateBefore => date_compare(&actual, &condition.value, now, |a, b| a < b),
        ConditionOperator::DateAfter => date_compare(&actual, &condition.value, now, |a, b| a > b),
    }
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::String(left), Value::String(right)) => left.trim() == right.trim(),
        _ => left == right,
    }
}

fn compare_numbers<F>(actual: &Option<Value>, expected: &Value, compare: F) -> bool
where
    F: Fn(f64, f64) -> bool,
{
    let Some(actual) = actual.as_ref().and_then(to_f64) else {
        return false;
    };
    let Some(expected) = to_f64(expected) else {
        return false;
    };
    compare(actual, expected)
}

fn to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn value_contains(actual: &Value, expected: &Value) -> bool {
    match actual {
        Value::String(text) => expected
            .as_str()
            .is_some_and(|needle| text.contains(needle)),
        Value::Array(items) => items.iter().any(|item| values_equal(item, expected)),
        _ => false,
    }
}

fn date_compare<F>(actual: &Option<Value>, expected: &Value, now: DateTime<Utc>, compare: F) -> bool
where
    F: Fn(NaiveDate, NaiveDate) -> bool,
{
    let Some(actual) = actual.as_ref().and_then(parse_date_value) else {
        return false;
    };
    let Some(expected) = parse_date_value_with_now(expected, now) else {
        return false;
    };
    compare(actual, expected)
}

fn parse_date_value(value: &Value) -> Option<NaiveDate> {
    let text = value.as_str()?.trim();
    if let Ok(date_time) = DateTime::parse_from_rfc3339(text) {
        return Some(date_time.date_naive());
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

fn parse_date_value_with_now(value: &Value, now: DateTime<Utc>) -> Option<NaiveDate> {
    if let Some(keyword) = value.as_str() {
        let today = now.date_naive();
        match keyword.trim().to_ascii_lowercase().as_str() {
            "today" => return Some(today),
            "yesterday" => return today.checked_sub_days(Days::new(1)),
            "day_before_yesterday" => return today.checked_sub_days(Days::new(2)),
            "tomorrow" => return today.checked_add_days(Days::new(1)),
            _ => {}
        }
    }
    parse_date_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_agent::model::{
        Condition, ConditionOperator, IntentNode, MatchMode, ToolCall,
    };
    use chrono::TimeZone;
    use serde_json::json;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap()
    }

    fn request_with_data() -> UnderstandRequest {
        UnderstandRequest {
            user_id: Some("usr_9527".to_string()),
            text: "帮我退掉昨天买的那个超 50 元的商品".to_string(),
            page_context: Some(PageContext {
                scene_code: Some("ORDER_DETAIL_PAGE".to_string()),
                entity_id: Some("ORD_20261014_001".to_string()),
                entity_type: Some("order".to_string()),
                html: None,
            }),
            data_context: Some(json!({
                "orders": [{
                    "id": "ORD_20261014_001",
                    "status": "PAID",
                    "created_at": "2026-10-21T09:00:00Z",
                    "items": [{
                        "id": "ITM_1",
                        "name": "智能降噪耳机",
                        "price": 299.0,
                        "created_at": "2026-10-21T09:00:00Z",
                        "status": "PAID"
                    }, {
                        "id": "ITM_2",
                        "name": "数据线",
                        "price": 29.0,
                        "created_at": "2026-10-21T09:00:00Z",
                        "status": "PAID"
                    }]
                }]
            })),
            session_context: None,
        }
    }

    #[test]
    fn resolves_page_entity_and_data_items() {
        let resolver = ContextResolver::new(fixed_now());
        let context = resolver.resolve(&request_with_data());
        assert_eq!(context.entity.as_ref().unwrap().id, "ORD_20261014_001");
        assert!(context
            .candidates
            .iter()
            .any(|entity| entity.id == "ITM_1" && entity.entity_type == "order_item"));
    }

    #[test]
    fn filters_items_by_price_and_yesterday() {
        let resolver = ContextResolver::new(fixed_now());
        let context = resolver.resolve(&request_with_data());
        let mut intent = IntentNode {
            intent_id: "intent_1".to_string(),
            intent_type: "REFUND".to_string(),
            summary: "退款".to_string(),
            confidence: 0.9,
            requires_confirmation: true,
            match_mode: MatchMode::All,
            conditions: vec![
                Condition {
                    field: "entity_type".to_string(),
                    operator: ConditionOperator::Eq,
                    value: json!("order_item"),
                },
                Condition {
                    field: "price".to_string(),
                    operator: ConditionOperator::Gt,
                    value: json!(50),
                },
                Condition {
                    field: "created_at".to_string(),
                    operator: ConditionOperator::DateEq,
                    value: json!("yesterday"),
                },
            ],
            tool_calls: vec![ToolCall {
                call_id: "call_1".to_string(),
                capability: "update_item_status".to_string(),
                parameters: json!({"status": "REFUNDED"}),
                depends_on: vec![],
                description: "退款".to_string(),
            }],
        };
        resolver
            .bind_intent(
                &mut intent,
                &context,
                "帮我退掉昨天买的那个超 50 元的商品",
                None,
            )
            .unwrap();
        assert_eq!(intent.tool_calls[0].parameters["item_id"], json!("ITM_1"));
    }

    #[test]
    fn ambiguous_candidates_require_clarification() {
        let resolver = ContextResolver::new(fixed_now());
        let request = UnderstandRequest {
            user_id: None,
            text: "把它删了".to_string(),
            page_context: None,
            data_context: Some(json!({
                "orders": [
                    {"id": "ORD_1", "status": "PAID"},
                    {"id": "ORD_2", "status": "PAID"}
                ]
            })),
            session_context: None,
        };
        let context = resolver.resolve(&request);
        let mut intent = IntentNode {
            intent_id: "intent_1".to_string(),
            intent_type: "DELETE".to_string(),
            summary: "删除".to_string(),
            confidence: 0.9,
            requires_confirmation: true,
            match_mode: MatchMode::All,
            conditions: vec![],
            tool_calls: vec![ToolCall {
                call_id: "call_1".to_string(),
                capability: "delete_record".to_string(),
                parameters: json!({"entity_type": "order"}),
                depends_on: vec![],
                description: "删除订单".to_string(),
            }],
        };
        let issue = resolver
            .bind_intent(&mut intent, &context, "把它删了", None)
            .unwrap_err();
        assert!(matches!(issue, ResolutionIssue::Ambiguous { .. }));
    }

    #[test]
    fn html_data_attributes_are_extracted() {
        let html = r#"<div data-entity-id="ITM_9" data-entity-type="order_item"
            data-entity-name="耳机" data-price="88.5" data-created-at="2026-10-21"></div>"#;
        let entities = extract_html_entities(html);
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].id, "ITM_9");
        assert_eq!(entities[0].attributes["price"], json!(88.5));
    }

    #[test]
    fn json_context_script_is_extracted() {
        let html = r#"<script type="application/json" data-intimeui-context>
            {"entities":[{"id":"ORD_9","entity_type":"order","status":"PAID"}]}
        </script>"#;
        let entities = extract_html_entities(html);
        assert!(entities.iter().any(|entity| entity.id == "ORD_9"));
    }

    #[test]
    fn pronouns_prefer_page_entity() {
        let resolver = ContextResolver::new(fixed_now());
        let request = UnderstandRequest {
            user_id: None,
            text: "把它删了".to_string(),
            page_context: Some(PageContext {
                scene_code: Some("ORDER_DETAIL_PAGE".to_string()),
                entity_id: Some("ORD_1".to_string()),
                entity_type: Some("order".to_string()),
                html: None,
            }),
            data_context: Some(json!({
                "orders": [{"id": "ORD_1"}, {"id": "ORD_2"}]
            })),
            session_context: None,
        };
        let context = resolver.resolve(&request);
        let mut intent = IntentNode {
            intent_id: "intent_1".to_string(),
            intent_type: "DELETE".to_string(),
            summary: "删除".to_string(),
            confidence: 0.9,
            requires_confirmation: true,
            match_mode: MatchMode::All,
            conditions: vec![],
            tool_calls: vec![ToolCall {
                call_id: "call_1".to_string(),
                capability: "delete_record".to_string(),
                parameters: json!({"entity_type": "order"}),
                depends_on: vec![],
                description: "删除订单".to_string(),
            }],
        };
        resolver
            .bind_intent(&mut intent, &context, "把它删了", None)
            .unwrap();
        assert_eq!(intent.tool_calls[0].parameters["entity_id"], json!("ORD_1"));
    }
}
