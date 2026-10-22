//! 单文件 HTML 的轻量 DOM 树与序列化。
//!
//! 复用校验器的容错 tokenizer，只构建混淆所需的树：元素、属性、文本、
//! 注释与 `script` / `style` 原始文本。解析失败时返回错误，不猜测结构。

use std::collections::HashSet;

use crate::html_validator::sink::IssueSink;
use crate::html_validator::source::SourceMap;
use crate::html_validator::tokenizer::{
    is_raw_text_element, is_void_element, tokenize, Attribute, Tag, Token, TokenKind,
};
use crate::html_validator::url::decode_html_entities;

use super::css;

/// DOM 属性。
#[derive(Debug, Clone)]
pub(crate) struct DomAttr {
    /// 原始属性名（保留 SVG 的 `viewBox` 等大小写）。
    pub name: String,
    /// 原始属性值（未做实体解码）。
    pub value: Option<String>,
}

/// DOM 元素。
#[derive(Debug, Clone)]
pub(crate) struct Element {
    /// 原始标签名。
    pub name: String,
    /// 小写标签名，用于 HTML 语义判断。
    pub lname: String,
    /// 属性列表。
    pub attrs: Vec<DomAttr>,
    /// 子节点。
    pub children: Vec<Node>,
    /// 是否自闭合。
    pub self_closing: bool,
}

/// DOM 节点。
#[derive(Debug, Clone)]
pub(crate) enum Node {
    /// DOCTYPE 原始文本。
    Doctype(String),
    /// 注释原始文本。
    Comment(String),
    /// 普通文本（保留原始实体）。
    Text(String),
    /// 元素。
    Element(Element),
    /// `script` / `style` 等原始文本内容。
    RawText(String),
}

/// 文档根。
#[derive(Debug, Clone)]
pub(crate) struct Document {
    /// 顶层节点。
    pub nodes: Vec<Node>,
}

/// 解析错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DomError {
    /// HTML 词法或结构错误。
    #[error("invalid html: {0}")]
    InvalidHtml(String),
}

impl Document {
    /// 从 HTML 文本构建 DOM。
    pub(crate) fn parse(input: &str) -> Result<Self, DomError> {
        let map = SourceMap::new(input);
        let mut sink = IssueSink::new(&map, 100);
        let tokens = tokenize(input, &mut sink);
        let issues = sink.finish();
        if let Some(issue) = issues
            .iter()
            .find(|issue| matches!(issue.severity, crate::html_validator::Severity::Error))
        {
            return Err(DomError::InvalidHtml(format!(
                "{} at {}:{}",
                issue.message, issue.position.line, issue.position.column
            )));
        }

        let spans = build_spans(input, &tokens);
        let mut document = Self { nodes: Vec::new() };
        let mut stack: Vec<Element> = Vec::new();
        let mut index = 0;

        while index < spans.len() {
            match &spans[index].kind {
                TokenKind::Doctype { .. } => {
                    append(
                        &mut document.nodes,
                        &mut stack,
                        Node::Doctype(input[spans[index].start..spans[index].end].to_string()),
                    );
                    index += 1;
                }
                TokenKind::Comment => {
                    append(
                        &mut document.nodes,
                        &mut stack,
                        Node::Comment(input[spans[index].start..spans[index].end].to_string()),
                    );
                    index += 1;
                }
                TokenKind::Text { text } => {
                    append(&mut document.nodes, &mut stack, Node::Text(text.clone()));
                    index += 1;
                }
                TokenKind::StartTag(tag) => {
                    let element = Element {
                        name: raw_tag_name(input, tag),
                        lname: tag.name.clone(),
                        attrs: tag
                            .attributes
                            .iter()
                            .map(|attribute| DomAttr {
                                name: raw_attr_name(input, attribute),
                                value: attribute.value.clone(),
                            })
                            .collect(),
                        children: Vec::new(),
                        self_closing: tag.self_closing,
                    };
                    let raw_text = is_raw_text_element(&tag.name);
                    if raw_text {
                        let mut element = element;
                        if let Some(next) = spans.get(index + 1) {
                            if let TokenKind::RawText { content, .. } = &next.kind {
                                element.children.push(Node::RawText(content.clone()));
                            }
                        }
                        append(&mut document.nodes, &mut stack, Node::Element(element));
                        index += 2;
                        if let Some(next) = spans.get(index) {
                            if matches!(&next.kind, TokenKind::EndTag { name, .. } if name == &tag.name)
                            {
                                index += 1;
                            }
                        }
                    } else if tag.self_closing || is_void_element(&tag.name) {
                        append(&mut document.nodes, &mut stack, Node::Element(element));
                        index += 1;
                    } else {
                        stack.push(element);
                        index += 1;
                    }
                }
                TokenKind::EndTag { name, .. } => {
                    let Some(element) = stack.pop() else {
                        return Err(DomError::InvalidHtml(format!(
                            "unexpected closing tag </{name}>"
                        )));
                    };
                    if element.lname != *name {
                        return Err(DomError::InvalidHtml(format!(
                            "mismatched closing tag: expected </{}>, got </{name}>",
                            element.lname
                        )));
                    }
                    append(&mut document.nodes, &mut stack, Node::Element(element));
                    index += 1;
                }
                TokenKind::RawText { content, .. } => {
                    append(
                        &mut document.nodes,
                        &mut stack,
                        Node::RawText(content.clone()),
                    );
                    index += 1;
                }
            }
        }

        if let Some(unclosed) = stack.last() {
            return Err(DomError::InvalidHtml(format!(
                "unclosed tag <{}>",
                unclosed.lname
            )));
        }
        Ok(document)
    }

