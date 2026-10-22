//! 数据访问层（Repository）。
//!
//! 所有 SQL 集中在本模块，向上层（HTTP / 意图引擎）暴露领域模型；
//! 金额在数据库中以「分」存储，在领域层以 [`rust_decimal::Decimal`] 暴露。

mod capabilities;
mod intent_sessions;
mod orders;
mod products;
mod profiles;
mod replay;
mod telemetry;
mod users;

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

use crate::error::AppError;
use crate::models::{
    cents_to_decimal, Order, OrderBusinessData, OrderItem, Product, User, UserBalance,
};

/// 仓储错误。
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// 实体不存在。
    #[error("{entity} not found: {id}")]
    NotFound {
        /// 实体类型（用于错误提示）。
        entity: &'static str,
        /// 实体主键。
        id: String,
    },
    /// 入参非法。
    #[error("invalid request: {0}")]
    Invalid(String),
    /// 唯一键冲突。
    #[error("conflict: {0}")]
    Conflict(String),
    /// 数据库错误。
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

impl From<RepositoryError> for AppError {
    fn from(error: RepositoryError) -> Self {
        match error {
            RepositoryError::NotFound { entity, id } => AppError::new(
                crate::error::ErrorCode::NotFound,
                format!("{entity} not found: {id}"),
            ),
            RepositoryError::Invalid(message) => AppError::invalid_param(message),
            RepositoryError::Conflict(message) => {
                AppError::new(crate::error::ErrorCode::Conflict, message)
            }
            RepositoryError::Database(error) => AppError::internal(error),
        }
    }
}

/// 数据访问入口。
#[derive(Debug, Clone)]
pub struct Repository {
    pool: SqlitePool,
}

impl Repository {
    /// 基于连接池构造仓储。
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 从 [`crate::db::Db`] 构造仓储。
    pub fn from_db(db: &crate::db::Db) -> Self {
        Self::new(db.pool().clone())
    }

    /// 底层连接池。
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// 记录一条原子能力操作审计。
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn record_operation(
        &self,
        capability: &str,
        actor_user_id: Option<&str>,
        entity_type: &str,
        entity_id: &str,
        request_payload: &str,
        result_payload: &str,
    ) -> Result<(), RepositoryError> {
        sqlx::query(
            "INSERT INTO operation_records
                 (id, actor_user_id, capability, entity_type, entity_id,
                  request_payload, result_payload, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'SUCCESS', ?)",
        )
        .bind(new_id("OPR"))
        .bind(actor_user_id)
        .bind(capability)
        .bind(entity_type)
        .bind(entity_id)
        .bind(request_payload)
        .bind(result_payload)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }
}

/// 生成业务主键：`PREFIX_<uuid-simple>`。
pub(crate) fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// 将 sqlx 错误映射为仓储错误（唯一键冲突单独识别）。
pub(crate) fn map_db_error(error: sqlx::Error) -> RepositoryError {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_unique_violation() => {
            RepositoryError::Conflict(database_error.message().to_string())
        }
        other => RepositoryError::Database(other),
    }
}

// ---- 数据库行 → 领域模型 ----

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct UserRow {
    pub id: String,
    pub display_name: String,
    pub balance_cents: i64,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl UserRow {
    pub(crate) fn into_user(self) -> User {
        User {
            id: self.id,
            display_name: self.display_name,
            balance: cents_to_decimal(self.balance_cents),
            currency: self.currency,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    pub(crate) fn into_balance(self) -> UserBalance {
        UserBalance {
            user_id: self.id,
            balance: cents_to_decimal(self.balance_cents),
            currency: self.currency,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ProductRow {
    pub id: String,
    pub name: String,
    pub price_cents: i64,
    pub currency: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProductRow {
    pub(crate) fn into_product(self) -> Product {
        Product {
            id: self.id,
            name: self.name,
            price: cents_to_decimal(self.price_cents),
            currency: self.currency,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct OrderRow {
    pub id: String,
    pub user_id: String,
    pub status: String,
    pub total_cents: i64,
    pub currency: String,
    pub shipping_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrderRow {
    pub(crate) fn into_order(self) -> Order {
        Order {
            id: self.id,
            user_id: self.user_id,
            status: self.status,
            total_amount: cents_to_decimal(self.total_cents),
            currency: self.currency,
            shipping_address: self.shipping_address,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct OrderItemRow {
    pub id: String,
    pub order_id: String,
    pub product_id: Option<String>,
    pub name: String,
    pub unit_price_cents: i64,
    pub qty: i32,
    pub amount_cents: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl OrderItemRow {
    pub(crate) fn into_item(self) -> OrderItem {
        OrderItem {
            id: self.id,
            order_id: self.order_id,
            product_id: self.product_id,
            name: self.name,
            unit_price: cents_to_decimal(self.unit_price_cents),
            qty: self.qty,
            amount: cents_to_decimal(self.amount_cents),
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// 订单 + 订单项查询结果（内部使用）。
pub(crate) struct OrderWithItems {
    pub order: Order,
    pub items: Vec<OrderItem>,
}

impl OrderWithItems {
    pub(crate) fn into_business_data(self) -> OrderBusinessData {
        OrderBusinessData {
            order_id: self.order.id,
            status: self.order.status,
            items: self
                .items
                .into_iter()
                .map(|item| crate::models::OrderBusinessItem {
                    name: item.name,
                    price: item.unit_price,
                    qty: item.qty,
                })
                .collect(),
        }
    }
}
