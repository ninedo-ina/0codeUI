//! 内联 JavaScript 标识符随机化。
//!
//! 不引入 JS 解析器依赖：使用轻量词法分析识别字符串、注释、正则与标识符，
//! 只重命名「声明过的局部标识符」（`var` / `let` / `const`、函数名、参数、
//! 箭头函数参数、catch 参数），并跳过属性访问、对象键与保留全局对象。
//!
//! 这是保守策略：未声明的全局变量一律不改，宁可少改也不破坏运行时语义。

use std::collections::{HashMap, HashSet};

use super::rng::Rng;

/// JS token 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsKind {
    Ident,
    Keyword,
    Number,
    String,
    Template,
    Punct,
    Comment,
    Regex,
    Whitespace,
}

/// JS token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JsToken {
    pub kind: JsKind,
    pub text: String,
}

/// 一次 JS 混淆的统计。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct JsRenameResult {
    pub code: String,
    pub declared_identifiers: usize,
    pub renamed_identifiers: usize,
    pub identifier_coverage: f64,
    pub renamed_occurrences: usize,
}

/// 改写脚本：按需重命名局部标识符，并始终同步改写字符串中的类选择器。
///
/// `rename_identifiers` 为 `false` 时只做类选择器同步（`basic` 强度），
/// 不做任何标识符替换。
pub(crate) fn rewrite_script(
    source: &str,
    class_mapping: &HashMap<String, String>,
    rng: &mut Rng,
    rename_identifiers: bool,
) -> JsRenameResult {
    let tokens = lex(source);
    let declared = if rename_identifiers {
        collect_declared(&tokens)
    } else {
        HashSet::new()
    };
    let mapping = build_identifier_mapping(&declared, rng);
    let (code, renamed_occurrences) = rename_tokens(&tokens, &mapping, class_mapping);

    let declared_identifiers = declared.len();
    let renamed_identifiers = mapping.len();
    JsRenameResult {
        code,
        declared_identifiers,
        renamed_identifiers,
        identifier_coverage: if declared_identifiers == 0 {
            1.0
        } else {
            renamed_identifiers as f64 / declared_identifiers as f64
        },
        renamed_occurrences,
    }
}

/// 仅做词法分析（测试与调试用）。
#[cfg(test)]
pub(crate) fn tokenize_for_test(source: &str) -> Vec<JsToken> {
    lex(source)
}

fn lex(source: &str) -> Vec<JsToken> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            push(&mut tokens, JsKind::Whitespace, &source[start..index]);
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            push(&mut tokens, JsKind::Comment, &source[start..index]);
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() {
                if bytes[index] == b'*' && bytes[index + 1] == b'/' {
                    index += 2;
                    break;
                }
                index += 1;
            }
            push(&mut tokens, JsKind::Comment, &source[start..index]);
        } else if matches!(byte, b'\'' | b'"') {
            index = scan_string(bytes, index);
            push(&mut tokens, JsKind::String, &source[start..index]);
        } else if byte == b'`' {
            index = scan_template(bytes, index);
            push(&mut tokens, JsKind::Template, &source[start..index]);
        } else if byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
        {
            index = scan_number(bytes, index);
            push(&mut tokens, JsKind::Number, &source[start..index]);
        } else if is_ident_start(byte) {
            index += 1;
            while index < bytes.len() && is_ident_continue(bytes[index]) {
                index += 1;
            }
            let text = &source[start..index];
            let kind = if is_keyword(text) {
                JsKind::Keyword
            } else {
                JsKind::Ident
            };
            push(&mut tokens, kind, text);
        } else if byte == b'/' && can_start_regex(&tokens) {
            if let Some(next) = scan_regex(bytes, index) {
                index = next;
                push(&mut tokens, JsKind::Regex, &source[start..index]);
            } else {
                index += 1;
                push(&mut tokens, JsKind::Punct, &source[start..index]);
            }
        } else {
            index += punct_len(bytes, index);
            push(&mut tokens, JsKind::Punct, &source[start..index]);
        }
    }
    tokens
}

