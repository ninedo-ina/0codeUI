//! 原子能力清单（对齐 REQ-202610-1178 / #10）。
//!
//! 本模块只保留意图 Agent 所需的能力元数据：名称、风险级别、参数 Schema。
//! 真正的执行逻辑仍由 #09 / #10 的仓储层负责；Agent 不直接访问数据库。

use std::sync::LazyLock;

use serde::Serialize;
use serde_json::{json, Value};

use crate::intent_agent::schema;

/// 能力风险级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    /// 只读。
    Read,
    /// 写入但可逆。
    Write,
    /// 删除或不可逆操作。
    Destructive,
}

/// 能力规格。
#[derive(Debug, Clone)]
pub struct CapabilitySpec {
    /// 能力名称（函数名）。
    pub name: &'static str,
    /// 能力描述。
    pub description: &'static str,
    /// 风险级别。
    pub risk_level: RiskLevel,
    /// 所需权限。
    pub required_permissions: &'static [&'static str],
    /// 参数 JSON Schema。
    pub parameters_schema: Value,
    /// 返回值 JSON Schema。
    pub returns_schema: Value,
}

/// 对外暴露的能力描述（含 OpenAI function calling 兼容结构）。
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityDescriptor {
    /// 能力名称。
    pub name: &'static str,
    /// 能力描述。
    pub description: &'static str,
    /// 风险级别。
    pub risk_level: RiskLevel,
    /// 所需权限。
    pub required_permissions: &'static [&'static str],
    /// 参数 JSON Schema。
    pub parameters: Value,
    /// OpenAI function 描述。
    pub function: FunctionDefinition,
}

/// OpenAI function calling 兼容结构。
#[derive(Debug, Clone, Serialize)]
pub struct FunctionDefinition {
    /// 函数名。
    pub name: &'static str,
    /// 函数描述。
    pub description: &'static str,
    /// 参数 JSON Schema。
    pub parameters: Value,
}

fn string_id_schema(max_length: u32) -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": max_length,
        "pattern": "^[A-Za-z0-9._:-]+$"
    })
}

fn object_schema(properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": required,
        "properties": properties
    })
}

/// 全部能力规格（顺序与 #10 清单一致）。
pub fn specs() -> &'static [CapabilitySpec] {
    static SPECS: LazyLock<Vec<CapabilitySpec>> = LazyLock::new(|| {
        vec![
            CapabilitySpec {
                name: "create_order",
                description: "创建订单，参数包含商品行、收货地址与备注。",
                risk_level: RiskLevel::Write,
                required_permissions: &["order:create"],
                parameters_schema: object_schema(
                    json!({
                        "items": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": 50,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["product_id", "quantity"],
                                "properties": {
                                    "product_id": string_id_schema(64),
                                    "quantity": {"type": "integer", "minimum": 1, "maximum": 999}
                                }
                            }
                        },
                        "shipping_address": {"type": "string", "minLength": 1, "maxLength": 200},
                        "remark": {"type": "string", "maxLength": 500}
                    }),
                    vec!["items"],
                ),
                returns_schema: json!({
                    "type": "object",
                    "required": ["order_id"],
                    "properties": {"order_id": {"type": "string"}}
                }),
            },
            CapabilitySpec {
                name: "query_user_balance",
                description: "查询当前用户或指定用户的账户余额。",
                risk_level: RiskLevel::Read,
                required_permissions: &["user:balance:read"],
                parameters_schema: object_schema(json!({"user_id": string_id_schema(64)}), vec![]),
                returns_schema: json!({
                    "type": "object",
                    "required": ["user_id", "balance", "currency"],
                    "properties": {
                        "user_id": {"type": "string"},
                        "balance": {"type": "string"},
                        "currency": {"type": "string"}
                    }
                }),
            },
            CapabilitySpec {
                name: "query_order_business_data",
                description: "按订单号查询订单业务数据（状态与商品行）。",
                risk_level: RiskLevel::Read,
                required_permissions: &["order:read"],
                parameters_schema: object_schema(
                    json!({"order_id": string_id_schema(64)}),
                    vec!["order_id"],
                ),
                returns_schema: json!({
                    "type": "object",
                    "required": ["order_id", "status", "items"],
                    "properties": {
                        "order_id": {"type": "string"},
                        "status": {"type": "string"},
                        "items": {"type": "array"}
                    }
                }),
            },
            CapabilitySpec {
                name: "update_order_address",
                description: "修改订单收货地址。",
                risk_level: RiskLevel::Write,
                required_permissions: &["order:update"],
                parameters_schema: object_schema(
                    json!({
                        "order_id": string_id_schema(64),
                        "new_address": {"type": "string", "minLength": 1, "maxLength": 200}
                    }),
                    vec!["order_id", "new_address"],
                ),
                returns_schema: json!({
                    "type": "object",
                    "required": ["order_id", "new_address"],
                    "properties": {
                        "order_id": {"type": "string"},
                        "new_address": {"type": "string"}
                    }
                }),
            },
            CapabilitySpec {
                name: "update_item_status",
                description: "更新订单项状态（发货、取消、退款、退货、完成）。",
                risk_level: RiskLevel::Write,
                required_permissions: &["order_item:update"],
                parameters_schema: object_schema(
                    json!({
                        "item_id": string_id_schema(64),
                        "status": {
                            "type": "string",
                            "enum": ["SHIPPED", "CANCELLED", "REFUNDED", "RETURNED", "COMPLETED"]
                        }
                    }),
                    vec!["item_id", "status"],
                ),
                returns_schema: json!({
                    "type": "object",
                    "required": ["item_id", "status"],
                    "properties": {
                        "item_id": {"type": "string"},
                        "status": {"type": "string"}
                    }
                }),
            },
            CapabilitySpec {
                name: "delete_record",
                description: "删除一条业务记录（订单、订单项或商品）。",
                risk_level: RiskLevel::Destructive,
                required_permissions: &["record:delete"],
                parameters_schema: object_schema(
                    json!({
                        "entity_type": {
                            "type": "string",
                            "enum": ["order", "order_item", "product"]
                        },
                        "entity_id": string_id_schema(64),
                        "reason": {"type": "string", "maxLength": 200}
                    }),
                    vec!["entity_type", "entity_id"],
                ),
                returns_schema: json!({
                    "type": "object",
                    "required": ["entity_type", "entity_id", "deleted"],
                    "properties": {
                        "entity_type": {"type": "string"},
                        "entity_id": {"type": "string"},
                        "deleted": {"type": "boolean"}
                    }
                }),
            },
        ]
    });
    SPECS.as_slice()
}

