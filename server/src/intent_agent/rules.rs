//! 规则回退模型。
//!
//! 生产环境应通过 [`crate::intent_agent::agent::IntentModel`] 接入真实大模型；
//! 本模块提供确定性的中文规则实现，用于：
//! - 无模型凭据时的可用降级；
//! - 单元测试与离线评测的基线；
//! - 对模型输出进行兜底（模型不可用时仍能处理高频指令）。

use std::sync::{Arc, LazyLock};

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{json, Value};

use crate::intent_agent::agent::IntentModel;
use crate::intent_agent::catalog;
use crate::intent_agent::model::{
    Clarification, Condition, ConditionOperator, Decision, DecisionStatus, IntentNode, MatchMode,
    ModelError, ModelInput, ResolvedContext, ToolCall, UnderstandRequest,
};

/// 基于中文关键词与正则的规则模型。
pub struct RuleBasedModel {
    clock: Arc<dyn crate::intent_agent::model::Clock>,
}

impl RuleBasedModel {
    /// 创建规则模型。
    pub fn new(clock: Arc<dyn crate::intent_agent::model::Clock>) -> Self {
        Self { clock }
    }
}

impl IntentModel for RuleBasedModel {
    fn name(&self) -> &'static str {
        "rule-based-v1"
    }

    fn generate(&self, input: &ModelInput<'_>) -> Result<String, ModelError> {
        let decision = parse(input.request, input.context, self.clock.now());
        serde_json::to_string(&decision).map_err(|error| ModelError::Output(error.to_string()))
    }
}

struct ParsedIntent {
    intent_type: &'static str,
    summary: String,
    confidence: f64,
    conditions: Vec<Condition>,
    calls: Vec<ParsedCall>,
}

struct ParsedCall {
    capability: &'static str,
    parameters: Value,
    description: String,
}

/// 使用规则解析一次请求，返回结构化决策树。
pub fn parse(
    request: &UnderstandRequest,
    context: &ResolvedContext,
    now: DateTime<Utc>,
) -> Decision {
    let text = request.normalized_text();
    if text.is_empty() {
        return clarification_decision(
            "请输入要执行的操作，例如：查询订单详情、修改收货地址、退款某个商品。",
            "text",
            "输入为空",
        );
    }

    let segments = split_multi_intent(text);
    if segments.is_empty() {
        return clarification_decision("请补充更具体的操作描述。", "text", "无法拆分意图");
    }
    if segments.len() > 10 {
        return clarification_decision(
            "一次最多处理 10 个意图，请拆分后再试。",
            "text",
            "意图数量超过上限",
        );
    }

    let mut intents = Vec::new();
    let mut previous_last_call: Option<String> = None;
    for (index, segment) in segments.iter().enumerate() {
        let parsed = match parse_single(segment, context, now) {
            Ok(parsed) => parsed,
            Err(clarification) => {
                return Decision {
                    status: DecisionStatus::NeedsClarification,
                    intents: vec![],
                    execution_order: vec![],
                    clarification: Some(clarification),
                }
            }
        };
        let mut intent = build_intent(index, parsed);
        if let Some(previous) = previous_last_call.take() {
            if let Some(first_call) = intent.tool_calls.first_mut() {
                first_call.depends_on.push(previous);
            }
        }
        previous_last_call = intent.tool_calls.last().map(|call| call.call_id.clone());
        intents.push(intent);
    }

    let execution_order = intents
        .iter()
        .map(|intent| intent.intent_id.clone())
        .collect();
    Decision {
        status: DecisionStatus::Ready,
        intents,
        execution_order,
        clarification: None,
    }
}

fn parse_single(
    text: &str,
    context: &ResolvedContext,
    _now: DateTime<Utc>,
) -> Result<ParsedIntent, Clarification> {
    if is_address_update(text) {
        return parse_address_update(text, context);
    }
    if text.contains('退') {
        return parse_refund(text);
    }
    if text.contains('删') || text.contains("移除") || text.contains("清除") {
        return parse_delete(text, context);
    }
    if text.contains("余额") {
        return Ok(ParsedIntent {
            intent_type: "QUERY_BALANCE",
            summary: truncate_chars(text, 200),
            confidence: 0.95,
            conditions: vec![],
            calls: vec![ParsedCall {
                capability: "query_user_balance",
                parameters: json!({}),
                description: "查询账户余额".to_string(),
            }],
        });
    }
    if text.contains("发货") {
        return parse_ship(text, context);
    }
    if text.contains("取消") {
        return parse_cancel(text, context);
    }
    if text.contains("订单")
        && (text.contains('查') || text.contains("详情") || text.contains("状态"))
    {
        return parse_order_query(text, context);
    }

    Err(clarification(
        "请补充要操作的对象和动作，例如：查询订单详情、修改收货地址、退款某个商品。",
        "intent",
    ))
}

