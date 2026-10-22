//! 静态模板库（编译期内嵌，无运行时文件依赖）。
//!
//! 新增场景时在此登记：场景编码、模板版本、模板正文与视图模型构建函数。
//! 场景编码与 REQ-202610-1171 的场景注册表保持一致。

use serde_json::Value;

use crate::ui_template::{flash_sale, order_detail};
use crate::ui_template::{DeviceInfo, TemplateError};

/// 一个场景模板的定义。
pub struct TemplateDefinition {
    /// 场景编码（请求参数 `scene_code`）。
    pub scene_code: &'static str,
    /// 模板版本（返回体 `template_version`）。
    pub template_version: &'static str,
    /// 模板正文（编译期内嵌）。
    pub template: &'static str,
    /// 业务 JSON → 模板上下文。
    pub build_view: fn(&Value, &DeviceInfo) -> Result<Value, TemplateError>,
}

/// 订单详情模板（首批）。
static ORDER_DETAIL_PAGE: TemplateDefinition = TemplateDefinition {
    scene_code: "ORDER_DETAIL_PAGE",
    template_version: "v1",
    template: include_str!("../../templates/order_detail.html"),
    build_view: order_detail::build_view,
};

/// 限时抢购模板（验收场景二）。
static FLASH_SALE_PAGE: TemplateDefinition = TemplateDefinition {
    scene_code: "FLASH_SALE_PAGE",
    template_version: "v1",
    template: include_str!("../../templates/flash_sale.html"),
    build_view: flash_sale::build_view,
};

/// 全部已注册模板。
static TEMPLATES: &[&TemplateDefinition] = &[&ORDER_DETAIL_PAGE, &FLASH_SALE_PAGE];

/// 按场景编码查找模板。
pub fn find(scene_code: &str) -> Option<&'static TemplateDefinition> {
    TEMPLATES
        .iter()
        .copied()
        .find(|definition| definition.scene_code == scene_code)
}

/// 已注册场景编码列表（用于错误提示与文档）。
pub fn scene_codes() -> Vec<&'static str> {
    TEMPLATES
        .iter()
        .map(|definition| definition.scene_code)
        .collect()
}
