//! 意图规划器：把自然语言 / UI 事件树转换为函数调用序列。
//!
//! 当前实现是确定性的规则规划器，作为 REQ-202610-1180「Tool Call Agent」
//! 落地前的可联调实现；[`IntentPlanner`] 是稳定的编排端口，后续模型版
//! Agent 只需实现同一 trait，即可替换本模块而不改路由与执行编排。

use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::capabilities::{
    self, DELETE_RECORD, QUERY_ORDER_BUSINESS_DATA, QUERY_USER_BALANCE, UPDATE_ITEM_STATUS,
    UPDATE_ORDER_ADDRESS,
};
use super::model::{IntentExecuteRequest, SessionContext};

/// 规划错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// 输入结构或参数非法。
    #[error("{0}")]
    Invalid(String),
    /// 当前规则规划器无法理解该意图。
    #[error("{0}")]
    Unsupported(String),
}

/// 单条函数调用。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedChange {
    /// 目标能力。
    pub capability: String,
    /// 实体类型。
    pub entity_type: String,
    /// 实体标识。
    pub entity_id: String,
    /// 调用参数。
    pub parameters: Value,
    /// 供确认卡片展示的中文描述。
    pub description: String,
}

/// 规划结果。
#[derive(Debug, Clone, PartialEq)]
pub struct IntentPlan {
    /// 意图摘要。
    pub intent_summary: String,
    /// 函数调用序列。
    pub changes: Vec<PlannedChange>,
    /// 从 UI 事件树解析出的上下文提示。
    pub context_hint: Option<SessionContext>,
}

/// 意图规划端口（Tool Call Agent 适配点）。
pub trait IntentPlanner: Send + Sync {
    /// 根据请求与会话上下文生成调用序列。
    fn plan(
        &self,
        request: &IntentExecuteRequest,
        context: &SessionContext,
    ) -> Result<IntentPlan, PlanError>;
}

/// 规则版规划器（REQ-202610-1180 之前的默认实现）。
#[derive(Debug, Default, Clone, Copy)]
pub struct RuleBasedIntentPlanner;

impl IntentPlanner for RuleBasedIntentPlanner {
    fn plan(
        &self,
        request: &IntentExecuteRequest,
        context: &SessionContext,
    ) -> Result<IntentPlan, PlanError> {
        match request.input_type {
            super::model::InputType::NaturalLanguage => plan_natural_language(request, context),
            super::model::InputType::UiInteraction => plan_ui_interaction(request, context),
        }
    }
}

// ---------------------------------------------------------------------------
// 自然语言
// ---------------------------------------------------------------------------

fn plan_natural_language(
    request: &IntentExecuteRequest,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let text = request
        .raw_input
        .as_str()
        .ok_or_else(|| {
            PlanError::Invalid("raw_input must be a string for NATURAL_LANGUAGE".into())
        })?
        .trim();
    if text.is_empty() {
        return Err(PlanError::Invalid("raw_input must not be empty".into()));
    }
    if text.chars().count() > 2000 {
        return Err(PlanError::Invalid(
            "raw_input must be at most 2000 characters".into(),
        ));
    }

    if is_address_update(text) {
        return plan_address_update(text, context);
    }
    if text.contains("删") {
        return plan_delete(text, request, context);
    }
    if text.contains("余额") {
        return Ok(plan_balance(request));
    }
    if is_order_query(text) {
        return plan_order_query(text, context);
    }
    if let Some(plan) = plan_item_status(text, context) {
        return plan;
    }

    Err(PlanError::Unsupported(
        "暂时无法理解该意图，请补充要操作的页面、对象或动作".into(),
    ))
}

fn is_address_update(text: &str) -> bool {
    text.contains("地址")
        && ["修改", "改", "换", "设置", "设为"]
            .iter()
            .any(|keyword| text.contains(keyword))
}