fn is_address_update(text: &str) -> bool {
    text.contains("地址") && (text.contains('改') || text.contains('换') || text.contains("设置"))
}

fn parse_address_update(
    text: &str,
    context: &ResolvedContext,
) -> Result<ParsedIntent, Clarification> {
    let order_id = extract_identifier(text, "ORD_").or_else(|| {
        context
            .entity
            .as_ref()
            .filter(|entity| entity.entity_type == "order")
            .map(|entity| entity.id.clone())
    });
    let Some(order_id) = order_id else {
        return Err(clarification("请先选择或提供要修改的订单号。", "order_id"));
    };
    let Some(new_address) = extract_address(text) else {
        return Err(clarification("请补充新的收货地址。", "new_address"));
    };
    Ok(ParsedIntent {
        intent_type: "UPDATE_ADDRESS",
        summary: truncate_chars(text, 200),
        confidence: 0.95,
        conditions: vec![],
        calls: vec![ParsedCall {
            capability: "update_order_address",
            parameters: json!({"order_id": order_id, "new_address": new_address}),
            description: "修改订单收货地址".to_string(),
        }],
    })
}

fn parse_refund(text: &str) -> Result<ParsedIntent, Clarification> {
    let mut conditions = vec![Condition {
        field: "entity_type".to_string(),
        operator: ConditionOperator::Eq,
        value: json!("order_item"),
    }];
    conditions.extend(parse_date_conditions(text));
    conditions.extend(parse_price_conditions(text));

    let status = if text.contains("退货") {
        "RETURNED"
    } else {
        "REFUNDED"
    };
    let mut parameters = json!({"status": status});
    if let Some(item_id) = extract_identifier(text, "ITM_") {
        parameters["item_id"] = json!(item_id);
    }

    Ok(ParsedIntent {
        intent_type: "REFUND",
        summary: truncate_chars(text, 200),
        confidence: if conditions.len() > 1 { 0.92 } else { 0.82 },
        conditions,
        calls: vec![ParsedCall {
            capability: "update_item_status",
            parameters,
            description: format!("将订单项状态更新为 {status}"),
        }],
    })
}

fn parse_delete(text: &str, context: &ResolvedContext) -> Result<ParsedIntent, Clarification> {
    let mut entity_type = infer_delete_entity_type(text, context);
    if entity_type == "unknown" {
        return Err(clarification(
            "请说明要删除的对象类型（订单、订单项或商品）。",
            "entity_type",
        ));
    }
    let mut parameters = json!({"entity_type": entity_type.clone()});
    if let Some(entity_id) = extract_any_identifier(text) {
        entity_type = infer_entity_type_from_id(&entity_id);
        parameters["entity_type"] = json!(entity_type);
        parameters["entity_id"] = json!(entity_id);
    }
    Ok(ParsedIntent {
        intent_type: "DELETE",
        summary: truncate_chars(text, 200),
        confidence: 0.9,
        conditions: vec![],
        calls: vec![ParsedCall {
            capability: "delete_record",
            parameters,
            description: "删除业务记录".to_string(),
        }],
    })
}

fn infer_delete_entity_type(text: &str, context: &ResolvedContext) -> String {
    if text.contains("订单项") {
        return "order_item".to_string();
    }
    if text.contains("商品") {
        return "product".to_string();
    }
    if text.contains("订单") {
        return "order".to_string();
    }
    context
        .entity
        .as_ref()
        .map(|entity| entity.entity_type.clone())
        .filter(|entity_type| entity_type != "unknown")
        .unwrap_or_else(|| "unknown".to_string())
}

fn parse_ship(text: &str, context: &ResolvedContext) -> Result<ParsedIntent, Clarification> {
    let item_id = extract_identifier(text, "ITM_").or_else(|| {
        context
            .entity
            .as_ref()
            .filter(|entity| entity.entity_type == "order_item")
            .map(|entity| entity.id.clone())
    });
    let Some(item_id) = item_id else {
        return Err(clarification("请指定要发货的订单项。", "item_id"));
    };
    Ok(ParsedIntent {
        intent_type: "SHIP",
        summary: truncate_chars(text, 200),
        confidence: 0.9,
        conditions: vec![],
        calls: vec![ParsedCall {
            capability: "update_item_status",
            parameters: json!({"item_id": item_id, "status": "SHIPPED"}),
            description: "订单项发货".to_string(),
        }],
    })
}

