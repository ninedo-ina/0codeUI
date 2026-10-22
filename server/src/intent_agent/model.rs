//! 意图理解与 Tool Call Agent 的领域模型（REQ-202610-1180）。
//!
//! 该模块只定义数据契约，不依赖数据库与 HTTP；便于被 #09（意图执行接口）
//! 与 #10（原子能力清单）复用。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 意图理解请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnderstandRequest {
    /// 调用方用户 ID；HTTP 层会强制与鉴权用户一致。
    #[serde(default)]
    pub user_id: Option<String>,
    /// 用户自然语言输入。
    pub text: String,
    /// 当前页面上下文（页面编码、当前实体、HTML 片段）。
    #[serde(default)]
    pub page_context: Option<PageContext>,
    /// 数据上下文（订单、订单项、商品等业务数据）。
    #[serde(default)]
    pub data_context: Option<Value>,
    /// 会话上下文（上一次操作留下的实体引用）。
    #[serde(default)]
    pub session_context: Option<SessionContext>,
}

impl UnderstandRequest {
    /// 去除首尾空白后的输入文本。
    pub fn normalized_text(&self) -> &str {
        self.text.trim()
    }
}

/// 当前页面上下文。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PageContext {
    /// 页面编码，例如 `ORDER_DETAIL_PAGE`。
    #[serde(default)]
    pub scene_code: Option<String>,
    /// 页面当前聚焦的实体 ID。
    #[serde(default)]
    pub entity_id: Option<String>,
    /// 页面当前聚焦的实体类型。
    #[serde(default)]
    pub entity_type: Option<String>,
    /// 页面 HTML 片段（仅用于提取 data-* 属性，不参与持久化）。
    #[serde(default)]
    pub html: Option<String>,
}

/// 会话上下文。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SessionContext {
    /// 当前活动页面编码。
    #[serde(default)]
    pub active_page: Option<String>,
    /// 上一次操作的实体 ID。
    #[serde(default)]
    pub entity_id: Option<String>,
    /// 上一次操作的实体类型。
    #[serde(default)]
    pub entity_type: Option<String>,
}

/// 可被指代的实体引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRef {
    /// 实体 ID。
    pub id: String,
    /// 实体类型（`order` / `order_item` / `product` / `user`）。
    pub entity_type: String,
    /// 展示名称。
    #[serde(default)]
    pub label: Option<String>,
    /// 实体属性（价格、创建时间、状态、订单号等）。
    #[serde(default = "empty_object")]
    pub attributes: Value,
    /// 来源：`page` / `html` / `session` / `data`。
    #[serde(default)]
    pub source: String,
}

impl EntityRef {
    /// 用于澄清选项的展示文案。
    pub fn display_label(&self) -> String {
        match &self.label {
            Some(label) if !label.trim().is_empty() => {
                format!("{}（{}）", label.trim(), self.id)
            }
            _ => self.id.clone(),
        }
    }

    /// 读取属性值（支持点路径）。
    pub fn attribute(&self, field: &str) -> Option<&Value> {
        if field == "id" {
            return None;
        }
        let mut current = &self.attributes;
        for segment in field.split('.') {
            current = current.as_object()?.get(segment)?;
        }
        Some(current)
    }
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// 条件操作符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOperator {
    /// 等于。
    Eq,
    /// 不等于。
    Ne,
    /// 大于。
    Gt,
    /// 大于等于。
    Gte,
    /// 小于。
    Lt,
    /// 小于等于。
    Lte,
    /// 包含。
    Contains,
    /// 属于集合。
    In,
    /// 存在且非空。
    Exists,
    /// 日期等于（支持 today / yesterday / ISO 日期）。
    DateEq,
    /// 日期早于。
    DateBefore,
    /// 日期晚于。
    DateAfter,
}

/// 条件组合方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    /// 所有条件都满足。
    #[default]
    All,
    /// 任一条件满足。
    Any,
}

/// 单个筛选条件。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// 字段名，例如 `price`、`created_at`、`status`。
    pub field: String,
    /// 比较操作符。
    pub operator: ConditionOperator,
    /// 比较值。
    pub value: Value,
}

/// 一次函数调用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// 调用 ID，在整棵决策树内唯一。
    pub call_id: String,
    /// 原子能力名称（必须来自 #10 能力清单）。
    pub capability: String,
    /// 能力参数（按能力 JSON Schema 校验）。
    #[serde(default = "empty_object")]
    pub parameters: Value,
    /// 依赖的前序调用 ID。
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// 调用说明。
    pub description: String,
}

