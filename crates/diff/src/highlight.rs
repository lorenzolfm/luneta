#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Rust,
    Nix,
    TypeScript,
    Python,
    Go,
    Swift,
    Shell,
    Proto,
    Toml,
    Json,
    Yaml,
    Markdown,
    Plain,
}

impl Language {
    #[must_use]
    pub fn from_path(path: &str) -> Language {
        let name = path.rsplit('/').next().unwrap_or(path);
        let lower_name = name.to_ascii_lowercase();
        if matches!(lower_name.as_str(), "makefile" | "dockerfile" | ".envrc") {
            return Language::Shell;
        }
        let Some((_, ext)) = name.rsplit_once('.') else {
            return Language::Plain;
        };
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Language::Rust,
            "nix" => Language::Nix,
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Language::TypeScript,
            "py" | "pyi" => Language::Python,
            "go" => Language::Go,
            "swift" => Language::Swift,
            "sh" | "bash" | "zsh" | "fish" => Language::Shell,
            "proto" => Language::Proto,
            "toml" => Language::Toml,
            "json" | "jsonc" => Language::Json,
            "yml" | "yaml" => Language::Yaml,
            "md" | "markdown" => Language::Markdown,
            _ => Language::Plain,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxRole {
    Plain,
    Keyword,
    Type,
    String,
    Number,
    Comment,
    Macro,
    Attribute,
    Operator,
    Punctuation,
    Heading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenSpan {
    pub start: usize,
    pub end: usize,
    pub role: SyntaxRole,
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];

const NIX_KEYWORDS: &[&str] = &[
    "assert", "else", "false", "if", "import", "in", "inherit", "let", "null", "or", "rec", "then",
    "true", "with",
];

const TS_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
];

const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is",
    "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while",
    "with", "yield",
];

const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];

const SWIFT_KEYWORDS: &[&str] = &[
    "actor",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "do",
    "else",
    "enum",
    "extension",
    "false",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "internal",
    "is",
    "let",
    "nil",
    "open",
    "private",
    "protocol",
    "public",
    "repeat",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];

const SHELL_KEYWORDS: &[&str] = &[
    "case", "do", "done", "elif", "else", "end", "esac", "export", "fi", "for", "function", "if",
    "in", "local", "return", "set", "then", "until", "while",
];

const PROTO_KEYWORDS: &[&str] = &[
    "bool",
    "bytes",
    "double",
    "enum",
    "extend",
    "extensions",
    "false",
    "fixed32",
    "fixed64",
    "float",
    "import",
    "int32",
    "int64",
    "map",
    "message",
    "oneof",
    "option",
    "optional",
    "package",
    "public",
    "repeated",
    "reserved",
    "returns",
    "rpc",
    "service",
    "sfixed32",
    "sfixed64",
    "sint32",
    "sint64",
    "stream",
    "string",
    "syntax",
    "to",
    "true",
    "uint32",
    "uint64",
];

const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16",
    "u32", "u64", "u128", "usize", "number", "string", "boolean", "int", "float", "bytes",
];

#[must_use]
pub fn highlight_line(line: &str, language: Language) -> Vec<TokenSpan> {
    if line.is_empty() || language == Language::Plain {
        return Vec::new();
    }
    let spans = match language {
        Language::Markdown => highlight_markdown(line),
        Language::Toml => highlight_toml(line),
        Language::Yaml => highlight_yaml(line),
        Language::Json => highlight_json(line),
        _ => highlight_code(line, language),
    };
    assert_valid_token_spans(line, &spans);
    spans
}

fn assert_valid_token_spans(line: &str, spans: &[TokenSpan]) {
    let mut previous_end = 0usize;
    for span in spans {
        assert!(span.start < span.end, "token span is non-empty");
        assert!(span.start >= previous_end, "token spans are sorted and non-overlapping");
        assert!(span.end <= line.len(), "token span stays in line");
        assert!(line.is_char_boundary(span.start), "token span start is UTF-8 aligned");
        assert!(line.is_char_boundary(span.end), "token span end is UTF-8 aligned");
        previous_end = span.end;
    }
}

fn highlight_markdown(line: &str) -> Vec<TokenSpan> {
    let trimmed = line.trim_start();
    let offset = line.len() - trimmed.len();
    if trimmed.starts_with('#') {
        return vec![TokenSpan {
            start: offset,
            end: line.len(),
            role: SyntaxRole::Heading,
        }];
    }
    if trimmed.starts_with("```") {
        return vec![TokenSpan {
            start: offset,
            end: line.len(),
            role: SyntaxRole::Comment,
        }];
    }
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let start = at;
            at += 1;
            while at < bytes.len() && bytes[at] != b'`' {
                at += 1;
            }
            if at < bytes.len() {
                at += 1;
            }
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::String,
            });
        } else {
            at += next_char_len(line, at);
        }
    }
    spans
}

