//! 混淆编排：解析 DOM → 会话类名映射 → CSS/JS 改写 → DOM 随机化 → 复检。

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::html_validator::{validate_single_file_html_with_config, ValidatorConfig};

use super::config::ObfuscationConfig;
use super::css;
use super::dom::{Document, DomAttr, Element, Node};
use super::js;
use super::rng::Rng;
use super::session::{ephemeral_mapping, SessionStore};

/// 混淆错误。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ObfuscationError {
    /// 会话标识非法。
    #[error("invalid session_id: {0}")]
    InvalidSessionId(String),
    /// 输入 HTML 无法解析。
    #[error("invalid html: {0}")]
    InvalidHtml(String),
    /// 混淆后的 HTML 未通过校验。
    #[error("obfuscated html failed validation: {0}")]
    InvalidOutput(String),
    /// 覆盖率未达到配置下限。
    #[error("identifier coverage {actual:.3} is below target {target:.3}")]
    CoverageBelowTarget { actual: f64, target: f64 },
    /// 内部错误。
    #[error("internal obfuscation error: {0}")]
    Internal(String),
}

/// 混淆统计。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObfuscationStats {
    /// 实际使用的强度。
    pub level: String,
    /// 会话标识（无会话请求为 `null`）。
    pub session_id: Option<String>,
    /// 发现的类名总数。
    pub classes_total: usize,
    /// 已改写的类名数。
    pub classes_renamed: usize,
    /// 声明的非保留标识符总数。
    pub identifiers_total: usize,
    /// 已改写的非保留标识符数。
    pub identifiers_renamed: usize,
    /// 非保留标识符覆盖率（0.0–1.0）。
    pub identifier_coverage: f64,
    /// 新增包装节点数。
    pub wrappers_added: usize,
    /// 新增诱饵节点数。
    pub decoys_added: usize,
    /// 打乱属性顺序的元素数。
    pub attributes_shuffled: usize,
    /// 输入字节数。
    pub bytes_before: usize,
    /// 输出字节数。
    pub bytes_after: usize,
    /// 耗时（毫秒，保留三位小数）。
    pub duration_ms: f64,
}

/// 混淆结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObfuscationResult {
    /// 混淆后的完整 HTML。
    pub html: String,
    /// 统计信息。
    pub stats: ObfuscationStats,
}

/// 进程内混淆引擎（持有会话类名映射表）。
pub struct ObfuscationEngine {
    sessions: SessionStore,
}

impl ObfuscationEngine {
    /// 创建引擎。
    pub fn new(session_ttl_secs: u64, max_sessions: usize) -> Self {
        Self {
            sessions: SessionStore::new(session_ttl_secs, max_sessions),
        }
    }

