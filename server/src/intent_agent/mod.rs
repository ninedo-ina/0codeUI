//! 意图理解与 Tool Call Agent（REQ-202610-1180 / PRD §4.3）。
//!
//! 模块分层：
//! - [`model`]：请求/响应/决策树数据契约；
//! - [`catalog`]：与 REQ-202610-1178 对齐的原子能力清单；
//! - [`schema`]：JSON Schema 子集校验与决策树语义校验；
//! - [`context`]：页面 HTML、业务数据与指代消解；
//! - [`prompt`]：系统提示词与用户提示词；
//! - [`rules`]：无模型凭据时的确定性规则回退；
//! - [`agent`]：模型调用、重试与澄清编排。

pub mod agent;
pub mod catalog;
pub mod context;
pub mod intent_execute;
pub mod model;
pub mod prompt;
pub mod rules;
pub mod schema;

pub use agent::{IntentModel, ToolCallAgent};
pub use catalog::{CapabilityDescriptor, RiskLevel};
pub use intent_execute::ToolCallPlanner;
pub use model::{
    AgentMeta, Clarification, Condition, ConditionOperator, Decision, DecisionStatus, EntityRef,
    IntentNode, MatchMode, ModelError, ModelInput, PageContext, ResolvedContext, SessionContext,
    ToolCall, UnderstandError, UnderstandRequest, UnderstandResponse,
};