fn parse_cancel(text: &str, context: &ResolvedContext) -> Result<ParsedIntent, Clarification> {
    if text.contains("订单")
        && !text.contains("订单项")
        && context
            .entity
            .as_ref()
            .is_some_and(|entity| entity.entity_type == "order")
    {
        return Err(clarification(
            "取消订单需要指定具体订单项，请选择要取消的订单项。",
            "item_id",
        ));
    }
    let item_id = extract_identifier(text, "ITM_").or_else(|| {
        context
            .entity
            .as_ref()
            .filter(|entity| entity.entity_type == "order_item")
            .map(|entity| entity.id.clone())
    });
    let Some(item_id) = item_id else {
        return Err(clarification("请指定要取消的订单项。", "item_id"));
    };
    Ok(ParsedIntent {
        intent_type: "CANCEL",
        summary: truncate_chars(text, 200),
        confidence: 0.88,
        conditions: vec![],
        calls: vec![ParsedCall {
            capability: "update_item_status",
            parameters: json!({"item_id": item_id, "status": "CANCELLED"}),
            description: "取消订单项".to_string(),
        }],
    })
}

fn parse_order_query(text: &str, context: &ResolvedContext) -> Result<ParsedIntent, Clarification> {
    let order_id = extract_identifier(text, "ORD_").or_else(|| {
        context
            .entity
            .as_ref()
            .filter(|entity| entity.entity_type == "order")
            .map(|entity| entity.id.clone())
    });
    let Some(order_id) = order_id else {
        return Err(clarification("请提供要查询的订单号。", "order_id"));
    };
    Ok(ParsedIntent {
        intent_type: "QUERY_ORDER",
        summary: truncate_chars(text, 200),
        confidence: 0.92,
        conditions: vec![],
        calls: vec![ParsedCall {
            capability: "query_order_business_data",
            parameters: json!({"order_id": order_id}),
            description: "查询订单业务数据".to_string(),
        }],
    })
}

fn build_intent(index: usize, parsed: ParsedIntent) -> IntentNode {
    let requires_confirmation = parsed
        .calls
        .iter()
        .any(|call| catalog::requires_confirmation(call.capability));
    let mut previous: Option<String> = None;
    let tool_calls = parsed
        .calls
        .into_iter()
        .enumerate()
        .map(|(call_index, call)| {
            let call_id = format!("call_{}_{}", index + 1, call_index + 1);
            let depends_on = previous.iter().cloned().collect();
            previous = Some(call_id.clone());
            ToolCall {
                call_id,
                capability: call.capability.to_string(),
                parameters: call.parameters,
                depends_on,
                description: call.description,
            }
        })
        .collect();
    IntentNode {
        intent_id: format!("intent_{}", index + 1),
        intent_type: parsed.intent_type.to_string(),
        summary: parsed.summary,
        confidence: parsed.confidence,
        requires_confirmation,
        match_mode: MatchMode::All,
        conditions: parsed.conditions,
        tool_calls,
    }
}

fn split_multi_intent(text: &str) -> Vec<String> {
    static SEPARATOR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:然后|接着|随后|并且|同时|；|;|。|，再)").expect("valid separator regex")
    });
    SEPARATOR
        .split(text)
        .map(trim_segment)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn trim_segment(segment: &str) -> String {
    segment
        .trim()
        .trim_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, '，' | ',' | '。' | '.' | '；' | ';' | '、')
        })
        .to_string()
}

fn parse_date_conditions(text: &str) -> Vec<Condition> {
    let value = if text.contains("前天") {
        "day_before_yesterday"
    } else if text.contains("昨天") {
        "yesterday"
    } else if text.contains("今天") {
        "today"
    } else if text.contains("明天") {
        "tomorrow"
    } else {
        return vec![];
    };
    vec![Condition {
        field: "created_at".to_string(),
        operator: ConditionOperator::DateEq,
        value: json!(value),
    }]
}