fn plan_address_update(text: &str, context: &SessionContext) -> Result<IntentPlan, PlanError> {
    let order_id = extract_identifier(text, "ORD_")
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ORD_"))
        .ok_or_else(|| PlanError::Invalid("请先选择要修改的订单，或在输入中提供订单号".into()))?;
    let new_address =
        extract_address(text).ok_or_else(|| PlanError::Invalid("请提供新的收货地址".into()))?;

    Ok(IntentPlan {
        intent_summary: "更新订单收货地址".to_string(),
        changes: vec![PlannedChange {
            capability: UPDATE_ORDER_ADDRESS.to_string(),
            entity_type: "order".to_string(),
            entity_id: order_id.clone(),
            parameters: json!({ "order_id": order_id, "new_address": new_address }),
            description: format!("将订单 {order_id} 的收货地址修改为【{new_address}】"),
        }],
        context_hint: Some(SessionContext {
            active_page: Some("ORDER_DETAIL_PAGE".to_string()),
            entity_id: Some(order_id),
        }),
    })
}

fn plan_balance(request: &IntentExecuteRequest) -> IntentPlan {
    let user_id = request.user_id.clone().unwrap_or_default();
    IntentPlan {
        intent_summary: "查询账户余额".to_string(),
        changes: vec![PlannedChange {
            capability: QUERY_USER_BALANCE.to_string(),
            entity_type: "user".to_string(),
            entity_id: user_id,
            parameters: json!({}),
            description: "查询账户余额".to_string(),
        }],
        context_hint: None,
    }
}

fn is_order_query(text: &str) -> bool {
    text.contains("订单")
        && ["查", "查看", "详情", "状态"]
            .iter()
            .any(|keyword| text.contains(keyword))
}

fn plan_order_query(text: &str, context: &SessionContext) -> Result<IntentPlan, PlanError> {
    let order_id = extract_identifier(text, "ORD_")
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ORD_"))
        .ok_or_else(|| PlanError::Invalid("请先选择订单或提供订单号".into()))?;
    Ok(IntentPlan {
        intent_summary: "查看订单详情".to_string(),
        changes: vec![PlannedChange {
            capability: QUERY_ORDER_BUSINESS_DATA.to_string(),
            entity_type: "order".to_string(),
            entity_id: order_id.clone(),
            parameters: json!({ "order_id": order_id }),
            description: format!("查看订单 {order_id} 的业务数据"),
        }],
        context_hint: None,
    })
}

fn plan_delete(
    text: &str,
    _request: &IntentExecuteRequest,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let entity_id = extract_any_identifier(text)
        .or_else(|| context.entity_id.clone())
        .ok_or_else(|| PlanError::Invalid("请先选择要删除的对象".into()))?;
    let entity_type = infer_entity_type(&entity_id, context.active_page.as_deref())
        .ok_or_else(|| PlanError::Invalid("无法确定要删除的对象类型".into()))?;
    let label = capabilities::entity_label(entity_type);
    Ok(IntentPlan {
        intent_summary: format!("删除{label}"),
        changes: vec![PlannedChange {
            capability: DELETE_RECORD.to_string(),
            entity_type: entity_type.to_string(),
            entity_id: entity_id.clone(),
            parameters: json!({ "entity_type": entity_type, "entity_id": entity_id }),
            description: format!("删除{label} {entity_id}"),
        }],
        context_hint: None,
    })
}

