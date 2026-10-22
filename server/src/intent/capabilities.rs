//! 原子能力元数据与参数校验（REQ-202610-1177 的编排适配层）。
//!
//! REQ-202610-1178 将提供可注册的能力中心；本模块先以静态目录承载
//! 首批能力的名称、风险等级与参数约束，供意图编排器使用。后续接入
//! 注册中心时只需替换 [`spec`] / [`validate_parameters`] 的实现。

use serde_json::Value;

use super::model::RiskLevel;

/// 查询用户余额（读）。
pub const QUERY_USER_BALANCE: &str = "query_user_balance";
/// 查询订单业务数据（读）。
pub const QUERY_ORDER_BUSINESS_DATA: &str = "query_order_business_data";
/// 修改订单收货地址（写，PRD §5.2 示例能力名）。
pub const UPDATE_ORDER_ADDRESS: &str = "update_order_address";
/// 更新订单项状态（写）。
pub const UPDATE_ITEM_STATUS: &str = "update_item_status";
/// 删除记录（删）。
pub const DELETE_RECORD: &str = "delete_record";
/// 创建订单（写）。
pub const CREATE_ORDER: &str = "create_order";

/// 能力元数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilitySpec {
    /// 能力名称（函数调用名）。
    pub name: &'static str,
    /// 默认实体类型。
    pub entity_type: &'static str,
    /// 风险等级。
    pub risk_level: RiskLevel,
}

/// 首批能力目录。
pub const CAPABILITIES: &[CapabilitySpec] = &[
    CapabilitySpec {
        name: QUERY_USER_BALANCE,
        entity_type: "user",
        risk_level: RiskLevel::Read,
    },
    CapabilitySpec {
        name: QUERY_ORDER_BUSINESS_DATA,
        entity_type: "order",
        risk_level: RiskLevel::Read,
    },
    CapabilitySpec {
        name: UPDATE_ORDER_ADDRESS,
        entity_type: "order",
        risk_level: RiskLevel::Write,
    },
    CapabilitySpec {
        name: UPDATE_ITEM_STATUS,
        entity_type: "order_item",
        risk_level: RiskLevel::Write,
    },
    CapabilitySpec {
        name: DELETE_RECORD,
        entity_type: "record",
        risk_level: RiskLevel::Delete,
    },
    CapabilitySpec {
        name: CREATE_ORDER,
        entity_type: "order",
        risk_level: RiskLevel::Write,
    },
];

/// 按名称查找能力元数据。
pub fn spec(name: &str) -> Option<CapabilitySpec> {
    CAPABILITIES.iter().copied().find(|item| item.name == name)
}

/// 能力是否可自动执行（读类）。
pub fn is_read_only(name: &str) -> bool {
    spec(name).is_some_and(|item| item.risk_level == RiskLevel::Read)
}

/// 实体类型的中文标签（用于摘要与确认文案）。
pub fn entity_label(entity_type: &str) -> &'static str {
    match entity_type {
        "order" => "订单",
        "order_item" => "订单项",
        "product" => "商品",
        "user" => "用户",
        "record" => "记录",
        _ => "对象",
    }
}

/// 校验能力参数（不访问数据库的静态约束）。
pub fn validate_parameters(name: &str, parameters: &Value) -> Result<(), String> {
    match name {
        QUERY_USER_BALANCE => validate_optional_identifier(parameters, "user_id", 64),
        QUERY_ORDER_BUSINESS_DATA => require_identifier(parameters, "order_id", 128),
        UPDATE_ORDER_ADDRESS => {
            require_identifier(parameters, "order_id", 128)?;
            require_text(parameters, "new_address", 200).map(|_| ())
        }
        UPDATE_ITEM_STATUS => {
            require_identifier(parameters, "item_id", 128)?;
            let status = require_text(parameters, "status", 32)?;
            if !["NORMAL", "SHIPPED", "REFUNDED", "CANCELLED"].contains(&status.as_str()) {
                return Err(format!("unsupported item status `{status}`"));
            }
            Ok(())
        }
        DELETE_RECORD => {
            let entity_type = require_text(parameters, "entity_type", 32)?;
            if !["order", "order_item", "product", "operation_record"]
                .contains(&entity_type.as_str())
            {
                return Err(format!("unsupported entity type `{entity_type}`"));
            }
            require_identifier(parameters, "entity_id", 128)
        }
        CREATE_ORDER => validate_create_order(parameters),
        other => Err(format!("unknown capability `{other}`")),
    }
}

