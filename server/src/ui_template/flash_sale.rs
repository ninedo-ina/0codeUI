//! `FLASH_SALE_PAGE` 场景：限时抢购页业务数据校验与视图模型构建。
//!
//! 业务数据契约（验收场景二）：
//! `{ sale_id, title, product_id, price, original_price?, stock_total, stock_remaining,
//!    status, countdown_seconds?, description? }`。
//! 所有字段在写入模板前完成类型、范围与枚举校验。

use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ui_template::{DeviceInfo, TemplateError};

/// 合法活动状态。
const VALID_STATUSES: &[&str] = &["UPCOMING", "LIVE", "ENDED"];

#[derive(Debug, Deserialize)]
struct FlashSaleData {
    sale_id: String,
    title: String,
    product_id: String,
    #[serde(with = "rust_decimal::serde::arbitrary_precision")]
    price: Decimal,
    #[serde(default, with = "rust_decimal::serde::arbitrary_precision_option")]
    original_price: Option<Decimal>,
    stock_total: u32,
    stock_remaining: u32,
    status: String,
    #[serde(default)]
    countdown_seconds: Option<u64>,
    #[serde(default)]
    description: Option<String>,
}

/// 解析并校验业务数据，产出可直接填充模板的视图模型。
pub fn build_view(business_data: &Value, device: &DeviceInfo) -> Result<Value, TemplateError> {
    let data: FlashSaleData = serde_json::from_value(business_data.clone()).map_err(|error| {
        TemplateError::InvalidBusinessData(format!(
            "does not match FLASH_SALE_PAGE contract: {error}"
        ))
    })?;
    validate(&data)?;

    let (status_label, status_class) = status_view(&data.status);
    let sold_out = data.stock_remaining == 0 || data.status == "ENDED";
    let stock_percent = if data.stock_total == 0 {
        0.0
    } else {
        (data.stock_remaining as f64 / data.stock_total as f64 * 100.0).clamp(0.0, 100.0)
    };
    let discount = data.original_price.and_then(|original| {
        if original > data.price && original > Decimal::ZERO {
            Some(format!(
                "{:.1} 折",
                (data.price / original * Decimal::from(10)).round_dp(1)
            ))
        } else {
            None
        }
    });
    let cta_label = match data.status.as_str() {
        "UPCOMING" => "即将开抢",
        "ENDED" => "活动已结束",
        _ if sold_out => "已抢光",
        _ => "立即抢购",
    };
    let countdown = data.countdown_seconds.unwrap_or(0);

    Ok(json!({
        "sale_id": data.sale_id,
        "title": data.title,
        "product_id": data.product_id,
        "price": format_money(data.price),
        "original_price": data.original_price.map(format_money),
        "discount": discount,
        "stock_total": data.stock_total,
        "stock_remaining": data.stock_remaining,
        "stock_percent": format!("{stock_percent:.1}"),
        "status": data.status,
        "status_label": status_label,
        "status_class": status_class,
        "countdown_seconds": countdown,
        "countdown_label": format_countdown(countdown),
        "description": data.description.unwrap_or_default(),
        "currency_symbol": "¥",
        "cta_label": cta_label,
        "cta_disabled": sold_out || data.status != "LIVE",
        "device_class": device_class(device),
    }))
}

fn validate(data: &FlashSaleData) -> Result<(), TemplateError> {
    ensure_text(&data.sale_id, "sale_id", 64)?;
    ensure_text(&data.title, "title", 200)?;
    ensure_text(&data.product_id, "product_id", 64)?;
    ensure_money(data.price, "price")?;
    if let Some(original) = data.original_price {
        ensure_money(original, "original_price")?;
        if original < data.price {
            return Err(TemplateError::InvalidBusinessData(
                "original_price must be >= price".to_string(),
            ));
        }
    }
    if data.stock_total == 0 {
        return Err(TemplateError::InvalidBusinessData(
            "stock_total must be >= 1".to_string(),
        ));
    }
    if data.stock_remaining > data.stock_total {
        return Err(TemplateError::InvalidBusinessData(
            "stock_remaining must be <= stock_total".to_string(),
        ));
    }
    if !VALID_STATUSES.contains(&data.status.as_str()) {
        return Err(TemplateError::InvalidBusinessData(format!(
            "status must be one of {}",
            VALID_STATUSES.join(", ")
        )));
    }
    if let Some(description) = &data.description {
        if description.chars().count() > 2000 {
            return Err(TemplateError::InvalidBusinessData(
                "description must be at most 2000 characters".to_string(),
            ));
        }
    }
    Ok(())
}

fn ensure_text(value: &str, field: &str, max_chars: usize) -> Result<(), TemplateError> {
    if value.trim().is_empty() {
        return Err(TemplateError::InvalidBusinessData(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(TemplateError::InvalidBusinessData(format!(
            "{field} must be at most {max_chars} characters"
        )));
    }
    Ok(())
}

fn ensure_money(value: Decimal, field: &str) -> Result<(), TemplateError> {
    if value < Decimal::ZERO {
        return Err(TemplateError::InvalidBusinessData(format!(
            "{field} must be >= 0"
        )));
    }
    if value.normalize().scale() > 2 {
        return Err(TemplateError::InvalidBusinessData(format!(
            "{field} must have at most 2 decimal places"
        )));
    }
    Ok(())
}

fn status_view(status: &str) -> (&'static str, &'static str) {
    match status {
        "UPCOMING" => ("即将开始", "status-upcoming"),
        "LIVE" => ("抢购进行中", "status-live"),
        "ENDED" => ("活动已结束", "status-ended"),
        _ => ("状态未知", "status-neutral"),
    }
}

fn format_countdown(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    format!("{hours:02}:{minutes:02}:{secs:02}")
}

fn format_money(amount: Decimal) -> String {
    format!("{amount:.2}")
}

fn device_class(device: &DeviceInfo) -> String {
    let platform = match device.platform.trim().to_ascii_lowercase().as_str() {
        "android" => "android",
        "ios" => "ios",
        "harmonyos" | "harmony" => "harmonyos",
        _ => "web",
    };
    let width = match device.screen_width {
        0..=479 => "compact",
        480..=767 => "regular",
        _ => "wide",
    };
    format!("platform-{platform} width-{width}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Value {
        json!({
            "sale_id": "SALE_20261022_001",
            "title": "限时秒杀 · 无线降噪耳机",
            "product_id": "PRD_1001",
            "price": 299.00,
            "original_price": 599.00,
            "stock_total": 1000,
            "stock_remaining": 137,
            "status": "LIVE",
            "countdown_seconds": 3661,
            "description": "限量 1000 件，售完即止。"
        })
    }

    #[test]
    fn builds_view_with_discount_and_countdown() {
        let view = build_view(&data(), &DeviceInfo::default()).unwrap();
        assert_eq!(view["price"], "299.00");
        assert_eq!(view["discount"], "5.0 折");
        assert_eq!(view["countdown_label"], "01:01:01");
        assert_eq!(view["stock_remaining"], 137);
        assert_eq!(view["cta_disabled"], false);
    }

    #[test]
    fn rejects_remaining_greater_than_total() {
        let mut value = data();
        value["stock_remaining"] = json!(1001);
        assert!(build_view(&value, &DeviceInfo::default()).is_err());
    }

    #[test]
    fn sold_out_disables_cta() {
        let mut value = data();
        value["stock_remaining"] = json!(0);
        let view = build_view(&value, &DeviceInfo::default()).unwrap();
        assert_eq!(view["cta_disabled"], true);
        assert_eq!(view["cta_label"], "已抢光");
    }
}