fn plan_item_status(text: &str, context: &SessionContext) -> Option<Result<IntentPlan, PlanError>> {
    let status = if text.contains("发货") {
        "SHIPPED"
    } else if text.contains("退款") {
        "REFUNDED"
    } else if text.contains("取消") {
        "CANCELLED"
    } else {
        return None;
    };

    let item_id = extract_identifier(text, "ITM_")
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ITM_"));
    let item_id = match item_id {
        Some(value) => value,
        None => {
            return Some(Err(PlanError::Invalid(
                "请先选择订单项或提供订单项编号".into(),
            )))
        }
    };
    Some(Ok(IntentPlan {
        intent_summary: "更新订单项状态".to_string(),
        changes: vec![PlannedChange {
            capability: UPDATE_ITEM_STATUS.to_string(),
            entity_type: "order_item".to_string(),
            entity_id: item_id.clone(),
            parameters: json!({ "item_id": item_id, "status": status }),
            description: format!("将订单项 {item_id} 的状态更新为 {status}"),
        }],
        context_hint: None,
    }))
}

/// 从「地址」之后提取新地址。
fn extract_address(text: &str) -> Option<String> {
    let position = text.find("地址")?;
    let tail = &text[position + "地址".len()..];
    let tail = tail.trim_start_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '：' | ':' | '，' | ',' | '的')
    });
    let delimiters = [
        "改成",
        "改为",
        "换成",
        "换为",
        "设置为",
        "设为",
        "改到",
        "换到",
        "到",
        "为",
    ];
    let mut earliest: Option<(usize, usize)> = None;
    for delimiter in delimiters {
        if let Some(index) = tail.find(delimiter) {
            if earliest.is_none_or(|(current, _)| index < current) {
                earliest = Some((index, delimiter.len()));
            }
        }
    }
    let value = match earliest {
        Some((index, length)) => &tail[index + length..],
        None => tail,
    };
    let value = value
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '。' | '！'
                        | '？'
                        | '!'
                        | '?'
                        | '；'
                        | ';'
                        | '，'
                        | ','
                        | '"'
                        | '\''
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                )
        })
        .trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn extract_identifier(text: &str, prefix: &str) -> Option<String> {
    let start = text.find(prefix)?;
    let candidate: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .collect();
    if candidate.len() > prefix.len() {
        Some(candidate)
    } else {
        None
    }
}

fn extract_any_identifier(text: &str) -> Option<String> {
    ["ORD_", "ITM_", "PROD_", "OPR_"]
        .iter()
        .find_map(|prefix| extract_identifier(text, prefix))
}

