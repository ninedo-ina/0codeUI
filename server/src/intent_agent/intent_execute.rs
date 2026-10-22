//! #09 意图执行接口适配器（REQ-202610-1177）。
//!
//! 把 [`ToolCallAgent`] 的决策树映射为 #09 的 [`IntentPlanner`] 输出：
//! - 自然语言请求走本模块的语义解析、上下文消歧与 Schema 校验；
//! - UI 交互事件树仍交给 #09 的规则规划器，保持既有行为；
//! - 澄清/拒绝转成 [`PlanError::Unsupported`]，由 #09 返回 400 并展示追问。

use serde_json::Value;

use crate::intent::model::{
    InputType, IntentExecuteRequest, SessionContext as ExecuteSessionContext,
};
use crate::intent::planner::{
    IntentPlan, IntentPlanner, PlanError, PlannedChange, RuleBasedIntentPlanner,
};
use crate::intent_agent::catalog;
use crate::intent_agent::model::{
    DecisionStatus, PageContext, SessionContext, UnderstandError, UnderstandRequest,
};
use crate::intent_agent::ToolCallAgent;

/// 基于 [`ToolCallAgent`] 的 #09 规划器。
pub struct ToolCallPlanner {
    agent: ToolCallAgent,
    fallback: RuleBasedIntentPlanner,
}

impl ToolCallPlanner {
    /// 创建适配器。
    pub fn new(agent: ToolCallAgent) -> Self {
        Self {
            agent,
            fallback: RuleBasedIntentPlanner,
        }
    }
}

impl IntentPlanner for ToolCallPlanner {
    fn plan(
        &self,
        request: &IntentExecuteRequest,
        context: &ExecuteSessionContext,
    ) -> Result<IntentPlan, PlanError> {
        if request.input_type != InputType::NaturalLanguage {
            return self.fallback.plan(request, context);
        }

        let text = request.raw_input.as_str().ok_or_else(|| {
            PlanError::Invalid("raw_input must be a string for NATURAL_LANGUAGE".to_string())
        })?;
        let entity_type =
            infer_entity_type(context.active_page.as_deref(), context.entity_id.as_deref());
        let page_context = context.active_page.as_ref().map(|page| PageContext {
            scene_code: Some(page.clone()),
            entity_id: context.entity_id.clone(),
            entity_type: entity_type.clone(),
            html: None,
        });
        let session_context = Some(SessionContext {
            active_page: context.active_page.clone(),
            entity_id: context.entity_id.clone(),
            entity_type,
        });

        let response = self
            .agent
            .understand(UnderstandRequest {
                user_id: request.user_id.clone(),
                text: text.to_string(),
                page_context,
                data_context: None,
                session_context,
            })
            .map_err(map_understand_error)?;

        if response.status != DecisionStatus::Ready {
            let question = response
                .clarification
                .map(|clarification| clarification.question)
                .unwrap_or_else(|| {
                    "暂时无法理解该意图，请补充要操作的页面、对象或动作".to_string()
                });
            return Err(PlanError::Unsupported(question));
        }

        let decision = response
            .decision
            .ok_or_else(|| PlanError::Unsupported("意图解析未返回决策树".to_string()))?;
        let changes = decision
            .ordered_tool_calls()
            .into_iter()
            .map(to_planned_change)
            .collect::<Vec<_>>();
        if changes.is_empty() {
            return Err(PlanError::Unsupported("意图解析未生成函数调用".to_string()));
        }

        let intent_summary = decision
            .intents
            .iter()
            .map(|intent| intent.summary.as_str())
            .collect::<Vec<_>>()
            .join("；");
        let context_hint = if response.resolved_context.active_page.is_some()
            || response.resolved_context.entity.is_some()
        {
            Some(ExecuteSessionContext {
                active_page: response.resolved_context.active_page,
                entity_id: response.resolved_context.entity.map(|entity| entity.id),
            })
        } else {
            None
        };

        Ok(IntentPlan {
            intent_summary,
            changes,
            context_hint,
        })
    }
}

