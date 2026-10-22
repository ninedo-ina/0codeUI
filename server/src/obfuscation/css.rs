//! CSS 类选择器扫描与改写。
//!
//! 只处理选择器中的 `.class`，跳过注释、字符串、`url(...)` 与数字小数，
//! 避免误改 `content: ".foo"`、`background: url(icon.png)` 等内容。

use std::collections::{HashMap, HashSet};

/// 收集 CSS 中出现的类选择器。
pub(crate) fn collect_classes(css: &str, output: &mut HashSet<String>) {
    let bytes = css.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            index = skip_comment(bytes, index + 2);
        } else if matches!(bytes[index], b'\'' | b'"') {
            index = skip_string(bytes, index);
        } else if starts_url(bytes, index) {
            index = skip_url(bytes, index);
        } else if bytes[index] == b'.' && is_ident_start(bytes.get(index + 1).copied()) {
            let (name, next) = parse_ident(css, index + 1);
            if !name.is_empty() {
                output.insert(name.to_string());
            }
            index = next;
        } else {
            index = advance(css, index);
        }
    }
}

/// 改写 CSS 中的类选择器；未命中映射表的类名保持原样。
pub(crate) fn rewrite_classes(css: &str, mapping: &HashMap<String, String>) -> String {
    let bytes = css.as_bytes();
    let mut output = String::with_capacity(css.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            let next = skip_comment(bytes, index + 2);
            output.push_str(&css[index..next]);
            index = next;
        } else if matches!(bytes[index], b'\'' | b'"') {
            let next = skip_string(bytes, index);
            output.push_str(&css[index..next]);
            index = next;
        } else if starts_url(bytes, index) {
            let next = skip_url(bytes, index);
            output.push_str(&css[index..next]);
            index = next;
        } else if bytes[index] == b'.' && is_ident_start(bytes.get(index + 1).copied()) {
            let (name, next) = parse_ident(css, index + 1);
            if let Some(mapped) = mapping.get(name) {
                output.push('.');
                output.push_str(mapped);
            } else {
                output.push_str(&css[index..next]);
            }
            index = next;
        } else {
            let next = advance(css, index);
            output.push_str(&css[index..next]);
            index = next;
        }
    }
    output
}

/// 判断 CSS 是否使用了对 DOM 层级敏感的选择器。
///
/// 此类选择器存在时不做包装节点 / 诱饵节点，避免破坏 `>`、`+`、`~`、
/// `:first-child` 等语义。类名与属性顺序混淆不受影响。
pub(crate) fn has_layout_sensitive_selectors(css: &str) -> bool {
    let lower = css.to_ascii_lowercase();
    if lower.contains(":nth-")
        || lower.contains(":first-child")
        || lower.contains(":last-child")
        || lower.contains(":only-child")
        || lower.contains(":empty")
        || lower.contains(":has(")
    {
        return true;
    }

    let bytes = css.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"/*") {
            index = skip_comment(bytes, index + 2);
        } else if matches!(bytes[index], b'\'' | b'"') {
            index = skip_string(bytes, index);
        } else if starts_url(bytes, index) {
            index = skip_url(bytes, index);
        } else if matches!(bytes[index], b'>' | b'+' | b'~') {
            return true;
        } else {
            index = advance(css, index);
        }
    }
    false
}

fn starts_url(bytes: &[u8], index: usize) -> bool {
    bytes.len() >= index + 4
        && bytes[index..index + 3].eq_ignore_ascii_case(b"url")
        && bytes[index + 3] == b'('
}

fn skip_url(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 4;
    let mut depth = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => index = skip_string(bytes, index),
            b'(' => {
                depth += 1;
                index += 1;
            }
            b')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return index;
                }
            }
            _ => index += 1,
        }
    }
    bytes.len()
}

fn skip_comment(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while index + 1 < bytes.len() {
        if bytes[index] == b'*' && bytes[index + 1] == b'/' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == quote {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn parse_ident(input: &str, start: usize) -> (&str, usize) {
    let bytes = input.as_bytes();
    let mut end = start;
    while end < bytes.len() && is_ident_continue(bytes[end]) {
        end += 1;
    }
    (&input[start..end], end)
}

fn is_ident_start(byte: Option<u8>) -> bool {
    byte.is_some_and(|byte| {
        byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-') || byte >= 0x80
    })
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') || byte >= 0x80
}

fn advance(input: &str, index: usize) -> usize {
    index + input[index..].chars().next().map_or(1, char::len_utf8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_and_rewrites_selectors_only() {
        let css = r#"
            /* .ignored { color: red } */
            .card, .card:hover > .title { content: ".not-a-selector"; }
            .icon { background: url(./icon.png); width: .5em; }
        "#;
        let mut classes = HashSet::new();
        collect_classes(css, &mut classes);
        assert!(classes.contains("card"));
        assert!(classes.contains("title"));
        assert!(classes.contains("icon"));
        assert!(!classes.contains("ignored"));
        assert!(!classes.contains("not-a-selector"));

        let mapping = HashMap::from([
            ("card".to_string(), "c1".to_string()),
            ("title".to_string(), "c2".to_string()),
        ]);
        let rewritten = rewrite_classes(css, &mapping);
        assert!(rewritten.contains(".c1:hover > .c2"));
        assert!(rewritten.contains(".not-a-selector"));
        assert!(rewritten.contains("url(./icon.png)"));
        assert!(rewritten.contains("width: .5em"));
    }

    #[test]
    fn detects_layout_sensitive_selectors() {
        assert!(has_layout_sensitive_selectors(".a > .b { color: red }"));
        assert!(has_layout_sensitive_selectors(".a + .b { color: red }"));
        assert!(has_layout_sensitive_selectors(
            ".a:nth-child(2) { color: red }"
        ));
        assert!(!has_layout_sensitive_selectors(".a .b { color: red }"));
    }
}