fn push(tokens: &mut Vec<JsToken>, kind: JsKind, text: &str) {
    tokens.push(JsToken {
        kind,
        text: text.to_string(),
    });
}

fn scan_string(bytes: &[u8], start: usize) -> usize {
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

fn scan_template(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            index = (index + 2).min(bytes.len());
        } else if bytes[index] == b'`' {
            return index + 1;
        } else {
            index += 1;
        }
    }
    bytes.len()
}

fn scan_number(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    if bytes[index] == b'0' && matches!(bytes.get(index + 1), Some(b'x' | b'X')) {
        index += 2;
        while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
            index += 1;
        }
        return index;
    }
    while index < bytes.len()
        && (bytes[index].is_ascii_digit()
            || bytes[index] == b'.'
            || matches!(bytes[index], b'e' | b'E' | b'+' | b'-'))
    {
        // 指数符号只在 e/E 后有效；这里保守接受，避免把 1e-3 拆开。
        if matches!(bytes[index], b'+' | b'-')
            && !matches!(bytes.get(index.wrapping_sub(1)), Some(b'e' | b'E'))
        {
            break;
        }
        index += 1;
    }
    index
}

fn scan_regex(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    let mut in_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index = (index + 2).min(bytes.len()),
            b'[' => {
                in_class = true;
                index += 1;
            }
            b']' => {
                in_class = false;
                index += 1;
            }
            b'/' if !in_class => {
                index += 1;
                while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                    index += 1;
                }
                return Some(index);
            }
            b'\n' | b'\r' => return None,
            _ => index += 1,
        }
    }
    None
}

fn punct_len(bytes: &[u8], start: usize) -> usize {
    const MULTI: &[&[u8]] = &[
        b">>>=", b"===", b"!==", b"**=", b">>>", b"<<=", b">>=", b"=>", b"==", b"!=", b"<=", b">=",
        b"&&", b"||", b"??", b"?.", b"++", b"--", b"**", b"+=", b"-=", b"*=", b"/=", b"%=", b"&=",
        b"|=", b"^=", b"<<", b">>",
    ];
    MULTI
        .iter()
        .find(|candidate| bytes[start..].starts_with(candidate))
        .map_or(1, |candidate| candidate.len())
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
}

fn is_ident_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "break"
            | "case"
            | "catch"
            | "class"
            | "const"
            | "continue"
            | "debugger"
            | "default"
            | "delete"
            | "do"
            | "else"
            | "export"
            | "extends"
            | "finally"
            | "for"
            | "function"
            | "if"
            | "import"
            | "in"
            | "instanceof"
            | "let"
            | "new"
            | "return"
            | "super"
            | "switch"
            | "this"
            | "throw"
            | "try"
            | "typeof"
            | "var"
            | "void"
            | "while"
            | "with"
            | "yield"
            | "async"
            | "await"
            | "of"
            | "static"
            | "get"
            | "set"
            | "true"
            | "false"
            | "null"
            | "undefined"
    )
}

fn is_reserved_global(text: &str) -> bool {
    matches!(
        text,
        "window"
            | "document"
            | "navigator"
            | "localStorage"
            | "sessionStorage"
            | "fetch"
            | "console"
            | "JSON"
            | "Math"
            | "Date"
            | "Promise"
            | "Object"
            | "Array"
            | "String"
            | "Number"
            | "Boolean"
            | "Error"
            | "RegExp"
            | "Set"
            | "Map"
            | "WeakMap"
            | "Intl"
            | "IntimeUI"
            | "setTimeout"
            | "clearTimeout"
            | "setInterval"
            | "clearInterval"
            | "requestAnimationFrame"
            | "cancelAnimationFrame"
            | "alert"
            | "confirm"
            | "prompt"
            | "URL"
            | "Blob"
            | "FormData"
            | "Headers"
            | "Request"
            | "Response"
            | "location"
            | "history"
            | "screen"
            | "performance"
            | "crypto"
            | "atob"
            | "btoa"
            | "NaN"
            | "Infinity"
    )
}