/// 一个意图节点（决策树的一个分支）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentNode {
    /// 意图 ID，在整棵决策树内唯一。
    pub intent_id: String,
    /// 意图类型，例如 `REFUND` / `DELETE` / `QUERY`。
    pub intent_type: String,
    /// 意图摘要。
    pub summary: String,
    /// 置信度，范围 0..=1。
    pub confidence: f64,
    /// 是否要求执行前确认；写/删类能力必须为 true。
    pub requires_confirmation: bool,
    /// 条件组合方式。
    #[serde(default)]
    pub match_mode: MatchMode,
    /// 筛选条件。
    #[serde(default)]
    pub conditions: Vec<Condition>,
    /// 该意图对应的函数调用序列。
    pub tool_calls: Vec<ToolCall>,
}

/// 决策状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DecisionStatus {
    /// 解析完成，可执行。
    Ready,
    /// 信息不足，需要澄清。
    NeedsClarification,
    /// 明确拒绝执行。
    Rejected,
}

/// 澄清选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClarificationOption {
    /// 选项展示文案。
    pub label: String,
    /// 选项对应的实体或参数值。
    pub value: Value,
}

/// 澄清追问。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clarification {
    /// 追问文案。
    pub question: String,
    /// 需要补充的字段。
    pub field: String,
    /// 追问原因。
    pub reason: String,
    /// 候选选项。
    #[serde(default)]
    pub options: Vec<ClarificationOption>,
}

/// 结构化决策树。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    /// 决策状态。
    pub status: DecisionStatus,
    /// 意图节点列表。
    #[serde(default)]
    pub intents: Vec<IntentNode>,
    /// 意图执行顺序（必须恰好覆盖所有 intent_id）。
    #[serde(default)]
    pub execution_order: Vec<String>,
    /// 澄清信息（仅 `NEEDS_CLARIFICATION` 时存在）。
    #[serde(default)]
    pub clarification: Option<Clarification>,
}

impl Decision {
    /// 按执行顺序展开全部函数调用。
    pub fn ordered_tool_calls(&self) -> Vec<&ToolCall> {
        let mut calls = Vec::new();
        for intent_id in &self.execution_order {
            if let Some(intent) = self
                .intents
                .iter()
                .find(|item| &item.intent_id == intent_id)
            {
                calls.extend(intent.tool_calls.iter());
            }
        }
        calls
    }
}

/// 解析后的上下文快照。
#[derive(Debug, Clone, Default, Serialize)]
pub struct ResolvedContext {
    /// 当前活动页面。
    pub active_page: Option<String>,
    /// 当前聚焦实体。
    pub entity: Option<EntityRef>,
    /// 可用于指代消解的候选实体。
    pub candidates: Vec<EntityRef>,
    /// 当前实体的来源。
    pub source: Option<String>,
    /// 是否提供了数据上下文。
    pub data_context_present: bool,
}

/// Agent 运行元信息。
#[derive(Debug, Clone, Serialize)]
pub struct AgentMeta {
    /// 模型名称。
    pub model: String,
    /// 实际尝试次数。
    pub attempts: usize,
    /// 是否使用了规则回退模型。
    pub fallback_used: bool,
    /// 耗时（毫秒）。
    pub latency_ms: u64,
}

/// 意图理解响应。
#[derive(Debug, Clone, Serialize)]
pub struct UnderstandResponse {
    /// 决策状态。
    pub status: DecisionStatus,
    /// 结构化决策树（澄清时为 null）。
    pub decision: Option<Decision>,
    /// 澄清追问（成功时为 null）。
    pub clarification: Option<Clarification>,
    /// 解析出的上下文快照。
    pub resolved_context: ResolvedContext,
    /// 运行元信息。
    pub meta: AgentMeta,
    /// 校验错误（用于可观测性与模型重试反馈）。
    #[serde(default)]
    pub validation_errors: Vec<String>,
}

/// 模型输入。
pub struct ModelInput<'a> {
    /// 系统提示词（能力清单 + JSON Schema + 少样本）。
    pub system_prompt: &'a str,
    /// 用户提示词（用户输入 + 上下文 + 上次校验错误）。
    pub user_prompt: &'a str,
    /// 原始请求。
    pub request: &'a UnderstandRequest,
    /// 解析后的上下文。
    pub context: &'a ResolvedContext,
    /// 当前尝试次数（从 1 开始）。
    pub attempt: usize,
}

/// 模型调用错误。
#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelError {
    /// 模型不可用（网络、限流、超时等）。
    #[error("model unavailable: {0}")]
    Unavailable(String),
    /// 模型输出无法使用。
    #[error("model output invalid: {0}")]
    Output(String),
}

/// 意图理解错误（映射为 HTTP 400/500）。
#[derive(Debug, Clone, thiserror::Error)]
pub enum UnderstandError {
    /// 请求参数非法。
    #[error("{0}")]
    Invalid(String),
    /// 内部错误。
    #[error("{0}")]
    Internal(String),
}

/// 当前时间来源，便于测试注入固定时钟。
pub trait Clock: Send + Sync {
    /// 当前 UTC 时间。
    fn now(&self) -> DateTime<Utc>;
}

/// 系统时钟。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