fn parse_price_conditions(text: &str) -> Vec<Condition> {
    static GTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:不低于|不少于|至少|>=)\s*(\d+(?:\.\d+)?)").expect("valid gte regex")
    });
    static LTE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:不超过|不高于|至多|<=)\s*(\d+(?:\.\d+)?)").expect("valid lte regex")
    });
    static GT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:超过|大于|高于|多于|超|>)\s*(\d+(?:\.\d+)?)").expect("valid gt regex")
    });
    static LT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:不到|小于|低于|少于|<)\s*(\d+(?:\.\d+)?)").expect("valid lt regex")
    });
    static ABOVE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+(?:\.\d+)?)\s*(?:元|块钱|块)\s*(?:以上|及以上)")
            .expect("valid above regex")
    });
    static BELOW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(\d+(?:\.\d+)?)\s*(?:元|块钱|块)\s*(?:以下|及以下)")
            .expect("valid below regex")
    });

    let candidates = [
        (&GTE, ConditionOperator::Gte),
        (&LTE, ConditionOperator::Lte),
        (&GT, ConditionOperator::Gt),
        (&LT, ConditionOperator::Lt),
        (&ABOVE, ConditionOperator::Gte),
        (&BELOW, ConditionOperator::Lte),
    ];
    for (regex, operator) in candidates {
        if let Some(captures) = regex.captures(text) {
            if let Some(value) = captures
                .get(1)
                .and_then(|capture| capture.as_str().parse::<f64>().ok())
            {
                return vec![Condition {
                    field: "price".to_string(),
                    operator,
                    value: json!(value),
                }];
            }
        }
    }
    vec![]
}

fn extract_identifier(text: &str, prefix: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let start = upper.find(prefix)?;
    let rest = &text[start + prefix.len()..];
    let end = rest
        .char_indices()
        .find(|(_, character)| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':'))
        })
        .map(|(index, _)| index)
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(format!("{prefix}{}", &rest[..end]))
}

fn extract_any_identifier(text: &str) -> Option<String> {
    ["ORD_", "ITM_", "PRD_", "USR_"]
        .iter()
        .find_map(|prefix| extract_identifier(text, prefix))
}

fn infer_entity_type_from_id(id: &str) -> String {
    let upper = id.to_ascii_uppercase();
    if upper.starts_with("ORD_") {
        "order".to_string()
    } else if upper.starts_with("ITM_") {
        "order_item".to_string()
    } else if upper.starts_with("PRD_") {
        "product".to_string()
    } else {
        "unknown".to_string()
    }
}

fn extract_address(text: &str) -> Option<String> {
    let start = text.find("地址")?;
    let mut tail = text[start + "地址".len()..].trim();
    tail = tail.trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '：' | ':' | '，' | ',' | '。' | '.')
    });
    for verb in [
        "改成",
        "改为",
        "改到",
        "设置为",
        "设为",
        "换到",
        "换为",
        "换成",
        "变成",
    ] {
        if let Some(rest) = tail.strip_prefix(verb) {
            tail = rest.trim();
            break;
        }
    }
    tail = tail.trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '为' | '是' | '到' | '在')
    });
    let tail = tail.trim_end_matches(|character: char| {
        character.is_whitespace() || matches!(character, '。' | '.' | '，' | ',' | '；' | ';')
    });
    if tail.is_empty() || tail.chars().count() > 200 {
        None
    } else {
        Some(tail.to_string())
    }
}

fn clarification(question: &str, field: &str) -> Clarification {
    Clarification {
        question: question.to_string(),
        field: field.to_string(),
        reason: "信息不足，不能安全地生成函数调用".to_string(),
        options: vec![],
    }
}

