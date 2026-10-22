//! 场景注册表：`scene_code` → 渲染策略、模板版本与配置。
//!
//! 第一阶段只注册模板场景；动态场景（REQ-202610-1185）后续以
//! [`RenderStrategy::Dynamic`] 加入同一注册表，接口契约保持不变。
//! 注册表是编译期静态数据，查找为 O(n) 且 n 很小，无需额外缓存。

use crate::ui::context::{RenderStrategy, SceneContext, SceneError};

/// 场景渲染函数签名。
pub type SceneRenderer = for<'a, 'b> fn(&'b SceneContext<'a>) -> Result<String, SceneError>;

/// 场景级配置。
#[derive(Debug, Clone, Copy)]
pub struct SceneConfig {
    /// 对外展示名称（日志与错误提示使用）。
    pub display_name: &'static str,
    /// 渲染策略：模板 / 动态。
    pub strategy: RenderStrategy,
    /// 模板版本（动态场景为 Prompt / 模型版本）。
    pub template_version: &'static str,
    /// `business_data` 序列化后的最大字节数。
    pub max_business_data_bytes: usize,
}

/// 一个已注册场景。
#[derive(Debug, Clone, Copy)]
pub struct SceneDefinition {
    /// 场景编码（请求中的 `scene_code`）。
    pub code: &'static str,
    /// 场景配置。
    pub config: SceneConfig,
    /// 渲染实现；动态策略在模型接入前可为 `None`。
    pub renderer: Option<SceneRenderer>,
}

impl SceneDefinition {
    /// 可追溯的版本串：`<strategy>:<scene_code>:<template_version>`。
    pub fn version(&self) -> String {
        format!(
            "{}:{}:{}",
            self.config.strategy.as_str(),
            self.code,
            self.config.template_version
        )
    }

    /// 当前是否具备可用的渲染实现。
    pub const fn is_renderable(&self) -> bool {
        self.renderer.is_some()
    }
}

/// 静态场景注册表。
#[derive(Debug)]
pub struct SceneRegistry {
    scenes: &'static [SceneDefinition],
}

impl SceneRegistry {
    /// 基于静态切片构造注册表。
    pub const fn new(scenes: &'static [SceneDefinition]) -> Self {
        Self { scenes }
    }

    /// 按 `scene_code` 精确查找（大小写敏感，避免歧义）。
    pub fn get(&self, scene_code: &str) -> Option<&'static SceneDefinition> {
        self.scenes.iter().find(|scene| scene.code == scene_code)
    }

    /// 全部已注册场景。
    pub const fn all(&self) -> &'static [SceneDefinition] {
        self.scenes
    }

    /// 已注册场景编码，用于未知场景错误提示。
    pub fn codes(&self) -> Vec<&'static str> {
        self.scenes.iter().map(|scene| scene.code).collect()
    }
}

/// 首批落地的场景清单（M1 第一阶段）。
pub static SCENES: &[SceneDefinition] = &[
    SceneDefinition {
        code: "ORDER_DETAIL_PAGE",
        config: SceneConfig {
            display_name: "订单详情页",
            strategy: RenderStrategy::Template,
            template_version: "v1",
            max_business_data_bytes: 64 * 1024,
        },
        // 订单详情由 REQ-202610-1172 的静态模板引擎渲染（同一模板供 /ui/render 使用）。
        renderer: Some(crate::ui_template::render_order_detail_scene),
    },
    SceneDefinition {
        code: "ORDER_LIST_PAGE",
        config: SceneConfig {
            display_name: "订单列表页",
            strategy: RenderStrategy::Template,
            template_version: "v1",
            max_business_data_bytes: 64 * 1024,
        },
        renderer: Some(crate::ui::template::render_order_list),
    },
    SceneDefinition {
        code: "PRODUCT_DETAIL_PAGE",
        config: SceneConfig {
            display_name: "商品详情页",
            strategy: RenderStrategy::Template,
            template_version: "v1",
            max_business_data_bytes: 64 * 1024,
        },
        renderer: Some(crate::ui::template::render_product_detail),
    },
    SceneDefinition {
        code: "FLASH_SALE_PAGE",
        config: SceneConfig {
            display_name: "限时抢购页",
            strategy: RenderStrategy::Template,
            template_version: "v1",
            max_business_data_bytes: 64 * 1024,
        },
        renderer: Some(crate::ui_template::render_flash_sale_scene),
    },
];

/// 全局注册表实例。
pub static REGISTRY: SceneRegistry = SceneRegistry::new(SCENES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_contains_first_batch_scenes() {
        assert!(REGISTRY.get("ORDER_DETAIL_PAGE").is_some());
        assert!(REGISTRY.get("ORDER_LIST_PAGE").is_some());
        assert!(REGISTRY.get("PRODUCT_DETAIL_PAGE").is_some());
        assert!(REGISTRY.get("FLASH_SALE_PAGE").is_some());
        assert_eq!(REGISTRY.all().len(), 4);
    }

    #[test]
    fn lookup_is_exact_and_unknown_returns_none() {
        assert!(REGISTRY.get("ORDER_DETAIL_PAGE").is_some());
        assert!(REGISTRY.get("order_detail_page").is_none());
        assert!(REGISTRY.get("ORDER_DETAIL_PAGE ").is_none());
        assert!(REGISTRY.get("NOT_A_SCENE").is_none());
    }

    #[test]
    fn every_scene_is_template_with_version_and_renderer() {
        for scene in REGISTRY.all() {
            assert_eq!(scene.config.strategy, RenderStrategy::Template);
            assert!(scene.is_renderable());
            assert_eq!(scene.version(), format!("template:{}:v1", scene.code));
        }
    }

    #[test]
    fn scene_codes_are_unique() {
        let mut codes = REGISTRY.codes();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total);
    }
}