    /// 序列化为 HTML。
    pub(crate) fn serialize(&self) -> String {
        let mut output = String::new();
        for node in &self.nodes {
            serialize_node(node, &mut output);
        }
        output
    }

    /// 收集所有类名（HTML `class` 属性 + `<style>` 选择器）。
    pub(crate) fn collect_classes(&self) -> HashSet<String> {
        let mut classes = HashSet::new();
        for node in &self.nodes {
            collect_node_classes(node, &mut classes);
        }
        classes
    }

    /// 遍历所有元素（可变）。
    pub(crate) fn for_each_element_mut<F>(&mut self, mut visit: F)
    where
        F: FnMut(&mut Element, usize),
    {
        for node in &mut self.nodes {
            visit_node(node, 0, &mut visit);
        }
    }

    /// 查找 `body` 元素（用于追加诱饵节点）。
    pub(crate) fn body_mut(&mut self) -> Option<&mut Element> {
        for node in &mut self.nodes {
            if let Some(body) = find_body(node) {
                return Some(body);
            }
        }
        None
    }
}

fn build_spans(input: &str, tokens: &[Token]) -> Vec<SpanToken> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, token)| SpanToken {
            kind: token.kind.clone(),
            start: token.start,
            end: tokens.get(index + 1).map_or(input.len(), |next| next.start),
        })
        .collect()
}

struct SpanToken {
    kind: TokenKind,
    start: usize,
    end: usize,
}

fn append(nodes: &mut Vec<Node>, stack: &mut [Element], node: Node) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        nodes.push(node);
    }
}

fn raw_tag_name(input: &str, tag: &Tag) -> String {
    let start = tag.start + 1;
    let bytes = input.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b':' | b'_'))
    {
        end += 1;
    }
    input[start..end].to_string()
}

fn raw_attr_name(input: &str, attribute: &Attribute) -> String {
    let start = attribute.name_start;
    let bytes = input.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && !bytes[end].is_ascii_whitespace()
        && !matches!(bytes[end], b'=' | b'>' | b'/' | b'<' | b'"' | b'\'')
    {
        end += 1;
    }
    input[start..end].to_string()
}

fn serialize_node(node: &Node, output: &mut String) {
    match node {
        Node::Doctype(raw) | Node::Comment(raw) | Node::Text(raw) | Node::RawText(raw) => {
            output.push_str(raw);
        }
        Node::Element(element) => serialize_element(element, output),
    }
}

fn serialize_element(element: &Element, output: &mut String) {
    output.push('<');
    output.push_str(&element.name);
    for attribute in &element.attrs {
        output.push(' ');
        output.push_str(&attribute.name);
        if let Some(value) = &attribute.value {
            output.push_str("=\"");
            output.push_str(&escape_attribute(value));
            output.push('"');
        }
    }
    if element.self_closing && !is_void_element(&element.lname) {
        output.push_str("/>");
        return;
    }
    output.push('>');
    if is_void_element(&element.lname) {
        return;
    }
    for child in &element.children {
        serialize_node(child, output);
    }
    output.push_str("</");
    output.push_str(&element.name);
    output.push('>');
}

fn escape_attribute(value: &str) -> String {
    let decoded = decode_html_entities(value);
    let mut output = String::with_capacity(decoded.len());
    for ch in decoded.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(ch),
        }
    }
    output
}

fn collect_node_classes(node: &Node, classes: &mut HashSet<String>) {
    let Node::Element(element) = node else {
        return;
    };
    if let Some(value) = element
        .attrs
        .iter()
        .find(|attribute| attribute.name.eq_ignore_ascii_case("class"))
        .and_then(|attribute| attribute.value.as_deref())
    {
        for class in decode_html_entities(value).split_whitespace() {
            classes.insert(class.to_string());
        }
    }
    if element.lname == "style" {
        for child in &element.children {
            if let Node::RawText(css_text) = child {
                css::collect_classes(css_text, classes);
            }
        }
    }
    for child in &element.children {
        collect_node_classes(child, classes);
    }
}

fn visit_node<F>(node: &mut Node, depth: usize, visit: &mut F)
where
    F: FnMut(&mut Element, usize),
{
    if let Node::Element(element) = node {
        visit(element, depth);
        for child in &mut element.children {
            visit_node(child, depth + 1, visit);
        }
    }
}

fn find_body(node: &mut Node) -> Option<&mut Element> {
    match node {
        Node::Element(element) => {
            if element.lname == "body" {
                return Some(element);
            }
            for child in &mut element.children {
                if let Some(body) = find_body(child) {
                    return Some(body);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_structure_and_svg_case() {
        let html = "<!DOCTYPE html><html><body><svg viewBox=\"0 0 24 24\"><path d=\"M0 0\"/></svg><p class=\"x\">你好 &amp; 世界</p></body></html>";
        let document = Document::parse(html).unwrap();
        let output = document.serialize();
        assert!(output.contains("viewBox=\"0 0 24 24\""));
        assert!(output.contains("你好 &amp; 世界"));
        assert!(output.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn collects_classes_from_attributes_and_style() {
        let html = "<html><head><style>.card > .title { color: red }</style></head><body class=\"page\"><div class=\"card\"></div></body></html>";
        let document = Document::parse(html).unwrap();
        let classes = document.collect_classes();
        assert!(classes.contains("page"));
        assert!(classes.contains("card"));
        assert!(classes.contains("title"));
    }

    #[test]
    fn rejects_mismatched_tags() {
        assert!(Document::parse("<div><span></div>").is_err());
    }
}