    /// 混淆一段完整 HTML。
    pub fn obfuscate(
        &self,
        html: &str,
        config: &ObfuscationConfig,
        session_id: Option<&str>,
    ) -> Result<ObfuscationResult, ObfuscationError> {
        config.validate().map_err(ObfuscationError::Internal)?;
        if let Some(session_id) = session_id {
            validate_session_id(session_id)?;
        }

        let started = Instant::now();
        let bytes_before = html.len();
        if !config.level.enabled() {
            return Ok(ObfuscationResult {
                html: html.to_string(),
                stats: ObfuscationStats {
                    level: config.level.to_string(),
                    session_id: session_id.map(str::to_string),
                    bytes_before,
                    bytes_after: html.len(),
                    duration_ms: round_millis(started.elapsed().as_secs_f64() * 1000.0),
                    ..ObfuscationStats::default()
                },
            });
        }

        let mut document = Document::parse(html)
            .map_err(|error| ObfuscationError::InvalidHtml(error.to_string()))?;
        let classes = document.collect_classes();
        let mut rng = Rng::new();
        let class_mapping = match session_id {
            Some(session_id) => self.sessions.mapping_for(session_id, &classes, &mut rng),
            None => ephemeral_mapping(&classes, &mut rng),
        };

        let style_text = collect_style_text(&document);
        let layout_sensitive = css::has_layout_sensitive_selectors(&style_text);
        let js_renaming = config.level.js_renaming();
        let mut identifiers_total = 0usize;
        let mut identifiers_renamed = 0usize;

        document.for_each_element_mut(|element, _depth| {
            rewrite_element(
                element,
                &class_mapping,
                &mut rng,
                js_renaming,
                &mut identifiers_total,
                &mut identifiers_renamed,
            );
        });

        let mut wrappers_added = 0usize;
        if config.level.wrappers() && !layout_sensitive {
            wrap_children(
                &mut document,
                config.level.max_wrapper_depth(),
                &mut rng,
                &mut wrappers_added,
            );
        }

        let mut decoys_added = 0usize;
        if config.level.decoys() && !layout_sensitive {
            add_decoys(&mut document, &mut rng, &mut decoys_added);
        }

        let mut attributes_shuffled = 0usize;
        document.for_each_element_mut(|element, _depth| {
            if element.attrs.len() > 1 {
                rng.shuffle(&mut element.attrs);
                attributes_shuffled += 1;
            }
        });

        let output = document.serialize();
        let validation =
            validate_single_file_html_with_config(&output, &ValidatorConfig::default());
        if !validation.valid {
            let detail = validation
                .issues
                .first()
                .map(|issue| format!("{}: {}", issue.kind as u8, issue.message))
                .unwrap_or_else(|| "unknown validation error".to_string());
            return Err(ObfuscationError::InvalidOutput(detail));
        }

        let coverage = if identifiers_total == 0 {
            1.0
        } else {
            identifiers_renamed as f64 / identifiers_total as f64
        };
        if coverage + f64::EPSILON < config.min_identifier_coverage {
            return Err(ObfuscationError::CoverageBelowTarget {
                actual: coverage,
                target: config.min_identifier_coverage,
            });
        }

        Ok(ObfuscationResult {
            stats: ObfuscationStats {
                level: config.level.to_string(),
                session_id: session_id.map(str::to_string),
                classes_total: classes.len(),
                classes_renamed: class_mapping.len(),
                identifiers_total,
                identifiers_renamed,
                identifier_coverage: coverage,
                wrappers_added,
                decoys_added,
                attributes_shuffled,
                bytes_before,
                bytes_after: output.len(),
                duration_ms: round_millis(started.elapsed().as_secs_f64() * 1000.0),
            },
            html: output,
        })
    }
}

/// 使用进程级默认引擎做一次混淆（无会话，类名映射一次性）。
pub fn obfuscate_html(
    html: &str,
    config: &ObfuscationConfig,
    session_id: Option<&str>,
) -> Result<ObfuscationResult, ObfuscationError> {
    static ENGINE: OnceLock<ObfuscationEngine> = OnceLock::new();
    ENGINE
        .get_or_init(|| ObfuscationEngine::new(1800, 1000))
        .obfuscate(html, config, session_id)
}

fn validate_session_id(session_id: &str) -> Result<(), ObfuscationError> {
    let valid_len = (1..=128).contains(&session_id.len());
    let valid_chars = session_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'));
    if !valid_len || !valid_chars {
        return Err(ObfuscationError::InvalidSessionId(
            "must be 1..=128 characters of [A-Za-z0-9._:-]".to_string(),
        ));
    }
    Ok(())
}

fn rewrite_element(
    element: &mut Element,
    class_mapping: &HashMap<String, String>,
    rng: &mut Rng,
    js_renaming: bool,
    identifiers_total: &mut usize,
    identifiers_renamed: &mut usize,
) {
    for attribute in &mut element.attrs {
        let lower_name = attribute.name.to_ascii_lowercase();
        if lower_name == "class" {
            if let Some(value) = &attribute.value {
                let mapped = map_class_list(value, class_mapping);
                attribute.value = Some(mapped);
            }
        } else if lower_name == "style" {
            if let Some(value) = &attribute.value {
                let decoded = crate::html_validator::url::decode_html_entities(value);
                attribute.value = Some(css::rewrite_classes(&decoded, class_mapping));
            }
        } else if lower_name.starts_with("on") && lower_name.len() > 2 {
            if let Some(value) = &attribute.value {
                let decoded = crate::html_validator::url::decode_html_entities(value);
                let result = js::rewrite_script(&decoded, class_mapping, rng, js_renaming);
                *identifiers_total += result.declared_identifiers;
                *identifiers_renamed += result.renamed_identifiers;
                attribute.value = Some(result.code);
            }
        }
    }

    if element.lname == "style" {
        for child in &mut element.children {
            if let Node::RawText(text) = child {
                *text = css::rewrite_classes(text, class_mapping);
            }
        }
    } else if element.lname == "script" {
        for child in &mut element.children {
            if let Node::RawText(text) = child {
                let result = js::rewrite_script(text, class_mapping, rng, js_renaming);
                *identifiers_total += result.declared_identifiers;
                *identifiers_renamed += result.renamed_identifiers;
                *text = result.code;
            }
        }
    }
}