fn infer_entity_type(entity_id: &str, active_page: Option<&str>) -> Option<&'static str> {
    if entity_id.starts_with("ORD_") {
        return Some("order");
    }
    if entity_id.starts_with("ITM_") {
        return Some("order_item");
    }
    if entity_id.starts_with("PROD_") {
        return Some("product");
    }
    if entity_id.starts_with("OPR_") {
        return Some("operation_record");
    }
    match active_page {
        Some("ORDER_DETAIL_PAGE" | "ORDER_LIST_PAGE") => Some("order"),
        Some("PRODUCT_DETAIL_PAGE") => Some("product"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// UI 事件树
// ---------------------------------------------------------------------------

/// UI 交互事件树（PRD §3.3「结构化事件树」）。
///
/// 兼容两种常见写法：
/// - `{ "event": { "type": "TAP", "target_id": "btn_x" }, "path": [...] }`
/// - `{ "event_type": "TAP", "target_id": "btn_x", "path": ["PAGE", "btn_x"] }`
#[derive(Debug, Clone, Deserialize)]
pub struct UiEventTree {
    /// 触发事件（对象或事件类型字符串）。
    #[serde(default)]
    pub event: Option<UiEventValue>,
    /// 从根节点到触发节点的路径（事件树路径），节点可为对象或字符串。
    #[serde(default, alias = "nodes")]
    pub path: Vec<UiNodeValue>,
    /// 事件携带的表单 / 业务数据。
    #[serde(default)]
    pub data: Map<String, Value>,
    /// 当前页面（可选，优先于路径推断）。
    #[serde(default)]
    pub page: Option<String>,
    /// 当前实体（可选，优先于 data 推断）。
    #[serde(default)]
    pub entity_id: Option<String>,
    /// 顶层事件类型（`event` 缺省时使用）。
    #[serde(default)]
    pub event_type: Option<String>,
    /// 顶层触发节点 ID（`event` 缺省时使用）。
    #[serde(default, alias = "target", alias = "node_id")]
    pub target_id: Option<String>,
    /// 顶层目标能力（可选）。
    #[serde(default)]
    pub action: Option<String>,
}

impl UiEventTree {
    fn resolved_event_type(&self) -> Option<&str> {
        match &self.event {
            Some(UiEventValue::Object(event)) => Some(&event.event_type),
            Some(UiEventValue::Text(event_type)) => Some(event_type),
            None => self.event_type.as_deref(),
        }
    }

    fn resolved_target_id(&self) -> Option<&str> {
        match &self.event {
            Some(UiEventValue::Object(event)) => Some(&event.target_id),
            _ => self.target_id.as_deref(),
        }
    }
}

/// UI 事件（对象或字符串）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum UiEventValue {
    /// 结构化事件对象。
    Object(UiEventObject),
    /// 仅事件类型字符串。
    Text(String),
}

/// 结构化 UI 事件。
#[derive(Debug, Clone, Deserialize)]
pub struct UiEventObject {
    /// 事件类型，例如 `TAP` / `SUBMIT` / `CHANGE`。
    #[serde(rename = "type", alias = "event_type")]
    pub event_type: String,
    /// 触发节点 ID。
    pub target_id: String,
}

/// 事件树节点（对象或字符串）。
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum UiNodeValue {
    /// 结构化节点。
    Object(UiNodeObject),
    /// 仅节点 ID 字符串。
    Text(String),
}

impl UiNodeValue {
    fn id(&self) -> &str {
        match self {
            Self::Object(node) => &node.id,
            Self::Text(id) => id,
        }
    }

    fn node_type(&self) -> Option<&str> {
        match self {
            Self::Object(node) => node.node_type.as_deref(),
            Self::Text(_) => None,
        }
    }

    fn action(&self) -> Option<&str> {
        match self {
            Self::Object(node) => node.action.as_deref(),
            Self::Text(_) => None,
        }
    }
}

/// 结构化事件树节点。
#[derive(Debug, Clone, Deserialize)]
pub struct UiNodeObject {
    /// 节点 ID。
    #[serde(alias = "node_id")]
    pub id: String,
    /// 节点类型，例如 `page` / `form` / `button` / `input` / `entity`。
    #[serde(rename = "type", alias = "node_type", default)]
    pub node_type: Option<String>,
    /// 节点声明的目标能力（可选）。
    #[serde(default)]
    pub action: Option<String>,
}

fn plan_ui_interaction(
    request: &IntentExecuteRequest,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let raw_value = match &request.raw_input {
        Value::String(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| request.raw_input.clone())
        }
        other => other.clone(),
    };
    let tree: UiEventTree = serde_json::from_value(raw_value).map_err(|error| {
        PlanError::Invalid(format!("raw_input must be a UI event tree object: {error}"))
    })?;
    validate_event_tree(&tree)?;

    let hint = ui_context_hint(&tree, context);
    let action = resolve_ui_action(&tree).ok_or_else(|| {
        PlanError::Unsupported(
            "无法从 UI 事件中识别可执行操作，请检查节点 action 或 target_id".into(),
        )
    })?;

    let plan = match action.as_str() {
        UPDATE_ORDER_ADDRESS => ui_update_address(&tree, &hint, context)?,
        QUERY_USER_BALANCE => IntentPlan {
            intent_summary: "查询账户余额".to_string(),
            changes: vec![PlannedChange {
                capability: QUERY_USER_BALANCE.to_string(),
                entity_type: "user".to_string(),
                entity_id: request.user_id.clone().unwrap_or_default(),
                parameters: json!({}),
                description: "查询账户余额".to_string(),
            }],
            context_hint: None,
        },
        QUERY_ORDER_BUSINESS_DATA => ui_order_query(&tree, &hint, context)?,
        DELETE_RECORD => ui_delete(&tree, &hint, context)?,
        UPDATE_ITEM_STATUS => ui_update_item_status(&tree, &hint, context)?,
        other => {
            return Err(PlanError::Unsupported(format!(
                "UI 事件声明的能力 `{other}` 暂不支持"
            )))
        }
    };

    Ok(IntentPlan {
        context_hint: Some(hint),
        ..plan
    })
}

