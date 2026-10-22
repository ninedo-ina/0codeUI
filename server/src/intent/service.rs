//! 意图执行编排（PRD §3.3）。
//!
//! 职责：
//! 1. 校验请求与用户归属，加载 / 合并会话上下文；
//! 2. 调用 [`IntentPlanner`] 生成函数调用序列；
//! 3. 静态校验能力参数，并做实体级归属校验；
//! 4. 读类能力直接执行，写 / 删类只返回确认卡片，绝不自动执行；
//! 5. 回写会话状态（上下文、最近意图、最近动作）。

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde_json::Value;

use crate::error::{AppError, ErrorCode};
use crate::repository::Repository;

use super::capabilities::{self, QUERY_ORDER_BUSINESS_DATA, QUERY_USER_BALANCE};
use super::model::{
    ActionType, ExecutionResult, IntentExecuteRequest, IntentExecuteResponse, IntentSession,
    ProposedCall, ProposedChanges, SessionContext,
};
use super::planner::{IntentPlan, IntentPlanner, PlanError, PlannedChange};

/// 意图执行编排服务。
pub struct IntentService {
    repository: Repository,
    planner: Arc<dyn IntentPlanner>,
    session_ttl: Duration,
}

impl IntentService {
    /// 构造服务。
    pub fn new(
        repository: Repository,
        planner: Arc<dyn IntentPlanner>,
        session_ttl_secs: u64,
    ) -> Self {
        Self {
            repository,
            planner,
            session_ttl: Duration::from_secs(session_ttl_secs.max(1)),
        }
    }

    /// 执行一次意图请求。
    pub async fn execute(
        &self,
        actor_user_id: &str,
        request: IntentExecuteRequest,
    ) -> Result<IntentExecuteResponse, AppError> {
        if actor_user_id.trim().is_empty() {
            return Err(AppError::unauthorized());
        }
        let user_id = self.resolve_user(actor_user_id, request.user_id.as_deref())?;
        let session_id = resolve_session_id(request.session_id.as_deref())?;
        validate_raw_input(&request.raw_input)?;

        let IntentExecuteRequest {
            input_type,
            raw_input,
            current_context,
            ..
        } = request;

        let mut context = SessionContext::default();
        let mut session = None;
        if let Some(existing) = self.repository.find_intent_session(&session_id).await? {
            if existing.user_id != user_id {
                return Err(forbidden_with("会话不属于当前用户"));
            }
            if !self.is_expired(&existing) {
                context = existing.context.clone();
                session = Some(existing);
            }
        }
        if let Some(current) = &current_context {
            validate_context(current)?;
            context.apply(current);
        }

        let normalized = IntentExecuteRequest {
            user_id: Some(user_id.clone()),
            session_id: Some(session_id.clone()),
            input_type,
            raw_input,
            current_context,
        };
        let plan = self
            .planner
            .plan(&normalized, &context)
            .map_err(map_plan_error)?;
        if plan.changes.is_empty() {
            return Err(AppError::internal(
                "planner returned an empty call sequence",
            ));
        }

        for change in &plan.changes {
            self.validate_change(&user_id, change).await?;
        }

        let action_type = if plan
            .changes
            .iter()
            .all(|change| capabilities::is_read_only(&change.capability))
        {
            ActionType::Execute
        } else {
            ActionType::RequireConfirmation
        };

        let execution_results = if action_type == ActionType::Execute {
            let mut results = Vec::with_capacity(plan.changes.len());
            for change in &plan.changes {
                results.push(self.execute_read(&user_id, change).await?);
            }
            results
        } else {
            Vec::new()
        };

        let confirmation_prompt = match action_type {
            ActionType::RequireConfirmation => Some(build_confirmation_prompt(&plan)),
            ActionType::Execute => None,
        };

        let mut final_context = context;
        if let Some(hint) = &plan.context_hint {
            final_context.fill_missing(hint);
        }

        let now = Utc::now();
        let mut session =
            session.unwrap_or_else(|| IntentSession::new(session_id.clone(), user_id.clone(), now));
        session.context = final_context.clone();
        session.last_intent_summary = Some(plan.intent_summary.clone());
        session.last_action_type = Some(action_type);
        session.updated_at = now;
        self.repository.upsert_intent_session(&session).await?;

        let proposed_changes = build_proposed_changes(&plan);

        Ok(IntentExecuteResponse {
            session_id,
            action_type,
            intent_summary: plan.intent_summary,
            proposed_changes,
            confirmation_prompt,
            context: final_context,
            execution_results,
        })
    }

    fn resolve_user(&self, actor: &str, requested: Option<&str>) -> Result<String, AppError> {
        match requested.map(str::trim) {
            None | Some("") => Ok(actor.to_string()),
            Some(value) if value == actor => Ok(actor.to_string()),
            Some(_) => Err(forbidden_with("user_id 与当前登录用户不一致")),
        }
    }

    fn is_expired(&self, session: &IntentSession) -> bool {
        let age = Utc::now().signed_duration_since(session.updated_at);
        let ttl_secs = i64::try_from(self.session_ttl.as_secs()).unwrap_or(i64::MAX);
        age > chrono::Duration::seconds(ttl_secs)
    }