fn can_start_regex(tokens: &[JsToken]) -> bool {
    let Some(previous) = tokens
        .iter()
        .rev()
        .find(|token| !matches!(token.kind, JsKind::Whitespace | JsKind::Comment))
    else {
        return true;
    };
    match previous.kind {
        JsKind::Keyword => matches!(
            previous.text.as_str(),
            "return"
                | "throw"
                | "case"
                | "delete"
                | "void"
                | "typeof"
                | "instanceof"
                | "in"
                | "of"
                | "new"
                | "do"
                | "else"
                | "yield"
                | "await"
        ),
        JsKind::Punct => matches!(
            previous.text.as_str(),
            "(" | "["
                | "{"
                | ","
                | ";"
                | ":"
                | "="
                | "!"
                | "?"
                | "&"
                | "|"
                | "+"
                | "-"
                | "*"
                | "%"
                | "^"
                | "~"
                | "<"
                | ">"
                | "=>"
        ),
        _ => false,
    }
}

fn collect_declared(tokens: &[JsToken]) -> HashSet<String> {
    let mut declared = HashSet::new();
    for index in 0..tokens.len() {
        let token = &tokens[index];
        if token.kind == JsKind::Keyword && matches!(token.text.as_str(), "var" | "let" | "const") {
            collect_var_names(tokens, index + 1, &mut declared);
        } else if token.kind == JsKind::Keyword && token.text == "function" {
            collect_function_names(tokens, index + 1, &mut declared);
        } else if token.kind == JsKind::Keyword && token.text == "catch" {
            collect_catch_name(tokens, index + 1, &mut declared);
        } else if token.kind == JsKind::Ident {
            if let Some(next) = next_significant(tokens, index) {
                if tokens[next].text == "=>" {
                    declared.insert(token.text.clone());
                }
            }
        }
    }

    // 括号箭头函数：`(a, b) => ...`。
    for index in 0..tokens.len() {
        if tokens[index].kind == JsKind::Punct
            && tokens[index].text == ")"
            && next_significant(tokens, index).is_some_and(|next| tokens[next].text == "=>")
        {
            if let Some(open) = matching_open_paren(tokens, index) {
                collect_params(tokens, open, &mut declared);
            }
        }
    }

    declared.retain(|name| !is_reserved_global(name) && !is_keyword(name));
    declared
}

fn collect_var_names(tokens: &[JsToken], start: usize, declared: &mut HashSet<String>) {
    let mut depth = 0i32;
    let mut expect_name = true;
    let mut index = start;
    while index < tokens.len() {
        let token = &tokens[index];
        if matches!(token.kind, JsKind::Whitespace | JsKind::Comment) {
            index += 1;
            continue;
        }
        match token.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ";" if depth == 0 => break,
            "in" | "of" if depth == 0 => break,
            "," if depth == 0 => expect_name = true,
            "=" if depth == 0 => expect_name = false,
            _ => {
                if depth == 0 && expect_name && token.kind == JsKind::Ident {
                    declared.insert(token.text.clone());
                    expect_name = false;
                }
            }
        }
        index += 1;
    }
}

fn collect_function_names(tokens: &[JsToken], start: usize, declared: &mut HashSet<String>) {
    let mut index = start;
    while index < tokens.len() && matches!(tokens[index].kind, JsKind::Whitespace | JsKind::Comment)
    {
        index += 1;
    }
    if index < tokens.len() && tokens[index].kind == JsKind::Ident {
        declared.insert(tokens[index].text.clone());
        index += 1;
    }
    while index < tokens.len() && matches!(tokens[index].kind, JsKind::Whitespace | JsKind::Comment)
    {
        index += 1;
    }
    if index < tokens.len() && tokens[index].text == "(" {
        collect_params(tokens, index, declared);
    }
}

fn collect_catch_name(tokens: &[JsToken], start: usize, declared: &mut HashSet<String>) {
    let mut index = start;
    while index < tokens.len() && matches!(tokens[index].kind, JsKind::Whitespace | JsKind::Comment)
    {
        index += 1;
    }
    if index < tokens.len() && tokens[index].text == "(" {
        collect_params(tokens, index, declared);
    }
}