fn highlight_toml(line: &str) -> Vec<TokenSpan> {
    let trimmed = line.trim_start();
    let offset = line.len() - trimmed.len();
    if trimmed.starts_with('#') {
        return vec![TokenSpan {
            start: offset,
            end: line.len(),
            role: SyntaxRole::Comment,
        }];
    }
    if trimmed.starts_with('[') {
        let end = trimmed.find('#').map_or(line.len(), |pos| offset + pos);
        let mut spans = vec![TokenSpan {
            start: offset,
            end,
            role: SyntaxRole::Heading,
        }];
        if end < line.len() {
            spans.push(TokenSpan {
                start: end,
                end: line.len(),
                role: SyntaxRole::Comment,
            });
        }
        return spans;
    }
    highlight_key_value(line, b'#')
}

fn highlight_yaml(line: &str) -> Vec<TokenSpan> {
    let trimmed = line.trim_start();
    let offset = line.len() - trimmed.len();
    if trimmed.starts_with('#') {
        return vec![TokenSpan {
            start: offset,
            end: line.len(),
            role: SyntaxRole::Comment,
        }];
    }
    let mut spans = Vec::new();
    if let Some(colon) = find_yaml_colon(line) {
        let key_start = offset + usize::from(trimmed.starts_with("- ")) * 2;
        if key_start < colon {
            spans.push(TokenSpan {
                start: key_start,
                end: colon,
                role: SyntaxRole::Keyword,
            });
        }
        spans.extend(scan_values(&line[colon + 1..], colon + 1, b'#'));
        return spans;
    }
    scan_values(line, 0, b'#')
}

fn find_yaml_colon(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut in_quote = None;
    for (idx, &byte) in bytes.iter().enumerate() {
        match in_quote {
            Some(q) if byte == q => in_quote = None,
            None if byte == b'"' || byte == b'\'' => in_quote = Some(byte),
            None if byte == b'#' => return None,
            None if byte == b':' && (idx + 1 == bytes.len() || bytes[idx + 1] == b' ') => {
                return Some(idx);
            }
            Some(_) | None => {}
        }
    }
    None
}

fn highlight_key_value(line: &str, comment: u8) -> Vec<TokenSpan> {
    let mut spans = Vec::new();
    if let Some(eq) = line.find('=') {
        let key = line[..eq].trim();
        if !key.is_empty() {
            let start = line.len() - line.trim_start().len();
            let end = start + key.len();
            if end <= eq {
                spans.push(TokenSpan {
                    start,
                    end,
                    role: SyntaxRole::Keyword,
                });
            }
        }
        spans.push(TokenSpan {
            start: eq,
            end: eq + 1,
            role: SyntaxRole::Operator,
        });
        spans.extend(scan_values(&line[eq + 1..], eq + 1, comment));
        return spans;
    }
    scan_values(line, 0, comment)
}

fn highlight_json(line: &str) -> Vec<TokenSpan> {
    let mut spans = Vec::new();
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                let start = at;
                at = scan_quoted(bytes, at, b'"');
                let rest = line[at..].trim_start();
                let role = if rest.starts_with(':') {
                    SyntaxRole::Keyword
                } else {
                    SyntaxRole::String
                };
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role,
                });
            }
            b'0'..=b'9' | b'-' if starts_number(bytes, at) => {
                let start = at;
                at = scan_number(bytes, at);
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role: SyntaxRole::Number,
                });
            }
            b'a'..=b'z' => {
                let start = at;
                at = scan_ident(bytes, at);
                if matches!(&line[start..at], "true" | "false" | "null") {
                    spans.push(TokenSpan {
                        start,
                        end: at,
                        role: SyntaxRole::Keyword,
                    });
                }
            }
            _ => at += next_char_len(line, at),
        }
    }
    spans
}

fn scan_values(slice: &str, base: usize, comment: u8) -> Vec<TokenSpan> {
    let mut spans = Vec::new();
    let bytes = slice.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == comment {
            spans.push(TokenSpan {
                start: base + at,
                end: base + bytes.len(),
                role: SyntaxRole::Comment,
            });
            break;
        }
        if byte == b'"' || byte == b'\'' {
            let start = at;
            at = scan_quoted(bytes, at, byte);
            spans.push(TokenSpan {
                start: base + start,
                end: base + at,
                role: SyntaxRole::String,
            });
        } else if starts_number(bytes, at) {
            let start = at;
            at = scan_number(bytes, at);
            spans.push(TokenSpan {
                start: base + start,
                end: base + at,
                role: SyntaxRole::Number,
            });
        } else if byte.is_ascii_alphabetic() {
            let start = at;
            at = scan_ident(bytes, at);
            if matches!(&slice[start..at], "true" | "false" | "null" | "yes" | "no") {
                spans.push(TokenSpan {
                    start: base + start,
                    end: base + at,
                    role: SyntaxRole::Keyword,
                });
            }
        } else {
            at += next_char_len(slice, at);
        }
    }
    spans
}