fn validate_event_tree(tree: &UiEventTree) -> Result<(), PlanError> {
    let event_type = tree
        .resolved_event_type()
        .ok_or_else(|| PlanError::Invalid("event.type is required".into()))?;
    let target_id = tree
        .resolved_target_id()
        .ok_or_else(|| PlanError::Invalid("event.target_id is required".into()))?;
    validate_text("event.type", event_type, 32)?;
    validate_text("event.target_id", target_id, 128)?;
    if tree.path.len() > 64 {
        return Err(PlanError::Invalid(
            "event path must not exceed 64 nodes".into(),
        ));
    }
    for node in &tree.path {
        validate_text("path[].id", node.id(), 128)?;
        if let Some(node_type) = node.node_type() {
            validate_text("path[].type", node_type, 32)?;
        }
        if let Some(action) = node.action() {
            validate_text("path[].action", action, 64)?;
        }
    }
    if let Some(page) = &tree.page {
        validate_text("page", page, 64)?;
    }
    if let Some(entity_id) = &tree.entity_id {
        validate_text("entity_id", entity_id, 128)?;
    }
    Ok(())
}

fn validate_text(key: &str, value: &str, max_len: usize) -> Result<(), PlanError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_len {
        return Err(PlanError::Invalid(format!(
            "{key} must be 1..={max_len} characters"
        )));
    }
    Ok(())
}

fn ui_context_hint(tree: &UiEventTree, context: &SessionContext) -> SessionContext {
    let page = tree
        .page
        .clone()
        .or_else(|| {
            tree.path
                .iter()
                .find(|node| node.node_type() == Some("page"))
                .map(|node| node.id().to_string())
        })
        .or_else(|| {
            // 扁平写法（path 为字符串数组）通常首节点即页面。
            tree.path
                .first()
                .filter(|node| node.node_type().is_none())
                .map(|node| node.id().to_string())
        });
    let entity_id = tree.entity_id.clone().or_else(|| {
        data_str(
            &tree.data,
            &["entity_id", "order_id", "item_id", "product_id"],
        )
        .map(str::to_string)
    });
    SessionContext {
        active_page: page.or_else(|| context.active_page.clone()),
        entity_id: entity_id.or_else(|| context.entity_id.clone()),
    }
}

fn resolve_ui_action(tree: &UiEventTree) -> Option<String> {
    if let Some(action) = tree.action.as_deref().map(normalize_action) {
        if !action.is_empty() {
            return Some(action);
        }
    }
    for node in tree.path.iter().rev() {
        if let Some(action) = node.action().map(normalize_action) {
            if !action.is_empty() {
                return Some(action);
            }
        }
    }
    let target = tree
        .resolved_target_id()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if target.contains("address") {
        return Some(UPDATE_ORDER_ADDRESS.to_string());
    }
    if target.contains("balance") {
        return Some(QUERY_USER_BALANCE.to_string());
    }
    if target.contains("delete") {
        return Some(DELETE_RECORD.to_string());
    }
    if target.contains("ship") {
        return Some(UPDATE_ITEM_STATUS.to_string());
    }
    if target.contains("order") && (target.contains("detail") || target.contains("view")) {
        return Some(QUERY_ORDER_BUSINESS_DATA.to_string());
    }
    if let Some(action) = data_str(&tree.data, &["action"]).map(normalize_action) {
        if !action.is_empty() {
            return Some(action);
        }
    }
    if data_str(&tree.data, &["new_address", "shipping_address", "address"]).is_some() {
        return Some(UPDATE_ORDER_ADDRESS.to_string());
    }
    if data_str(&tree.data, &["item_id"]).is_some() && data_str(&tree.data, &["status"]).is_some() {
        return Some(UPDATE_ITEM_STATUS.to_string());
    }
    if data_str(&tree.data, &["entity_type"]).is_some()
        && data_str(&tree.data, &["entity_id"]).is_some()
    {
        return Some(DELETE_RECORD.to_string());
    }
    None
}

