//! Tool Call Agent 编排：模型调用 → JSON Schema 校验 → 上下文绑定 → 参数校验。
//!
//! 设计要点：
//! - 模型通过 [`IntentModel`] 端口注入，默认使用规则模型，生产可替换为真实 LLM；
//! - 非法输出不会进入执行链路：结构错误、语义错误、参数错误都会反馈给模型重试；
//! - 重试耗尽仍不合法时返回 `NEEDS_CLARIFICATION`，而不是猜测；
//! - 指代消解在本地完成，模型只负责语义解析，避免把实体 ID 交给模型编造。

use std::sync::Arc;
use std::time::Instant;

use crate::intent_agent::context::{ContextResolver, ResolutionIssue};
use crate::intent_agent::model::{
    AgentMeta, Clarification, Clock, Decision, DecisionStatus, ModelError, ModelInput,
    ResolvedContext, SystemClock, UnderstandError, UnderstandRequest, UnderstandResponse,
};
use crate::intent_agent::{prompt, rules, schema};

/// 模型端口。
pub trait IntentModel: Send + Sync {
    /// 模型名称（用于可观测性）。
    fn name(&self) -> &'static str {
        "custom"
    }

    /// 生成一次输出。返回原始文本，由 Agent 负责解析与校验。
    fn generate(&self, input: &ModelInput<'_>) -> Result<String, ModelError>;
}

/// 意图理解与 Tool Call Agent。
pub struct ToolCallAgent {
    model: Arc<dyn IntentModel>,
    max_attempts: usize,
    clock: Arc<dyn Clock>,
}

impl ToolCallAgent {
    /// 使用自定义模型构造 Agent；`max_attempts` 会被限制在 1..=3。
    pub fn new(model: Arc<dyn IntentModel>, max_attempts: usize) -> Self {
        Self {
            model,
            max_attempts: max_attempts.clamp(1, 3),
            clock: Arc::new(SystemClock),
        }
    }