fn clarification_decision(question: &str, field: &str, reason: &str) -> Decision {
    Decision {
        status: DecisionStatus::NeedsClarification,
        intents: vec![],
        execution_order: vec![],
        clarification: Some(Clarification {
            question: question.to_string(),
            field: field.to_string(),
            reason: reason.to_string(),
            options: vec![],
        }),
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_agent::model::{EntityRef, PageContext};
    use chrono::TimeZone;
    use serde_json::json;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap()
    }

    fn context_with_order_and_items() -> ResolvedContext {
        ResolvedContext {
            active_page: Some("ORDER_DETAIL_PAGE".to_string()),
            entity: Some(EntityRef {
                id: "ORD_20261014_001".to_string(),
                entity_type: "order".to_string(),
                label: None,
                attributes: json!({}),
                source: "page".to_string(),
            }),
            candidates: vec![
                EntityRef {
                    id: "ORD_20261014_001".to_string(),
                    entity_type: "order".to_string(),
                    label: None,
                    attributes: json!({}),
                    source: "page".to_string(),
                },
                EntityRef {
                    id: "ITM_1".to_string(),
                    entity_type: "order_item".to_string(),
                    label: Some("智能降噪耳机".to_string()),
                    attributes: json!({"price": 299.0, "created_at": "2026-10-21T09:00:00Z"}),
                    source: "data".to_string(),
                },
                EntityRef {
                    id: "ITM_2".to_string(),
                    entity_type: "order_item".to_string(),
                    label: Some("数据线".to_string()),
                    attributes: json!({"price": 29.0, "created_at": "2026-10-21T09:00:00Z"}),
                    source: "data".to_string(),
                },
            ],
            source: Some("page".to_string()),
            data_context_present: true,
        }
    }

    fn request(text: &str) -> UnderstandRequest {
        UnderstandRequest {
            user_id: None,
            text: text.to_string(),
            page_context: Some(PageContext {
                scene_code: Some("ORDER_DETAIL_PAGE".to_string()),
                entity_id: Some("ORD_20261014_001".to_string()),
                entity_type: Some("order".to_string()),
                html: None,
            }),
            data_context: None,
            session_context: None,
        }
    }

    #[test]
    fn parses_refund_example_into_conditions() {
        let decision = parse(
            &request("帮我退掉昨天买的那个超 50 元的商品"),
            &context_with_order_and_items(),
            fixed_now(),
        );
        assert_eq!(decision.status, DecisionStatus::Ready);
        let intent = &decision.intents[0];
        assert_eq!(intent.intent_type, "REFUND");
        assert!(intent.requires_confirmation);
        assert!(intent.conditions.iter().any(|condition| {
            condition.field == "price" && condition.operator == ConditionOperator::Gt
        }));
        assert!(intent
            .conditions
            .iter()
            .any(|condition| condition.field == "created_at"));
        assert_eq!(
            intent.tool_calls[0].capability,
            "update_item_status".to_string()
        );
        assert_eq!(intent.tool_calls[0].parameters["status"], json!("REFUNDED"));
    }

    #[test]
    fn parses_delete_pronoun() {
        let decision = parse(
            &request("把它删了"),
            &context_with_order_and_items(),
            fixed_now(),
        );
        assert_eq!(decision.status, DecisionStatus::Ready);
        assert_eq!(decision.intents[0].intent_type, "DELETE");
        assert_eq!(
            decision.intents[0].tool_calls[0].capability,
            "delete_record".to_string()
        );
        assert_eq!(
            decision.intents[0].tool_calls[0].parameters["entity_type"],
            json!("order")
        );
    }

    #[test]
    fn parses_address_update() {
        let decision = parse(
            &request("把订单 ORD_20261014_001 的地址改成北京市朝阳区建国路 88 号"),
            &context_with_order_and_items(),
            fixed_now(),
        );
        assert_eq!(decision.status, DecisionStatus::Ready);
        let call = &decision.intents[0].tool_calls[0];
        assert_eq!(call.capability, "update_order_address");
        assert_eq!(
            call.parameters["new_address"],
            json!("北京市朝阳区建国路 88 号")
        );
    }

    #[test]
    fn splits_multi_intent_and_keeps_order() {
        let decision = parse(
            &request("先查一下我的余额，然后查询订单 ORD_20261014_001 的详情"),
            &context_with_order_and_items(),
            fixed_now(),
        );
        assert_eq!(decision.status, DecisionStatus::Ready);
        assert_eq!(decision.intents.len(), 2);
        assert_eq!(decision.execution_order, vec!["intent_1", "intent_2"]);
        assert_eq!(
            decision.intents[0].tool_calls[0].capability,
            "query_user_balance"
        );
        assert_eq!(
            decision.intents[1].tool_calls[0].capability,
            "query_order_business_data"
        );
        assert_eq!(
            decision.intents[1].tool_calls[0].depends_on,
            vec!["call_1_1".to_string()]
        );
    }

    #[test]
    fn unknown_request_generates_clarification() {
        let decision = parse(
            &request("帮我弄一下"),
            &context_with_order_and_items(),
            fixed_now(),
        );
        assert_eq!(decision.status, DecisionStatus::NeedsClarification);
        assert!(decision.clarification.is_some());
        assert!(decision.intents.is_empty());
    }

    #[test]
    fn parses_price_and_date_conditions() {
        let conditions = parse_price_conditions("超过 50 元");
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].operator, ConditionOperator::Gt);
        let dates = parse_date_conditions("昨天买的");
        assert_eq!(dates.len(), 1);
        assert_eq!(dates[0].value, json!("yesterday"));
    }

    #[test]
    fn does_not_treat_不超过_as_greater_than() {
        let conditions = parse_price_conditions("不超过 50 元");
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].operator, ConditionOperator::Lte);
        assert_eq!(conditions[0].value, json!(50.0));
    }
}
