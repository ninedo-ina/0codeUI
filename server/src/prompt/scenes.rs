//! 内置场景约束（与 REQ-202610-1171 场景注册表对齐）。
//!
//! 拼装器本身不依赖 UI 模块；调用方可以传入任意 [`SceneConstraints`]，
//! 这里的三个内置场景用于默认接入与测试。

use super::model::SceneConstraints;

/// 内置场景编码列表。
pub const BUILTIN_SCENE_CODES: &[&str] = &[
    "ORDER_DETAIL_PAGE",
    "ORDER_LIST_PAGE",
    "PRODUCT_DETAIL_PAGE",
    "FLASH_SALE_PAGE",
];

/// 按场景编码查找内置约束。
pub fn builtin_scene_constraints(scene_code: &str) -> Option<SceneConstraints> {
    match scene_code {
        "ORDER_DETAIL_PAGE" => Some(order_detail_page()),
        "ORDER_LIST_PAGE" => Some(order_list_page()),
        "PRODUCT_DETAIL_PAGE" => Some(product_detail_page()),
        "FLASH_SALE_PAGE" => Some(flash_sale_page()),
        _ => None,
    }
}

fn order_detail_page() -> SceneConstraints {
    SceneConstraints {
        scene_code: "ORDER_DETAIL_PAGE".to_string(),
        display_name: "订单详情页".to_string(),
        required_elements: vec![
            "订单号".to_string(),
            "订单状态".to_string(),
            "商品明细".to_string(),
            "商品数量".to_string(),
            "金额合计".to_string(),
        ],
        allowed_actions: vec!["刷新订单状态".to_string()],
        forbidden: vec![
            "外部脚本、样式、字体或图片".to_string(),
            "收集或提交支付凭据".to_string(),
        ],
        layout_notes: vec![
            "移动端优先，金额右对齐".to_string(),
            "空明细时渲染明确的空状态".to_string(),
        ],
    }
}

fn order_list_page() -> SceneConstraints {
    SceneConstraints {
        scene_code: "ORDER_LIST_PAGE".to_string(),
        display_name: "订单列表页".to_string(),
        required_elements: vec![
            "订单列表".to_string(),
            "每笔订单的状态与金额".to_string(),
            "空列表状态提示".to_string(),
        ],
        allowed_actions: vec!["进入订单详情".to_string()],
        forbidden: vec!["外部脚本、样式、字体或图片".to_string()],
        layout_notes: vec!["列表项保持统一节奏，空数组时渲染空状态".to_string()],
    }
}

fn product_detail_page() -> SceneConstraints {
    SceneConstraints {
        scene_code: "PRODUCT_DETAIL_PAGE".to_string(),
        display_name: "商品详情页".to_string(),
        required_elements: vec![
            "商品名称".to_string(),
            "价格".to_string(),
            "库存状态".to_string(),
            "商品描述".to_string(),
        ],
        allowed_actions: vec!["加入购物车".to_string()],
        forbidden: vec![
            "外部脚本、样式、字体或图片".to_string(),
            "虚构库存或价格".to_string(),
        ],
        layout_notes: vec!["价格与库存状态使用语义化强调".to_string()],
    }
}

fn flash_sale_page() -> SceneConstraints {
    SceneConstraints {
        scene_code: "FLASH_SALE_PAGE".to_string(),
        display_name: "限时抢购页".to_string(),
        required_elements: vec![
            "活动标题".to_string(),
            "抢购价与原价".to_string(),
            "倒计时".to_string(),
            "剩余库存".to_string(),
            "抢购按钮".to_string(),
        ],
        allowed_actions: vec!["立即抢购".to_string()],
        forbidden: vec![
            "外部脚本、样式、字体或图片".to_string(),
            "虚构库存或价格".to_string(),
            "自动提交订单".to_string(),
        ],
        layout_notes: vec![
            "价格与倒计时是视觉焦点，库存进度条紧随其后".to_string(),
            "售罄或活动结束时按钮必须禁用并给出明确文案".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_scenes_are_available() {
        for code in BUILTIN_SCENE_CODES {
            let scene = builtin_scene_constraints(code).expect("scene must exist");
            assert_eq!(&scene.scene_code, code);
            scene.validate().expect("builtin scene must be valid");
        }
    }

    #[test]
    fn unknown_scene_returns_none() {
        assert!(builtin_scene_constraints("UNKNOWN_PAGE").is_none());
    }
}