/// 按名称查找能力。
pub fn capability(name: &str) -> Option<&'static CapabilitySpec> {
    specs().iter().find(|spec| spec.name == name)
}

/// 全部能力名称。
pub fn names() -> Vec<&'static str> {
    specs().iter().map(|spec| spec.name).collect()
}

/// 对外能力描述列表。
pub fn descriptors() -> Vec<CapabilityDescriptor> {
    specs()
        .iter()
        .map(|spec| CapabilityDescriptor {
            name: spec.name,
            description: spec.description,
            risk_level: spec.risk_level,
            required_permissions: spec.required_permissions,
            parameters: spec.parameters_schema.clone(),
            function: FunctionDefinition {
                name: spec.name,
                description: spec.description,
                parameters: spec.parameters_schema.clone(),
            },
        })
        .collect()
}

/// 判断能力是否属于写/删类（必须二次确认）。
pub fn requires_confirmation(name: &str) -> bool {
    capability(name)
        .map(|spec| spec.risk_level != RiskLevel::Read)
        .unwrap_or(true)
}

/// 能力对应的默认实体类型。
pub fn entity_type_for(capability: &str, parameters: &Value) -> Option<String> {
    match capability {
        "query_user_balance" => Some("user".to_string()),
        "query_order_business_data" | "update_order_address" | "create_order" => {
            Some("order".to_string())
        }
        "update_item_status" => Some("order_item".to_string()),
        "delete_record" => parameters
            .get("entity_type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(ToString::to_string),
        _ => None,
    }
}

/// 校验能力参数。
pub fn validate_parameters(name: &str, parameters: &Value) -> Result<(), Vec<String>> {
    let spec = capability(name).ok_or_else(|| vec![format!("未知能力 `{name}`")])?;
    let errors = schema::validate(&spec.parameters_schema, parameters);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors
            .into_iter()
            .map(|error| format!("{name}: {error}"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_required_six_capabilities() {
        let names = names();
        assert_eq!(
            names,
            vec![
                "create_order",
                "query_user_balance",
                "query_order_business_data",
                "update_order_address",
                "update_item_status",
                "delete_record"
            ]
        );
    }

    #[test]
    fn write_capabilities_require_confirmation() {
        assert!(!requires_confirmation("query_user_balance"));
        assert!(requires_confirmation("update_item_status"));
        assert!(requires_confirmation("delete_record"));
    }

    #[test]
    fn validates_update_item_status_parameters() {
        assert!(validate_parameters(
            "update_item_status",
            &json!({"item_id": "ITM_1", "status": "REFUNDED"})
        )
        .is_ok());
        let errors = validate_parameters(
            "update_item_status",
            &json!({"item_id": "ITM_1", "status": "UNKNOWN"}),
        )
        .unwrap_err();
        assert!(errors.iter().any(|error| error.contains("enum")));
    }

    #[test]
    fn rejects_unknown_capability() {
        assert!(validate_parameters("drop_database", &json!({})).is_err());
    }
}