    /// 使用自定义模型与时钟构造 Agent（测试用）。
    pub fn with_clock(
        model: Arc<dyn IntentModel>,
        max_attempts: usize,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            model,
            max_attempts: max_attempts.clamp(1, 3),
            clock,
        }
    }

    /// 使用内置规则模型构造 Agent。
    pub fn rule_based(max_attempts: usize) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let model = Arc::new(rules::RuleBasedModel::new(clock.clone()));
        Self {
            model,
            max_attempts: max_attempts.clamp(1, 3),
            clock,
        }
    }

    /// 模型名称。
    pub fn model_name(&self) -> &'static str {
        self.model.name()
    }

    /// 最大尝试次数。
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }

    /// 解析一次用户输入。
    pub fn understand(
        &self,
        request: UnderstandRequest,
    ) -> Result<UnderstandResponse, UnderstandError> {
        validate_request(&request)?;
        let started = Instant::now();
        let now = self.clock.now();
        let context = ContextResolver::new(now).resolve(&request);
        let system_prompt = prompt::system_prompt();
        let mut feedback: Vec<String> = Vec::new();
        let mut all_errors: Vec<String> = Vec::new();
        let mut attempts = 0;

        for attempt in 1..=self.max_attempts {
            attempts = attempt;
            let user_prompt = prompt::user_prompt(&request, &context, attempt, &feedback);
            let input = ModelInput {
                system_prompt: &system_prompt,
                user_prompt: &user_prompt,
                request: &request,
                context: &context,
                attempt,
            };
            let mut attempt_errors = Vec::new();

            let raw = match self.model.generate(&input) {
                Ok(raw) => raw,
                Err(error) => {
                    attempt_errors.push(error.to_string());
                    feedback = attempt_errors.clone();
                    all_errors.extend(attempt_errors);
                    continue;
                }
            };

            let mut decision = match parse_decision(&raw) {
                Ok(decision) => decision,
                Err(error) => {
                    attempt_errors.push(error);
                    feedback = attempt_errors.clone();
                    all_errors.extend(attempt_errors);
                    continue;
                }
            };

            let value = serde_json::to_value(&decision)
                .map_err(|error| UnderstandError::Internal(error.to_string()))?;
            attempt_errors.extend(schema::validate(schema::decision_schema(), &value));
            attempt_errors.extend(schema::validate_decision(&decision));
            if !attempt_errors.is_empty() {
                feedback = attempt_errors.clone();
                all_errors.extend(attempt_errors);
                continue;
            }

            // 模型可能直接判定信息不足或拒绝执行：尊重其状态，不包装成 READY。
            if decision.status != DecisionStatus::Ready {
                return Ok(UnderstandResponse {
                    status: decision.status,
                    decision: None,
                    clarification: decision.clarification,
                    resolved_context: context,
                    meta: self.meta(attempts, started),
                    validation_errors: all_errors,
                });
            }

            let resolver = ContextResolver::new(now);
            let mut resolution_issue = None;
            for intent in &mut decision.intents {
                match resolver.bind_intent(
                    intent,
                    &context,
                    &request.text,
                    request.user_id.as_deref(),
                ) {
                    Ok(()) => {}
                    Err(issue @ ResolutionIssue::Ambiguous { .. }) => {
                        resolution_issue = Some(issue);
                        break;
                    }
                    Err(issue) => {
                        resolution_issue = Some(issue);
                        break;
                    }
                }
            }
            if let Some(issue) = resolution_issue {
                match issue {
                    ResolutionIssue::Ambiguous { .. } => {
                        return Ok(self.clarification_response(
                            issue, context, attempts, all_errors, started,
                        ));
                    }
                    ResolutionIssue::Missing { .. } => {
                        let question = issue_question(&issue);
                        attempt_errors.push(question);
                        feedback = attempt_errors.clone();
                        all_errors.extend(attempt_errors);
                        continue;
                    }
                }
            }

            let parameter_errors = schema::validate_tool_parameters(&decision);
            if !parameter_errors.is_empty() {
                feedback = parameter_errors.clone();
                all_errors.extend(parameter_errors);
                continue;
            }

            return Ok(self.ready_response(decision, context, attempts, all_errors, started));
        }

        let issue = ResolutionIssue::Missing {
            field: "intent".to_string(),
            question: "我还不能确定你的操作对象，请补充更具体的信息（例如订单号、商品名或要执行的动作用词）。"
                .to_string(),
        };
        Ok(self.clarification_response(issue, context, attempts, all_errors, started))
    }

    fn ready_response(
        &self,
        decision: Decision,
        context: ResolvedContext,
        attempts: usize,
        validation_errors: Vec<String>,
        started: Instant,
    ) -> UnderstandResponse {
        UnderstandResponse {
            status: DecisionStatus::Ready,
            decision: Some(decision),
            clarification: None,
            resolved_context: context,
            meta: self.meta(attempts, started),
            validation_errors,
        }
    }

    fn clarification_response(
        &self,
        issue: ResolutionIssue,
        context: ResolvedContext,
        attempts: usize,
        validation_errors: Vec<String>,
        started: Instant,
    ) -> UnderstandResponse {
        UnderstandResponse {
            status: DecisionStatus::NeedsClarification,
            decision: None,
            clarification: Some(issue.to_clarification()),
            resolved_context: context,
            meta: self.meta(attempts, started),
            validation_errors,
        }
    }

    fn meta(&self, attempts: usize, started: Instant) -> AgentMeta {
        AgentMeta {
            model: self.model.name().to_string(),
            attempts,
            fallback_used: self.model.name() == "rule-based-v1",
            latency_ms: started.elapsed().as_millis() as u64,
        }
    }
}

fn parse_decision(raw: &str) -> Result<Decision, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("模型输出为空".to_string());
    }
    if trimmed.starts_with("```") {
        return Err("模型输出包含 Markdown 代码块，必须只输出 JSON 对象".to_string());
    }
    serde_json::from_str(trimmed).map_err(|error| format!("模型输出不是合法 JSON: {error}"))
}

fn issue_question(issue: &ResolutionIssue) -> String {
    match issue {
        ResolutionIssue::Missing { question, .. } | ResolutionIssue::Ambiguous { question, .. } => {
            question.clone()
        }
    }
}