fn collect_params(tokens: &[JsToken], open: usize, declared: &mut HashSet<String>) {
    let mut depth = 1i32;
    let mut index = open + 1;
    let mut in_default = false;
    while index < tokens.len() {
        match tokens[index].text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            "," if depth == 1 => in_default = false,
            "=" if depth == 1 => in_default = true,
            _ if depth == 1 && !in_default && tokens[index].kind == JsKind::Ident => {
                declared.insert(tokens[index].text.clone());
            }
            _ => {}
        }
        index += 1;
    }
}

fn matching_open_paren(tokens: &[JsToken], close: usize) -> Option<usize> {
    let mut depth = 1i32;
    let mut index = close;
    while index > 0 {
        index -= 1;
        match tokens[index].text.as_str() {
            ")" => depth += 1,
            "(" => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn build_identifier_mapping(declared: &HashSet<String>, rng: &mut Rng) -> HashMap<String, String> {
    let mut used: HashSet<String> = declared.clone();
    let mut mapping = HashMap::new();
    for name in declared {
        let mut candidate = rng.js_identifier();
        while used.contains(&candidate) || is_keyword(&candidate) {
            candidate = rng.js_identifier();
        }
        used.insert(candidate.clone());
        mapping.insert(name.clone(), candidate);
    }
    mapping
}

fn rename_tokens(
    tokens: &[JsToken],
    mapping: &HashMap<String, String>,
    class_mapping: &HashMap<String, String>,
) -> (String, usize) {
    let mut output = String::new();
    let mut brace_stack: Vec<bool> = Vec::new();
    let mut renamed_occurrences = 0usize;

    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            JsKind::String => {
                output.push_str(&rewrite_string_classes(&token.text, class_mapping));
            }
            JsKind::Punct if token.text == "{" => {
                brace_stack.push(is_object_brace(tokens, index, &brace_stack));
                output.push_str(&token.text);
            }
            JsKind::Punct if token.text == "}" => {
                brace_stack.pop();
                output.push_str(&token.text);
            }
            JsKind::Ident => {
                let Some(replacement) = mapping.get(&token.text) else {
                    output.push_str(&token.text);
                    continue;
                };
                let previous = previous_significant(tokens, index).map(|i| tokens[i].text.as_str());
                let next = next_significant(tokens, index).map(|i| tokens[i].text.as_str());
                let in_object = brace_stack.last().copied().unwrap_or(false);

                if matches!(previous, Some("." | "?." | "#"))
                    || next == Some(":")
                    || (in_object && next == Some("(") && matches!(previous, Some("{" | ",")))
                    || (in_object && next == Some("=") && matches!(previous, Some("{" | ",")))
                {
                    output.push_str(&token.text);
                    continue;
                }

                if in_object
                    && matches!(next, Some("," | "}"))
                    && matches!(previous, Some("{" | ","))
                {
                    // 对象简写 `{el}` 必须展开成 `{el: v123}`，否则键名会被改掉。
                    output.push_str(&token.text);
                    output.push_str(": ");
                    output.push_str(replacement);
                } else {
                    output.push_str(replacement);
                }
                renamed_occurrences += 1;
            }
            _ => output.push_str(&token.text),
        }
    }
    (output, renamed_occurrences)
}

fn is_object_brace(tokens: &[JsToken], index: usize, brace_stack: &[bool]) -> bool {
    let Some(previous) = previous_significant(tokens, index) else {
        return false;
    };
    match tokens[previous].text.as_str() {
        "=" | "(" | "[" | "," | ":" | "?" | "return" | "=>" => {
            // `=> {` 是函数体，不是对象；其余上下文按对象处理。
            tokens[previous].text != "=>"
        }
        "{" => brace_stack.last().copied().unwrap_or(false),
        _ => false,
    }
}

fn rewrite_string_classes(text: &str, mapping: &HashMap<String, String>) -> String {
    if mapping.is_empty() || text.len() < 2 {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let quote = bytes[0];
    let mut output = String::with_capacity(text.len());
    output.push(quote as char);
    let mut index = 1;
    while index + 1 < bytes.len() {
        if bytes[index] == b'\\' {
            let mut end = (index + 2).min(bytes.len());
            while end < bytes.len() && !text.is_char_boundary(end) {
                end += 1;
            }
            output.push_str(&text[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'.'
            && bytes
                .get(index + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-'))
        {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len()
                && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'_' | b'-'))
            {
                end += 1;
            }
            if let Some(mapped) = mapping.get(&text[start..end]) {
                output.push('.');
                output.push_str(mapped);
                index = end;
                continue;
            }
        }
        let ch = text[index..]
            .chars()
            .next()
            .expect("index must be a char boundary");
        output.push(ch);
        index += ch.len_utf8();
    }
    if index < bytes.len() {
        output.push_str(&text[index..]);
    }
    output
}

fn next_significant(tokens: &[JsToken], index: usize) -> Option<usize> {
    (index + 1..tokens.len())
        .find(|&i| !matches!(tokens[i].kind, JsKind::Whitespace | JsKind::Comment))
}

fn previous_significant(tokens: &[JsToken], index: usize) -> Option<usize> {
    (0..index)
        .rev()
        .find(|&i| !matches!(tokens[i].kind, JsKind::Whitespace | JsKind::Comment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(source: &str) -> JsRenameResult {
        rewrite_script(source, &HashMap::new(), &mut Rng::new(), true)
    }

    #[test]
    fn renames_declared_locals_and_parameters() {
        let result = rewrite(
            "function(response){var payload=response.json();if(!response.ok){throw new Error('x');}return payload;}",
        );
        assert!(!result.code.contains("response"));
        assert!(!result.code.contains("payload"));
        assert!(result.identifier_coverage >= 0.8);
        assert!(result.renamed_occurrences >= 4);
    }

    #[test]
    fn preserves_properties_and_globals() {
        let result = rewrite(
            "var el=document.getElementById('x');el.textContent='ok';window.IntimeUI.token();",
        );
        assert!(result.code.contains("document.getElementById"));
        assert!(result.code.contains(".textContent"));
        assert!(result.code.contains("window.IntimeUI.token"));
        assert!(!result.code.contains("var el"));
    }

    #[test]
    fn expands_object_shorthand() {
        let result = rewrite("var el=document.body;var obj={el};");
        assert!(result.code.contains(": "));
        assert!(result.code.contains("el:"));
    }

    #[test]
    fn rewrites_class_selectors_in_strings() {
        let mapping = HashMap::from([("card".to_string(), "c123".to_string())]);
        let result = rewrite_script(
            "var x=document.querySelector('.card');",
            &mapping,
            &mut Rng::new(),
            true,
        );
        assert!(result.code.contains("'.c123'"));
    }

    #[test]
    fn basic_level_only_rewrites_class_selectors() {
        let mapping = HashMap::from([("card".to_string(), "c123".to_string())]);
        let result = rewrite_script(
            "var el=document.querySelector('.card');",
            &mapping,
            &mut Rng::new(),
            false,
        );
        assert!(result.code.contains("'.c123'"));
        assert!(result.code.contains("var el"));
        assert_eq!(result.declared_identifiers, 0);
    }

    #[test]
    fn preserves_non_ascii_string_content() {
        let mapping = HashMap::from([("card".to_string(), "c123".to_string())]);
        let result = rewrite_script(
            "var msg='已复制 ✓';var el=document.querySelector('.card');",
            &mapping,
            &mut Rng::new(),
            true,
        );
        assert!(result.code.contains("已复制 ✓"));
        assert!(result.code.contains("'.c123'"));
    }

    #[test]
    fn default_parameter_expressions_are_not_renamed() {
        let result = rewrite("function f(a, b = outerValue) { return a + b; }");
        assert!(result.code.contains("outerValue"));
    }

    #[test]
    fn lexer_handles_regex_and_comments() {
        let tokens = tokenize_for_test("var re=/a\\/b/g; // comment\nvar x=1;");
        assert!(tokens.iter().any(|token| token.kind == JsKind::Regex));
        assert!(tokens.iter().any(|token| token.kind == JsKind::Comment));
    }
}
