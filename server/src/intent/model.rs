//! 意图执行接口的领域模型（REQ-202610-1177）。
//!
//! 对齐 PRD §5.2：
//! - 输入：`user_id` / `input_type` / `raw_input` / `current_context`；
//! - 输出：`action_type` / `intent_summary` / `proposed_changes` /
//!   `confirmation_prompt`。
//!
//! 项目统一响应包络仍为 `{code, msg, data}`，§5.2 字段位于 `data` 内。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 输入类型：自然语言 / 前端控件交互事件树。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InputType {
    /// 自然语言（文本 / 语音转写）。
    NaturalLanguage,
    /// 前端控件交互触发的结构化事件树。
    UiInteraction,
}

impl InputType {
    /// 序列化名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NaturalLanguage => "NATURAL_LANGUAGE",
            Self::UiInteraction => "UI_INTERACTION",
        }
    }
}

/// 执行动作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionType {
    /// 读类意图：直接执行。
    Execute,
    /// 写 / 删类意图：仅返回确认卡片，不执行。
    RequireConfirmation,
}

impl ActionType {
    /// 序列化名称。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "EXECUTE",
            Self::RequireConfirmation => "REQUIRE_CONFIRMATION",
        }
    }

    /// 从数据库文本还原（未知值返回 `None`）。
    pub fn from_db(value: &str) -> Option<Self> {
        match value {
            "EXECUTE" => Some(Self::Execute),
            "REQUIRE_CONFIRMATION" => Some(Self::RequireConfirmation),
            _ => None,
        }
    }
}

/// 能力风险等级（决定是否需要二次确认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    /// 读操作：可自动执行。
    Read,
    /// 写操作：必须二次确认。
    Write,
    /// 删操作：必须二次确认。
    Delete,
}

/// 当前上下文（PRD §5.2 `current_context`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentContext {
    /// 当前活动页面编码，例如 `ORDER_DETAIL_PAGE`。
    pub active_page: Option<String>,
    /// 当前选中实体，例如 `ORD_20261014_001`。
    pub entity_id: Option<String>,
}

/// 会话上下文（落库后可在同一会话内延续）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContext {
    /// 当前活动页面编码。
    pub active_page: Option<String>,
    /// 当前选中实体。
    pub entity_id: Option<String>,
}

impl SessionContext {
    /// 用请求上下文覆盖已有值（仅覆盖显式提供的字段）。
    pub fn apply(&mut self, context: &CurrentContext) {
        if context.active_page.is_some() {
            self.active_page.clone_from(&context.active_page);
        }
        if context.entity_id.is_some() {
            self.entity_id.clone_from(&context.entity_id);
        }
    }

    /// 用事件树提示补齐缺失字段（不覆盖已有值）。
    pub fn fill_missing(&mut self, hint: &SessionContext) {
        if self.active_page.is_none() {
            self.active_page.clone_from(&hint.active_page);
        }
        if self.entity_id.is_none() {
            self.entity_id.clone_from(&hint.entity_id);
        }
    }
}

/// 意图执行请求（PRD §5.2 输入参数）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentExecuteRequest {
    /// 用户标识；缺省取鉴权用户，显式提供时必须与鉴权用户一致。
    pub user_id: Option<String>,
    /// 会话标识；缺省由服务端生成。
    pub session_id: Option<String>,
    /// 输入类型。
    pub input_type: InputType,
    /// 自然语言字符串或 UI 事件树对象。
    pub raw_input: Value,
    /// 当前上下文。
    pub current_context: Option<CurrentContext>,
}

/// 会话状态（数据库模型）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentSession {
    /// 会话标识。
    pub session_id: String,
    /// 归属用户。
    pub user_id: String,
    /// 会话上下文。
    pub context: SessionContext,
    /// 最近一次意图摘要。
    pub last_intent_summary: Option<String>,
    /// 最近一次动作类型。
    pub last_action_type: Option<ActionType>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 最近更新时间。
    pub updated_at: DateTime<Utc>,
}

impl IntentSession {
    /// 新建会话。
    pub fn new(session_id: String, user_id: String, now: DateTime<Utc>) -> Self {
        Self {
            session_id,
            user_id,
            context: SessionContext::default(),
            last_intent_summary: None,
            last_action_type: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 单次函数调用（PRD §5.2 `proposed_changes` 的步骤）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedCall {
    /// 目标原子能力。
    pub target_capability: String,
    /// 调用参数。
    pub parameters: Value,
}

/// 提议的变更（PRD §5.2 `proposed_changes`）。
///
/// 单能力场景直接使用 `target_capability` + `parameters`；
/// 多意图场景通过 `additional_calls` 追加后续调用（REQ-202610-1180 扩展）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedChanges {
    /// 首个目标能力。
    pub target_capability: String,
    /// 首个能力的参数。
    pub parameters: Value,
    /// 后续调用序列（仅在多意图时出现）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_calls: Option<Vec<ProposedCall>>,
}

/// 读类能力的执行结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// 目标能力。
    pub target_capability: String,
    /// 执行状态（当前仅 `SUCCESS`）。
    pub status: String,
    /// 能力返回数据。
    pub data: Value,
}

/// 意图执行响应（`data` 内容）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentExecuteResponse {
    /// 会话标识（缺省时由服务端生成）。
    pub session_id: String,
    /// `EXECUTE` 或 `REQUIRE_CONFIRMATION`。
    pub action_type: ActionType,
    /// 意图摘要。
    pub intent_summary: String,
    /// 提议的变更。
    pub proposed_changes: ProposedChanges,
    /// 二次确认文案（仅 `REQUIRE_CONFIRMATION` 时非空）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation_prompt: Option<String>,
    /// 解析后的会话上下文。
    pub context: SessionContext,
    /// 读类能力的执行结果（仅 `EXECUTE` 时非空）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub execution_results: Vec<ExecutionResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_type_round_trips_through_db_text() {
        assert_eq!(ActionType::from_db("EXECUTE"), Some(ActionType::Execute));
        assert_eq!(
            ActionType::from_db("REQUIRE_CONFIRMATION"),
            Some(ActionType::RequireConfirmation)
        );
        assert_eq!(ActionType::from_db("UNKNOWN"), None);
    }

    #[test]
    fn session_context_apply_only_overrides_explicit_fields() {
        let mut context = SessionContext {
            active_page: Some("ORDER_DETAIL_PAGE".to_string()),
            entity_id: Some("ORD_1".to_string()),
        };
        context.apply(&CurrentContext {
            active_page: None,
            entity_id: Some("ORD_2".to_string()),
        });
        assert_eq!(context.active_page.as_deref(), Some("ORDER_DETAIL_PAGE"));
        assert_eq!(context.entity_id.as_deref(), Some("ORD_2"));
    }

    #[test]
    fn session_context_fill_missing_keeps_existing_values() {
        let mut context = SessionContext {
            active_page: Some("PAGE_A".to_string()),
            entity_id: None,
        };
        context.fill_missing(&SessionContext {
            active_page: Some("PAGE_B".to_string()),
            entity_id: Some("E1".to_string()),
        });
        assert_eq!(context.active_page.as_deref(), Some("PAGE_A"));
        assert_eq!(context.entity_id.as_deref(), Some("E1"));
    }
}