fn to_planned_change(call: &crate::intent_agent::model::ToolCall) -> PlannedChange {
    let entity_type = call
        .parameters
        .get("entity_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| catalog::entity_type_for(&call.capability, &call.parameters))
        .unwrap_or_else(|| "unknown".to_string());
    let entity_id = entity_id_for(&call.capability, &call.parameters);
    let description = if entity_id.is_empty() || call.description.contains(entity_id.as_str()) {
        call.description.clone()
    } else {
        format!("{}（{}）", call.description, entity_id)
    };
    PlannedChange {
        capability: call.capability.clone(),
        entity_type,
        entity_id,
        parameters: call.parameters.clone(),
        description,
    }
}

fn entity_id_for(capability: &str, parameters: &Value) -> String {
    let key = match capability {
        "update_item_status" => "item_id",
        "update_order_address" | "query_order_business_data" | "create_order" => "order_id",
        "delete_record" => "entity_id",
        "query_user_balance" => "user_id",
        _ => return String::new(),
    };
    parameters
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn infer_entity_type(active_page: Option<&str>, entity_id: Option<&str>) -> Option<String> {
    if let Some(page) = active_page {
        if page.contains("ORDER_ITEM") {
            return Some("order_item".to_string());
        }
        if page.contains("ORDER") {
            return Some("order".to_string());
        }
        if page.contains("PRODUCT") {
            return Some("product".to_string());
        }
        if page.contains("USER") {
            return Some("user".to_string());
        }
    }
    match entity_id.and_then(|id| id.split('_').next()) {
        Some("ORD") => Some("order".to_string()),
        Some("ITM") => Some("order_item".to_string()),
        Some("PRD") => Some("product".to_string()),
        Some("USR") => Some("user".to_string()),
        _ => None,
    }
}

fn map_understand_error(error: UnderstandError) -> PlanError {
    match error {
        UnderstandError::Invalid(message) => PlanError::Invalid(message),
        UnderstandError::Internal(message) => PlanError::Unsupported(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent_agent::model::Clock;
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    #[test]
    fn maps_balance_to_execute_plan() {
        let clock: Arc<dyn Clock> = Arc::new(crate::intent_agent::model::SystemClock);
        let planner = ToolCallPlanner::new(ToolCallAgent::with_clock(
            Arc::new(crate::intent_agent::rules::RuleBasedModel::new(
                clock.clone(),
            )),
            2,
            clock,
        ));
        let request = IntentExecuteRequest {
            user_id: Some("usr_9527".to_string()),
            session_id: None,
            input_type: InputType::NaturalLanguage,
            raw_input: serde_json::json!("查一下我的账户余额"),
            current_context: None,
        };
        let plan = planner
            .plan(&request, &ExecuteSessionContext::default())
            .expect("plan");
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].capability, "query_user_balance");
        assert_eq!(plan.changes[0].entity_id, "usr_9527");
    }

    #[test]
    fn maps_delete_with_context_to_planned_change() {
        let now = Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap();
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(now));
        let planner = ToolCallPlanner::new(ToolCallAgent::with_clock(
            Arc::new(crate::intent_agent::rules::RuleBasedModel::new(
                clock.clone(),
            )),
            2,
            clock,
        ));
        let request = IntentExecuteRequest {
            user_id: Some("usr_9527".to_string()),
            session_id: None,
            input_type: InputType::NaturalLanguage,
            raw_input: serde_json::json!("把它删了"),
            current_context: None,
        };
        let context = ExecuteSessionContext {
            active_page: Some("ORDER_DETAIL_PAGE".to_string()),
            entity_id: Some("ORD_20261014_001".to_string()),
        };
        let plan = planner.plan(&request, &context).expect("plan");
        assert_eq!(plan.changes[0].capability, "delete_record");
        assert_eq!(plan.changes[0].entity_type, "order");
        assert_eq!(plan.changes[0].entity_id, "ORD_20261014_001");
    }

    #[derive(Clone, Copy)]
    struct FixedClock(chrono::DateTime<Utc>);

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<Utc> {
            self.0
        }
    }
}