fn ui_update_address(
    tree: &UiEventTree,
    hint: &SessionContext,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let new_address = data_str(
        &tree.data,
        &["new_address", "shipping_address", "address", "value"],
    )
    .ok_or_else(|| PlanError::Invalid("UI 事件缺少新收货地址".into()))?;
    let order_id = data_str(&tree.data, &["order_id"])
        .map(str::to_string)
        .or_else(|| hint.entity_id.clone())
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ORD_"))
        .ok_or_else(|| PlanError::Invalid("UI 事件缺少订单标识".into()))?;
    Ok(IntentPlan {
        intent_summary: "更新订单收货地址".to_string(),
        changes: vec![PlannedChange {
            capability: UPDATE_ORDER_ADDRESS.to_string(),
            entity_type: "order".to_string(),
            entity_id: order_id.clone(),
            parameters: json!({ "order_id": order_id, "new_address": new_address }),
            description: format!("将订单 {order_id} 的收货地址修改为【{new_address}】"),
        }],
        context_hint: None,
    })
}

fn ui_order_query(
    tree: &UiEventTree,
    hint: &SessionContext,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let order_id = data_str(&tree.data, &["order_id"])
        .map(str::to_string)
        .or_else(|| hint.entity_id.clone())
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ORD_"))
        .ok_or_else(|| PlanError::Invalid("UI 事件缺少订单标识".into()))?;
    Ok(IntentPlan {
        intent_summary: "查看订单详情".to_string(),
        changes: vec![PlannedChange {
            capability: QUERY_ORDER_BUSINESS_DATA.to_string(),
            entity_type: "order".to_string(),
            entity_id: order_id.clone(),
            parameters: json!({ "order_id": order_id }),
            description: format!("查看订单 {order_id} 的业务数据"),
        }],
        context_hint: None,
    })
}

fn ui_delete(
    tree: &UiEventTree,
    hint: &SessionContext,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let entity_id = data_str(
        &tree.data,
        &["entity_id", "order_id", "item_id", "product_id"],
    )
    .map(str::to_string)
    .or_else(|| hint.entity_id.clone())
    .or_else(|| context.entity_id.clone())
    .ok_or_else(|| PlanError::Invalid("UI 事件缺少要删除的实体标识".into()))?;
    let entity_type = data_str(&tree.data, &["entity_type"])
        .map(str::to_string)
        .or_else(|| infer_entity_type(&entity_id, hint.active_page.as_deref()).map(str::to_string))
        .ok_or_else(|| PlanError::Invalid("无法确定要删除的实体类型".into()))?;
    let label = capabilities::entity_label(&entity_type);
    Ok(IntentPlan {
        intent_summary: format!("删除{label}"),
        changes: vec![PlannedChange {
            capability: DELETE_RECORD.to_string(),
            entity_type: entity_type.clone(),
            entity_id: entity_id.clone(),
            parameters: json!({ "entity_type": entity_type, "entity_id": entity_id }),
            description: format!("删除{label} {entity_id}"),
        }],
        context_hint: None,
    })
}