fn validate_request(request: &UnderstandRequest) -> Result<(), UnderstandError> {
    let text_length = request.normalized_text().chars().count();
    if text_length == 0 {
        return Err(UnderstandError::Invalid("text 不能为空".to_string()));
    }
    if text_length > 2000 {
        return Err(UnderstandError::Invalid(
            "text 长度不能超过 2000 个字符".to_string(),
        ));
    }
    if let Some(user_id) = &request.user_id {
        validate_identifier(user_id, "user_id")?;
    }
    if let Some(page) = &request.page_context {
        if let Some(scene_code) = &page.scene_code {
            validate_identifier(scene_code, "page_context.scene_code")?;
        }
        if let Some(entity_id) = &page.entity_id {
            validate_identifier(entity_id, "page_context.entity_id")?;
        }
        if let Some(entity_type) = &page.entity_type {
            validate_identifier(entity_type, "page_context.entity_type")?;
        }
        if let Some(html) = &page.html {
            if html.len() > 65_536 {
                return Err(UnderstandError::Invalid(
                    "page_context.html 不能超过 64KB".to_string(),
                ));
            }
        }
    }
    if let Some(session) = &request.session_context {
        if let Some(active_page) = &session.active_page {
            validate_identifier(active_page, "session_context.active_page")?;
        }
        if let Some(entity_id) = &session.entity_id {
            validate_identifier(entity_id, "session_context.entity_id")?;
        }
        if let Some(entity_type) = &session.entity_type {
            validate_identifier(entity_type, "session_context.entity_type")?;
        }
    }
    if let Some(data_context) = &request.data_context {
        let size = serde_json::to_vec(data_context)
            .map_err(|error| UnderstandError::Invalid(format!("data_context 无法序列化: {error}")))?
            .len();
        if size > 65_536 {
            return Err(UnderstandError::Invalid(
                "data_context 不能超过 64KB".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &str) -> Result<(), UnderstandError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 64 {
        return Err(UnderstandError::Invalid(format!(
            "{field} 长度必须在 1..=64 之间"
        )));
    }
    if !trimmed.chars().all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | ':')
    }) {
        return Err(UnderstandError::Invalid(format!(
            "{field} 只能包含字母、数字、下划线、连字符、点和冒号"
        )));
    }
    Ok(())
}

/// 将 `UnderstandError` 转为统一 API 错误。
impl From<UnderstandError> for crate::error::AppError {
    fn from(error: UnderstandError) -> Self {
        match error {
            UnderstandError::Invalid(message) => crate::error::AppError::invalid_param(message),
            UnderstandError::Internal(message) => crate::error::AppError::internal(message),
        }
    }
}

