//! 意图执行接口（REQ-202610-1177）。
//!
//! 分层：
//! - [`model`]：PRD §5.2 请求 / 响应模型与会话状态；
//! - [`capabilities`]：原子能力目录与参数约束（REQ-202610-1178 的适配点）；
//! - [`planner`]：自然语言 / UI 事件树 → 函数调用序列（REQ-202610-1180 的适配点）；
//! - [`service`]：会话上下文、归属校验、执行与确认编排。

pub mod capabilities;
pub mod model;
pub mod planner;
pub mod service;

pub use model::{
    ActionType, CurrentContext, InputType, IntentExecuteRequest, IntentExecuteResponse,
    IntentSession, ProposedChanges, SessionContext,
};
pub use service::IntentService;