fn highlight_code(line: &str, language: Language) -> Vec<TokenSpan> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if language == Language::Rust && (trimmed.starts_with("#[") || trimmed.starts_with("#![")) {
        return vec![TokenSpan {
            start: indent,
            end: line.len(),
            role: SyntaxRole::Attribute,
        }];
    }
    let bytes = line.as_bytes();
    let hash_comment = matches!(language, Language::Nix | Language::Python | Language::Shell);
    let slash_comment = matches!(
        language,
        Language::Rust | Language::TypeScript | Language::Go | Language::Swift | Language::Proto
    );
    let keywords = keywords_for(language);
    let mut spans = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if slash_comment && byte == b'/' && bytes.get(at + 1) == Some(&b'/') {
            spans.push(TokenSpan {
                start: at,
                end: bytes.len(),
                role: SyntaxRole::Comment,
            });
            break;
        }
        if hash_comment && byte == b'#' {
            spans.push(TokenSpan {
                start: at,
                end: bytes.len(),
                role: SyntaxRole::Comment,
            });
            break;
        }
        if slash_comment && byte == b'/' && bytes.get(at + 1) == Some(&b'*') {
            let start = at;
            at = scan_block_comment(bytes, at);
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::Comment,
            });
            continue;
        }
        if is_quoted_literal_start(bytes, at, language) {
            let start = at;
            at = scan_quoted(bytes, at, byte);
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::String,
            });
            continue;
        }
        if language == Language::Rust
            && byte == b'\''
            && bytes.get(at + 1).is_some_and(u8::is_ascii_alphabetic)
        {
            let start = at;
            at = scan_ident(bytes, at + 1);
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::Type,
            });
            continue;
        }
        if matches!(
            language,
            Language::Python | Language::Swift | Language::TypeScript
        ) && byte == b'@'
        {
            let start = at;
            at = scan_ident(bytes, at + 1);
            if at > start + 1 {
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role: SyntaxRole::Attribute,
                });
                continue;
            }
        }
        if byte.is_ascii_digit() {
            let start = at;
            at = scan_number(bytes, at);
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::Number,
            });
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = at;
            at = scan_ident(bytes, at);
            let word = &line[start..at];
            if language == Language::Rust && bytes.get(at) == Some(&b'!') {
                at += 1;
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role: SyntaxRole::Macro,
                });
            } else if keywords.contains(&word) {
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role: SyntaxRole::Keyword,
                });
            } else if PRIMITIVE_TYPES.contains(&word) || is_pascal_type(word) {
                spans.push(TokenSpan {
                    start,
                    end: at,
                    role: SyntaxRole::Type,
                });
            }
            continue;
        }
        if matches!(
            byte,
            b'=' | b'!' | b'<' | b'>' | b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^'
        ) {
            let start = at;
            at += 1;
            spans.push(TokenSpan {
                start,
                end: at,
                role: SyntaxRole::Operator,
            });
            continue;
        }
        at += next_char_len(line, at);
    }
    spans
}

fn is_string_quote(bytes: &[u8], at: usize, language: Language) -> bool {
    if language != Language::Rust {
        return true;
    }
    let Some(&next) = bytes.get(at + 1) else {
        return false;
    };
    if next == b'\\' {
        return bytes.get(at + 3) == Some(&b'\'') || bytes.get(at + 4) == Some(&b'\'');
    }
    bytes.get(at + 2) == Some(&b'\'')
}

fn is_pascal_type(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase() && chars.any(|ch| ch.is_ascii_lowercase())
}

fn keywords_for(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => RUST_KEYWORDS,
        Language::Nix => NIX_KEYWORDS,
        Language::TypeScript => TS_KEYWORDS,
        Language::Python => PYTHON_KEYWORDS,
        Language::Go => GO_KEYWORDS,
        Language::Swift => SWIFT_KEYWORDS,
        Language::Shell => SHELL_KEYWORDS,
        Language::Proto => PROTO_KEYWORDS,
        Language::Toml | Language::Json | Language::Yaml | Language::Markdown | Language::Plain => {
            &[]
        }
    }
}