fn map_class_list(value: &str, mapping: &HashMap<String, String>) -> String {
    let decoded = crate::html_validator::url::decode_html_entities(value);
    decoded
        .split_whitespace()
        .map(|class| mapping.get(class).map_or(class, String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

fn collect_style_text(document: &Document) -> String {
    let mut output = String::new();
    for node in &document.nodes {
        collect_style_node(node, &mut output);
    }
    output
}

fn collect_style_node(node: &Node, output: &mut String) {
    if let Node::Element(element) = node {
        if element.lname == "style" {
            for child in &element.children {
                if let Node::RawText(text) = child {
                    output.push_str(text);
                    output.push('\n');
                }
            }
        }
        for child in &element.children {
            collect_style_node(child, output);
        }
    }
}

fn wrap_children(document: &mut Document, max_depth: usize, rng: &mut Rng, added: &mut usize) {
    for node in &mut document.nodes {
        wrap_node(node, 0, max_depth, rng, added);
    }
}

fn wrap_node(node: &mut Node, depth: usize, max_depth: usize, rng: &mut Rng, added: &mut usize) {
    let Node::Element(element) = node else {
        return;
    };
    for child in &mut element.children {
        wrap_node(child, depth + 1, max_depth, rng, added);
    }

    let element_children = element
        .children
        .iter()
        .filter(|child| matches!(child, Node::Element(_)))
        .count();
    if depth > max_depth || element_children < 2 || !wrapper_eligible(&element.lname) {
        return;
    }

    let mut attrs = vec![
        DomAttr {
            name: "style".to_string(),
            value: Some("display:contents".to_string()),
        },
        DomAttr {
            name: format!("data-o{}", rng.hex(6)),
            value: Some(rng.hex(8)),
        },
    ];
    rng.shuffle(&mut attrs);
    let wrapper = Element {
        name: "div".to_string(),
        lname: "div".to_string(),
        attrs,
        children: std::mem::take(&mut element.children),
        self_closing: false,
    };
    element.children.push(Node::Element(wrapper));
    *added += 1;
}

fn wrapper_eligible(name: &str) -> bool {
    matches!(
        name,
        "body"
            | "main"
            | "section"
            | "header"
            | "footer"
            | "div"
            | "article"
            | "aside"
            | "nav"
            | "form"
            | "li"
            | "td"
            | "th"
    )
}

fn add_decoys(document: &mut Document, rng: &mut Rng, added: &mut usize) {
    let Some(body) = document.body_mut() else {
        return;
    };
    for _ in 0..2 {
        let attrs = vec![
            DomAttr {
                name: "style".to_string(),
                value: Some("display:none".to_string()),
            },
            DomAttr {
                name: "aria-hidden".to_string(),
                value: Some("true".to_string()),
            },
            DomAttr {
                name: format!("data-d{}", rng.hex(6)),
                value: Some(rng.hex(8)),
            },
        ];
        body.children.push(Node::Element(Element {
            name: "div".to_string(),
            lname: "div".to_string(),
            attrs,
            children: Vec::new(),
            self_closing: false,
        }));
        *added += 1;
    }
}

fn round_millis(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obfuscation::ObfuscationLevel;

    const SAMPLE: &str = r#"<!DOCTYPE html><html><head><style>.card { color: red } .title { color: blue }</style></head><body><div class="card"><span class="title">Hello</span><button class="btn" onclick="var response=1;var el=document.getElementById('x');el.textContent=String(response);">Go</button></div></body></html>"#;

    #[test]
    fn obfuscates_classes_and_js_and_remains_valid() {
        let engine = ObfuscationEngine::new(60, 10);
        let result = engine
            .obfuscate(SAMPLE, &ObfuscationConfig::default(), Some("s1"))
            .unwrap();
        assert!(result.stats.classes_total >= 3);
        assert_eq!(result.stats.classes_renamed, result.stats.classes_total);
        assert!(result.stats.identifiers_total >= 2);
        assert!(result.stats.identifier_coverage >= 0.8);
        assert!(!result.html.contains("class=\"card\""));
        assert!(!result.html.contains("var response"));
        assert!(result.html.contains("document.getElementById"));
        assert!(
            validate_single_file_html_with_config(&result.html, &ValidatorConfig::default()).valid
        );
    }

    #[test]
    fn structural_and_text_fingerprint_matches_original() {
        let original = Document::parse(SAMPLE).unwrap();
        let engine = ObfuscationEngine::new(60, 10);
        let result = engine
            .obfuscate(SAMPLE, &ObfuscationConfig::default(), Some("s1"))
            .unwrap();
        let obfuscated = Document::parse(&result.html).unwrap();

        assert_eq!(text_content(&original), text_content(&obfuscated));
        assert_eq!(element_tags(&original), element_tags(&obfuscated));
    }

    fn text_content(document: &Document) -> String {
        let mut output = String::new();
        for node in &document.nodes {
            collect_text(node, &mut output);
        }
        output
    }

    fn collect_text(node: &Node, output: &mut String) {
        match node {
            Node::Text(text) => output.push_str(text),
            Node::Element(element)
                if !matches!(element.lname.as_str(), "script" | "style" | "title") =>
            {
                for child in &element.children {
                    collect_text(child, output);
                }
            }
            _ => {}
        }
    }

    fn element_tags(document: &Document) -> Vec<String> {
        let mut output = Vec::new();
        for node in &document.nodes {
            collect_tags(node, &mut output);
        }
        output
    }

    fn collect_tags(node: &Node, output: &mut Vec<String>) {
        let Node::Element(element) = node else {
            return;
        };
        let style = element
            .attrs
            .iter()
            .find(|attribute| attribute.name.eq_ignore_ascii_case("style"))
            .and_then(|attribute| attribute.value.as_deref())
            .unwrap_or_default();
        let is_decoy = style.eq_ignore_ascii_case("display:none");
        let is_wrapper = style.eq_ignore_ascii_case("display:contents");
        if !is_decoy && !is_wrapper {
            output.push(element.lname.clone());
        }
        if !is_decoy {
            for child in &element.children {
                collect_tags(child, output);
            }
        }
    }

    #[test]
    fn same_session_keeps_class_mapping_but_output_differs() {
        let engine = ObfuscationEngine::new(60, 10);
        let config = ObfuscationConfig {
            level: ObfuscationLevel::Aggressive,
            ..ObfuscationConfig::default()
        };
        let first = engine.obfuscate(SAMPLE, &config, Some("s1")).unwrap();
        let second = engine.obfuscate(SAMPLE, &config, Some("s1")).unwrap();
        assert_ne!(first.html, second.html);
        assert!(first.stats.wrappers_added > 0);
    }

    #[test]
    fn basic_level_keeps_js_identifiers() {
        let engine = ObfuscationEngine::new(60, 10);
        let config = ObfuscationConfig {
            level: ObfuscationLevel::Basic,
            ..ObfuscationConfig::default()
        };
        let result = engine.obfuscate(SAMPLE, &config, None).unwrap();
        assert_eq!(result.stats.identifiers_total, 0);
        assert!(result.html.contains("var response"));
        assert!(!result.html.contains("class=\"card\""));
    }

    #[test]
    fn off_returns_original() {
        let engine = ObfuscationEngine::new(60, 10);
        let config = ObfuscationConfig {
            level: ObfuscationLevel::Off,
            ..ObfuscationConfig::default()
        };
        let result = engine.obfuscate(SAMPLE, &config, None).unwrap();
        assert_eq!(result.html, SAMPLE);
    }

    #[test]
    fn invalid_session_id_is_rejected() {
        let engine = ObfuscationEngine::new(60, 10);
        let error = engine
            .obfuscate(SAMPLE, &ObfuscationConfig::default(), Some("bad id!"))
            .unwrap_err();
        assert!(matches!(error, ObfuscationError::InvalidSessionId(_)));
    }
}