fn validate_create_order(parameters: &Value) -> Result<(), String> {
    let items = parameters
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| "items must be an array".to_string())?;
    if items.is_empty() || items.len() > 100 {
        return Err("items must contain 1..=100 entries".to_string());
    }
    for item in items {
        require_identifier(item, "product_id", 128)?;
        let qty = item
            .get("qty")
            .and_then(Value::as_i64)
            .ok_or_else(|| "qty must be an integer".to_string())?;
        if !(1..=999).contains(&qty) {
            return Err("qty must be within 1..=999".to_string());
        }
    }
    Ok(())
}

fn validate_optional_identifier(
    parameters: &Value,
    key: &str,
    max_len: usize,
) -> Result<(), String> {
    match parameters.get(key) {
        None | Some(Value::Null) => Ok(()),
        Some(Value::String(value)) => validate_identifier_text(key, value, max_len),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn require_identifier(parameters: &Value, key: &str, max_len: usize) -> Result<(), String> {
    let value = require_text(parameters, key, max_len)?;
    validate_identifier_text(key, &value, max_len)
}

fn require_text(parameters: &Value, key: &str, max_len: usize) -> Result<String, String> {
    let value = parameters
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    if value.chars().count() > max_len {
        return Err(format!("{key} must be at most {max_len} characters"));
    }
    Ok(value.to_string())
}

fn validate_identifier_text(key: &str, value: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() || value.chars().count() > max_len {
        return Err(format!("{key} must be 1..={max_len} characters"));
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "._:-".contains(ch))
    {
        return Err(format!(
            "{key} may only contain ASCII letters, digits and `._:-`"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capability_catalog_covers_expected_names() {
        assert_eq!(
            spec(QUERY_USER_BALANCE).unwrap().risk_level,
            RiskLevel::Read
        );
        assert_eq!(
            spec(UPDATE_ORDER_ADDRESS).unwrap().risk_level,
            RiskLevel::Write
        );
        assert_eq!(spec(DELETE_RECORD).unwrap().risk_level, RiskLevel::Delete);
        assert!(spec("missing").is_none());
    }

    #[test]
    fn update_order_address_requires_order_and_new_address() {
        assert!(validate_parameters(
            UPDATE_ORDER_ADDRESS,
            &json!({"order_id": "ORD_1", "new_address": "北京市海淀区"})
        )
        .is_ok());
        assert!(validate_parameters(UPDATE_ORDER_ADDRESS, &json!({"order_id": "ORD_1"})).is_err());
        assert!(validate_parameters(
            UPDATE_ORDER_ADDRESS,
            &json!({"order_id": "ORD_1", "new_address": "   "})
        )
        .is_err());
    }

    #[test]
    fn identifier_charset_is_enforced() {
        assert!(validate_parameters(
            QUERY_ORDER_BUSINESS_DATA,
            &json!({"order_id": "ORD_1; DROP TABLE orders"})
        )
        .is_err());
        assert!(validate_parameters(
            QUERY_ORDER_BUSINESS_DATA,
            &json!({"order_id": "ORD_20261014_001"})
        )
        .is_ok());
    }

    #[test]
    fn item_status_is_whitelisted() {
        assert!(validate_parameters(
            UPDATE_ITEM_STATUS,
            &json!({"item_id": "ITM_1", "status": "SHIPPED"})
        )
        .is_ok());
        assert!(validate_parameters(
            UPDATE_ITEM_STATUS,
            &json!({"item_id": "ITM_1", "status": "BOGUS"})
        )
        .is_err());
    }
}