fn is_quoted_literal_start(bytes: &[u8], at: usize, language: Language) -> bool {
    assert!(at < bytes.len(), "quote check stays in bounds");
    match bytes[at] {
        b'"' | b'`' => true,
        b'\'' => is_string_quote(bytes, at, language),
        _ => false,
    }
}

fn scan_block_comment(bytes: &[u8], start: usize) -> usize {
    assert!(start + 1 < bytes.len(), "block comment starts with two bytes");
    let mut at = start + 2;
    while at + 1 < bytes.len() && !(bytes[at] == b'*' && bytes[at + 1] == b'/') {
        at += 1;
    }
    let end = if at + 1 < bytes.len() {
        at + 2
    } else {
        bytes.len()
    };
    assert!(end > start && end <= bytes.len(), "block comment scan advances");
    end
}

fn starts_number(bytes: &[u8], at: usize) -> bool {
    assert!(at < bytes.len(), "number check stays in bounds");
    if bytes[at].is_ascii_digit() {
        return true;
    }
    bytes[at] == b'-' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit)
}

fn scan_number(bytes: &[u8], start: usize) -> usize {
    assert!(start < bytes.len(), "number scan starts in bounds");
    let mut at = start;
    if bytes.get(at) == Some(&b'-') {
        at += 1;
    }
    while at < bytes.len()
        && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] == b'.')
    {
        at += 1;
    }
    assert!(at > start && at <= bytes.len(), "number scan advances");
    at
}

fn scan_ident(bytes: &[u8], start: usize) -> usize {
    assert!(start <= bytes.len(), "identifier scan starts in bounds");
    let mut at = start;
    while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
        at += 1;
    }
    assert!(at <= bytes.len(), "identifier scan stays in bounds");
    at
}

fn scan_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    assert!(start < bytes.len(), "quoted scan starts in bounds");
    let mut at = start + 1;
    while at < bytes.len() {
        if bytes[at] == b'\\' && at + 1 < bytes.len() {
            at += 2;
            continue;
        }
        if bytes[at] == quote {
            let end = at + 1;
            assert!(end > start && end <= bytes.len(), "closed quote advances");
            return end;
        }
        at += 1;
    }
    assert!(bytes.len() > start, "unclosed quote advances to line end");
    bytes.len()
}

fn next_char_len(text: &str, at: usize) -> usize {
    assert!(at < text.len(), "character step starts in bounds");
    assert!(text.is_char_boundary(at), "character step starts on UTF-8 boundary");
    let len = text[at..].chars().next().map_or(1, char::len_utf8);
    assert!(len >= 1 && at + len <= text.len(), "character step advances in bounds");
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_is_inferred_from_file_extension() {
        assert_eq!(
            Language::from_path("crates/diff/src/lib.rs"),
            Language::Rust
        );
        assert_eq!(Language::from_path("flake.nix"), Language::Nix);
        assert_eq!(
            Language::from_path("proto/lune/v1/daemon.proto"),
            Language::Proto
        );
        assert_eq!(Language::from_path("README.md"), Language::Markdown);
        assert_eq!(Language::from_path("Cargo.toml"), Language::Toml);
        assert_eq!(Language::from_path("LICENSE"), Language::Plain);
    }

    #[test]
    fn rust_line_highlights_keywords_types_numbers_strings_and_attributes() {
        let attr = highlight_line("    #[serde(rename = \"repoCount\")]", Language::Rust);
        assert_eq!(attr.len(), 1);
        assert_eq!(attr[0].role, SyntaxRole::Attribute);

        let line = "const MAX_BYTES: usize = 4 * 1024; // limit";
        let spans = highlight_line(line, Language::Rust);
        assert!(
            spans
                .iter()
                .any(|s| &line[s.start..s.end] == "const" && s.role == SyntaxRole::Keyword)
        );
        assert!(
            spans
                .iter()
                .any(|s| &line[s.start..s.end] == "usize" && s.role == SyntaxRole::Type)
        );
        assert!(
            spans
                .iter()
                .any(|s| &line[s.start..s.end] == "1024" && s.role == SyntaxRole::Number)
        );
        assert!(
            spans
                .iter()
                .any(|s| &line[s.start..s.end] == "// limit" && s.role == SyntaxRole::Comment)
        );
    }

    #[test]
    fn spans_are_sorted_non_overlapping_and_utf8_aligned() {
        let line = "let greeting = \"héllo ☾\"; format!(\"{greeting}\")";
        let spans = highlight_line(line, Language::Rust);
        let mut prev = 0;
        for span in spans {
            assert!(span.start >= prev);
            assert!(span.start < span.end);
            assert!(span.end <= line.len());
            assert!(line.is_char_boundary(span.start));
            assert!(line.is_char_boundary(span.end));
            prev = span.end;
        }
    }
}