fn ui_update_item_status(
    tree: &UiEventTree,
    hint: &SessionContext,
    context: &SessionContext,
) -> Result<IntentPlan, PlanError> {
    let item_id = data_str(&tree.data, &["item_id"])
        .map(str::to_string)
        .or_else(|| hint.entity_id.clone())
        .or_else(|| context.entity_id.clone())
        .filter(|value| value.starts_with("ITM_"))
        .ok_or_else(|| PlanError::Invalid("UI 事件缺少订单项标识".into()))?;
    let status = data_str(&tree.data, &["status"])
        .map(str::to_string)
        .or_else(|| infer_item_status(tree.resolved_target_id().unwrap_or_default()))
        .ok_or_else(|| PlanError::Invalid("UI 事件缺少目标状态".into()))?;
    Ok(IntentPlan {
        intent_summary: "更新订单项状态".to_string(),
        changes: vec![PlannedChange {
            capability: UPDATE_ITEM_STATUS.to_string(),
            entity_type: "order_item".to_string(),
            entity_id: item_id.clone(),
            parameters: json!({ "item_id": item_id, "status": status }),
            description: format!("将订单项 {item_id} 的状态更新为 {status}"),
        }],
        context_hint: None,
    })
}

fn normalize_action(action: &str) -> String {
    action.trim().to_ascii_lowercase()
}

fn infer_item_status(target_id: &str) -> Option<String> {
    let target = target_id.to_ascii_lowercase();
    if target.contains("ship") {
        Some("SHIPPED".to_string())
    } else if target.contains("refund") {
        Some("REFUNDED".to_string())
    } else if target.contains("cancel") {
        Some("CANCELLED".to_string())
    } else {
        None
    }
}