    async fn validate_change(&self, actor: &str, change: &PlannedChange) -> Result<(), AppError> {
        if capabilities::spec(&change.capability).is_none() {
            return Err(AppError::internal(format!(
                "planner produced unknown capability `{}`",
                change.capability
            )));
        }
        capabilities::validate_parameters(&change.capability, &change.parameters)
            .map_err(AppError::invalid_param)?;
        if change.entity_id.trim().is_empty() {
            return Err(AppError::invalid_param("entity_id must not be empty"));
        }
        self.ensure_owned(actor, change).await
    }

    async fn ensure_owned(&self, actor: &str, change: &PlannedChange) -> Result<(), AppError> {
        match change.entity_type.as_str() {
            "order" => self.ensure_order_owned(actor, &change.entity_id).await,
            "order_item" => {
                let item = self.repository.find_order_item(&change.entity_id).await?;
                self.ensure_order_owned(actor, &item.order_id).await
            }
            "user" => {
                if change.entity_id == actor {
                    Ok(())
                } else {
                    Err(forbidden_with("无权访问其他用户的数据"))
                }
            }
            "product" => Err(forbidden_with(
                "商品为全局目录，暂不支持通过意图接口修改或删除",
            )),
            "operation_record" => Err(forbidden_with("审计记录不允许通过意图接口删除")),
            other => Err(AppError::invalid_param(format!(
                "unsupported entity type `{other}`"
            ))),
        }
    }

    async fn ensure_order_owned(&self, actor: &str, order_id: &str) -> Result<(), AppError> {
        let order = self.repository.find_order(order_id).await?;
        if order.user_id == actor {
            Ok(())
        } else {
            Err(forbidden_with("无权操作其他用户的订单"))
        }
    }

    async fn execute_read(
        &self,
        actor: &str,
        change: &PlannedChange,
    ) -> Result<ExecutionResult, AppError> {
        let data = match change.capability.as_str() {
            QUERY_USER_BALANCE => {
                let balance = self.repository.query_user_balance(actor).await?;
                serde_json::to_value(balance).map_err(AppError::internal)?
            }
            QUERY_ORDER_BUSINESS_DATA => {
                let order_id = change
                    .parameters
                    .get("order_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&change.entity_id);
                let data = self.repository.order_business_data(order_id).await?;
                serde_json::to_value(data).map_err(AppError::internal)?
            }
            other => {
                return Err(AppError::internal(format!(
                    "capability `{other}` is not a read capability"
                )))
            }
        };
        Ok(ExecutionResult {
            target_capability: change.capability.clone(),
            status: "SUCCESS".to_string(),
            data,
        })
    }
}

fn resolve_session_id(requested: Option<&str>) -> Result<String, AppError> {
    match requested.map(str::trim) {
        Some(value) if !value.is_empty() => {
            validate_token("session_id", value, 128)?;
            Ok(value.to_string())
        }
        _ => Ok(format!("sess_{}", uuid::Uuid::new_v4().simple())),
    }
}

fn validate_raw_input(raw_input: &Value) -> Result<(), AppError> {
    let size = serde_json::to_vec(raw_input)
        .map_err(AppError::internal)?
        .len();
    if size > 32 * 1024 {
        return Err(AppError::invalid_param("raw_input must be at most 32 KiB"));
    }
    Ok(())
}

fn validate_context(context: &super::model::CurrentContext) -> Result<(), AppError> {
    if let Some(page) = &context.active_page {
        validate_token("current_context.active_page", page, 64)?;
    }
    if let Some(entity_id) = &context.entity_id {
        validate_token("current_context.entity_id", entity_id, 128)?;
    }
    Ok(())
}

fn validate_token(field: &str, value: &str, max_len: usize) -> Result<(), AppError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_len {
        return Err(AppError::invalid_param(format!(
            "{field} must be 1..={max_len} characters"
        )));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "._:-".contains(ch))
    {
        return Err(AppError::invalid_param(format!(
            "{field} may only contain ASCII letters, digits and `._:-`"
        )));
    }
    Ok(())
}

fn build_proposed_changes(plan: &IntentPlan) -> ProposedChanges {
    let mut changes = plan.changes.iter();
    let first = changes.next().expect("plan changes checked non-empty");
    let additional: Vec<ProposedCall> = changes
        .map(|change| ProposedCall {
            target_capability: change.capability.clone(),
            parameters: change.parameters.clone(),
        })
        .collect();
    ProposedChanges {
        target_capability: first.capability.clone(),
        parameters: first.parameters.clone(),
        additional_calls: (!additional.is_empty()).then_some(additional),
    }
}

fn build_confirmation_prompt(plan: &IntentPlan) -> String {
    if plan.changes.len() == 1 {
        format!("确认{}吗？", plan.changes[0].description)
    } else {
        let descriptions = plan
            .changes
            .iter()
            .map(|change| change.description.as_str())
            .collect::<Vec<_>>()
            .join("；");
        format!(
            "确认执行以下 {} 项操作吗？{}",
            plan.changes.len(),
            descriptions
        )
    }
}

fn map_plan_error(error: PlanError) -> AppError {
    match error {
        PlanError::Invalid(message) | PlanError::Unsupported(message) => {
            AppError::invalid_param(message)
        }
    }
}

fn forbidden_with(message: &str) -> AppError {
    AppError::new(ErrorCode::Forbidden, message)
}