/// 供路由层使用的澄清构造（保持字段与 Agent 一致）。
pub fn clarification(field: &str, question: &str) -> Clarification {
    Clarification {
        question: question.to_string(),
        field: field.to_string(),
        reason: "信息不足".to_string(),
        options: vec![],
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;
    use crate::intent_agent::model::{PageContext, SystemClock};

    struct ScriptedModel {
        outputs: Mutex<VecDeque<Result<String, ModelError>>>,
        calls: Mutex<usize>,
    }

    impl ScriptedModel {
        fn new(outputs: Vec<Result<String, ModelError>>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into()),
                calls: Mutex::new(0),
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    impl IntentModel for ScriptedModel {
        fn name(&self) -> &'static str {
            "scripted"
        }

        fn generate(&self, _input: &ModelInput<'_>) -> Result<String, ModelError> {
            *self.calls.lock().unwrap() += 1;
            self.outputs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ModelError::Unavailable("no more outputs".to_string())))
        }
    }

    fn request(text: &str) -> UnderstandRequest {
        UnderstandRequest {
            user_id: Some("usr_9527".to_string()),
            text: text.to_string(),
            page_context: Some(PageContext {
                scene_code: Some("ORDER_DETAIL_PAGE".to_string()),
                entity_id: Some("ORD_20261014_001".to_string()),
                entity_type: Some("order".to_string()),
                html: None,
            }),
            data_context: Some(json!({
                "orders": [{
                    "id": "ORD_20261014_001",
                    "items": [
                        {"id": "ITM_1", "name": "耳机", "price": 299.0, "created_at": "2026-10-21T09:00:00Z"},
                        {"id": "ITM_2", "name": "数据线", "price": 29.0, "created_at": "2026-10-21T09:00:00Z"}
                    ]
                }]
            })),
            session_context: None,
        }
    }

    fn valid_delete_json() -> String {
        json!({
            "status": "READY",
            "intents": [{
                "intent_id": "intent_1",
                "intent_type": "DELETE",
                "summary": "删除订单",
                "confidence": 0.9,
                "requires_confirmation": true,
                "match_mode": "all",
                "conditions": [],
                "tool_calls": [{
                    "call_id": "call_1_1",
                    "capability": "delete_record",
                    "parameters": {"entity_type": "order"},
                    "depends_on": [],
                    "description": "删除订单"
                }]
            }],
            "execution_order": ["intent_1"],
            "clarification": null
        })
        .to_string()
    }

    #[test]
    fn rule_based_agent_parses_refund_example() {
        let agent = ToolCallAgent::rule_based(2);
        let response = agent
            .understand(request("帮我退掉昨天买的那个超 50 元的商品"))
            .unwrap();
        assert_eq!(response.status, DecisionStatus::Ready);
        let decision = response.decision.unwrap();
        assert_eq!(decision.intents[0].intent_type, "REFUND");
        assert_eq!(
            decision.intents[0].tool_calls[0].parameters["item_id"],
            json!("ITM_1")
        );
        assert_eq!(response.meta.model, "rule-based-v1");
    }

    #[test]
    fn invalid_output_is_retried_then_succeeds() {
        let model = Arc::new(ScriptedModel::new(vec![
            Ok("not-json".to_string()),
            Ok(valid_delete_json()),
        ]));
        let agent = ToolCallAgent::with_clock(model.clone(), 2, Arc::new(SystemClock));
        let response = agent.understand(request("把订单删了")).unwrap();
        assert_eq!(response.status, DecisionStatus::Ready);
        assert_eq!(response.meta.attempts, 2);
        assert_eq!(model.calls(), 2);
    }

    #[test]
    fn exhausted_invalid_output_becomes_clarification() {
        let model = Arc::new(ScriptedModel::new(vec![
            Ok("not-json".to_string()),
            Ok("still-not-json".to_string()),
        ]));
        let agent = ToolCallAgent::new(model, 2);
        let response = agent.understand(request("把订单删了")).unwrap();
        assert_eq!(response.status, DecisionStatus::NeedsClarification);
        assert!(response.clarification.is_some());
        assert!(!response.validation_errors.is_empty());
    }

    #[test]
    fn model_error_is_retried_then_clarifies() {
        let model = Arc::new(ScriptedModel::new(vec![
            Err(ModelError::Unavailable("timeout".to_string())),
            Err(ModelError::Unavailable("timeout".to_string())),
        ]));
        let agent = ToolCallAgent::new(model, 2);
        let response = agent.understand(request("把订单删了")).unwrap();
        assert_eq!(response.status, DecisionStatus::NeedsClarification);
        assert_eq!(response.meta.attempts, 2);
    }

    #[test]
    fn ambiguous_context_returns_clarification_without_retry() {
        let model = Arc::new(ScriptedModel::new(vec![Ok(valid_delete_json())]));
        let agent = ToolCallAgent::new(model.clone(), 2);
        let mut request = request("把它删了");
        request.page_context = None;
        request.data_context = Some(json!({
            "orders": [{"id": "ORD_1"}, {"id": "ORD_2"}]
        }));
        let response = agent.understand(request).unwrap();
        assert_eq!(response.status, DecisionStatus::NeedsClarification);
        assert_eq!(response.meta.attempts, 1);
        assert_eq!(model.calls(), 1);
        let options = response.clarification.unwrap().options;
        assert_eq!(options.len(), 2);
    }

    #[test]
    fn rejects_empty_text() {
        let agent = ToolCallAgent::rule_based(2);
        let error = agent
            .understand(UnderstandRequest {
                user_id: None,
                text: "   ".to_string(),
                page_context: None,
                data_context: None,
                session_context: None,
            })
            .unwrap_err();
        assert!(matches!(error, UnderstandError::Invalid(_)));
    }

    #[test]
    fn fixed_clock_is_used_for_relative_dates() {
        #[derive(Clone, Copy)]
        struct FixedClock(chrono::DateTime<Utc>);
        impl Clock for FixedClock {
            fn now(&self) -> chrono::DateTime<Utc> {
                self.0
            }
        }
        let clock = Arc::new(FixedClock(
            Utc.with_ymd_and_hms(2026, 10, 22, 8, 0, 0).unwrap(),
        ));
        let model = Arc::new(rules::RuleBasedModel::new(clock.clone()));
        let agent = ToolCallAgent::with_clock(model, 2, clock);
        let response = agent
            .understand(request("帮我退掉昨天买的那个超 50 元的商品"))
            .unwrap();
        assert_eq!(response.status, DecisionStatus::Ready);
        assert_eq!(
            response.decision.unwrap().intents[0].tool_calls[0].parameters["item_id"],
            json!("ITM_1")
        );
    }
}