fn data_str<'a>(data: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        data.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::model::{CurrentContext, InputType};

    fn request(
        input_type: InputType,
        raw_input: Value,
        context: Option<CurrentContext>,
    ) -> IntentExecuteRequest {
        IntentExecuteRequest {
            user_id: Some("usr_9527".to_string()),
            session_id: Some("sess_test".to_string()),
            input_type,
            raw_input,
            current_context: context,
        }
    }

    fn context(active_page: &str, entity_id: &str) -> SessionContext {
        SessionContext {
            active_page: Some(active_page.to_string()),
            entity_id: Some(entity_id.to_string()),
        }
    }

    #[test]
    fn prd_example_maps_to_update_order_address() {
        let req = request(
            InputType::NaturalLanguage,
            json!("把这个订单的收货地址修改为北京市海淀区中关村南大街1号"),
            Some(CurrentContext {
                active_page: Some("ORDER_DETAIL_PAGE".to_string()),
                entity_id: Some("ORD_20261014_001".to_string()),
            }),
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &context("ORDER_DETAIL_PAGE", "ORD_20261014_001"))
            .unwrap();
        assert_eq!(plan.intent_summary, "更新订单收货地址");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].capability, UPDATE_ORDER_ADDRESS);
        assert_eq!(
            plan.changes[0].parameters,
            json!({
                "order_id": "ORD_20261014_001",
                "new_address": "北京市海淀区中关村南大街1号"
            })
        );
        assert!(plan.changes[0]
            .description
            .contains("【北京市海淀区中关村南大街1号】"));
    }

    #[test]
    fn address_extraction_handles_common_phrasings() {
        assert_eq!(
            extract_address("把收货地址改成上海市浦东新区世纪大道100号。").as_deref(),
            Some("上海市浦东新区世纪大道100号")
        );
        assert_eq!(
            extract_address("收货地址：北京市朝阳区望京街道1号").as_deref(),
            Some("北京市朝阳区望京街道1号")
        );
        assert_eq!(extract_address("修改收货地址"), None);
    }

    #[test]
    fn balance_is_read_only_plan() {
        let req = request(
            InputType::NaturalLanguage,
            json!("查一下我的账户余额"),
            None,
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap();
        assert_eq!(plan.changes[0].capability, QUERY_USER_BALANCE);
        assert!(capabilities::is_read_only(&plan.changes[0].capability));
    }

    #[test]
    fn pronoun_delete_uses_session_entity() {
        let req = request(InputType::NaturalLanguage, json!("把它删了"), None);
        let plan = RuleBasedIntentPlanner
            .plan(&req, &context("ORDER_DETAIL_PAGE", "ORD_20261015_002"))
            .unwrap();
        assert_eq!(plan.changes[0].capability, DELETE_RECORD);
        assert_eq!(plan.changes[0].entity_type, "order");
        assert_eq!(plan.changes[0].entity_id, "ORD_20261015_002");
    }

    #[test]
    fn ui_event_tree_with_explicit_action_is_planned() {
        let req = request(
            InputType::UiInteraction,
            json!({
                "event": {"type": "SUBMIT", "target_id": "btn_save_address"},
                "path": [
                    {"id": "ORDER_DETAIL_PAGE", "type": "page"},
                    {"id": "address_form", "type": "form"},
                    {"id": "btn_save_address", "type": "button", "action": "update_order_address"}
                ],
                "data": {"order_id": "ORD_20261014_001", "shipping_address": "北京市朝阳区望京街道1号"}
            }),
            None,
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap();
        assert_eq!(plan.intent_summary, "更新订单收货地址");
        assert_eq!(plan.changes[0].capability, UPDATE_ORDER_ADDRESS);
        let hint = plan.context_hint.unwrap();
        assert_eq!(hint.active_page.as_deref(), Some("ORDER_DETAIL_PAGE"));
        assert_eq!(hint.entity_id.as_deref(), Some("ORD_20261014_001"));
    }

    #[test]
    fn ui_event_tree_infers_action_from_target_id() {
        let req = request(
            InputType::UiInteraction,
            json!({
                "event": {"type": "TAP", "target_id": "btn_query_balance"},
                "path": [{"id": "PROFILE_PAGE", "type": "page"}]
            }),
            None,
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap();
        assert_eq!(plan.changes[0].capability, QUERY_USER_BALANCE);
    }

    #[test]
    fn ui_event_tree_accepts_flat_schema_and_string_path() {
        let req = request(
            InputType::UiInteraction,
            json!({
                "event_type": "TAP",
                "target_id": "btn_save_address",
                "path": ["ORDER_DETAIL_PAGE", "address_form", "btn_save_address"],
                "data": {
                    "order_id": "ORD_20261014_001",
                    "new_address": "北京市东城区东长安街1号"
                }
            }),
            None,
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap();
        assert_eq!(plan.changes[0].capability, UPDATE_ORDER_ADDRESS);
        assert_eq!(
            plan.changes[0].parameters["new_address"],
            "北京市东城区东长安街1号"
        );
        let hint = plan.context_hint.unwrap();
        assert_eq!(hint.active_page.as_deref(), Some("ORDER_DETAIL_PAGE"));
    }

    #[test]
    fn ui_event_tree_infers_address_action_from_data() {
        let req = request(
            InputType::UiInteraction,
            json!({
                "event": {"type": "SUBMIT", "target_id": "btn_submit"},
                "path": [{"id": "ORDER_DETAIL_PAGE", "type": "page"}],
                "data": {
                    "order_id": "ORD_20261014_001",
                    "address": "北京市西城区金融大街1号"
                }
            }),
            None,
        );
        let plan = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap();
        assert_eq!(plan.changes[0].capability, UPDATE_ORDER_ADDRESS);
    }

    #[test]
    fn ui_event_tree_rejects_unknown_action() {
        let req = request(
            InputType::UiInteraction,
            json!({
                "event": {"type": "TAP", "target_id": "btn_unknown"},
                "path": [{"id": "PAGE", "type": "page"}]
            }),
            None,
        );
        let error = RuleBasedIntentPlanner
            .plan(&req, &SessionContext::default())
            .unwrap_err();
        assert!(matches!(error, PlanError::Unsupported(_)));
    }
}
