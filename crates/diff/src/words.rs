#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

const TOKENS_MAX: usize = 192;

const PAIR_LINES_MAX: usize = 32;

const _: () = assert!(TOKENS_MAX > 0);
const _: () = assert!(TOKENS_MAX <= u16::MAX as usize);
const _: () = assert!(PAIR_LINES_MAX > 0);

#[must_use]
pub fn pair_emphasis(deleted: &[&str], added: &[&str]) -> (Vec<Vec<ByteSpan>>, Vec<Vec<ByteSpan>>) {
    let mut deleted_spans = vec![Vec::new(); deleted.len()];
    let mut added_spans = vec![Vec::new(); added.len()];
    if deleted.is_empty() || added.is_empty() {
        return (deleted_spans, added_spans);
    }
    if deleted.len() > PAIR_LINES_MAX || added.len() > PAIR_LINES_MAX {
        return (deleted_spans, added_spans);
    }
    let pairs = deleted.len().min(added.len());
    assert!(pairs <= PAIR_LINES_MAX, "paired line count stays bounded");
    for index in 0..pairs {
        let (left, right) = line_emphasis(deleted[index], added[index]);
        deleted_spans[index] = left;
        added_spans[index] = right;
    }
    assert_eq!(deleted_spans.len(), deleted.len(), "deleted span count matches lines");
    assert_eq!(added_spans.len(), added.len(), "added span count matches lines");
    (deleted_spans, added_spans)
}

#[must_use]
pub fn line_emphasis(old: &str, new: &str) -> (Vec<ByteSpan>, Vec<ByteSpan>) {
    if old == new {
        return (Vec::new(), Vec::new());
    }
    let Some(old_tokens) = tokenize_bounded(old, TOKENS_MAX) else {
        return fallback_affix(old, new);
    };
    let Some(new_tokens) = tokenize_bounded(new, TOKENS_MAX) else {
        return fallback_affix(old, new);
    };
    assert!(old_tokens.len() <= TOKENS_MAX, "old token count fits DP table");
    assert!(new_tokens.len() <= TOKENS_MAX, "new token count fits DP table");

    let rows = old_tokens.len() + 1;
    let cols = new_tokens.len() + 1;
    let mut table = vec![0u16; rows * cols];
    for old_index in (0..old_tokens.len()).rev() {
        for new_index in (0..new_tokens.len()).rev() {
            let cell = if old_tokens[old_index].text == new_tokens[new_index].text {
                table[(old_index + 1) * cols + (new_index + 1)] + 1
            } else {
                table[(old_index + 1) * cols + new_index]
                    .max(table[old_index * cols + (new_index + 1)])
            };
            table[old_index * cols + new_index] = cell;
        }
    }
    let mut old_matched = vec![false; old_tokens.len()];
    let mut new_matched = vec![false; new_tokens.len()];
    let (mut old_index, mut new_index) = (0usize, 0usize);
    while old_index < old_tokens.len() && new_index < new_tokens.len() {
        if old_tokens[old_index].text == new_tokens[new_index].text {
            old_matched[old_index] = true;
            new_matched[new_index] = true;
            old_index += 1;
            new_index += 1;
        } else if table[(old_index + 1) * cols + new_index]
            >= table[old_index * cols + (new_index + 1)]
        {
            old_index += 1;
        } else {
            new_index += 1;
        }
    }
    let common_non_ws: usize = old_tokens
        .iter()
        .zip(&old_matched)
        .filter(|(token, matched)| **matched && !token.text.trim().is_empty())
        .map(|(token, _)| token.text.len())
        .sum();
    let total_non_ws = old.trim().len().max(new.trim().len());
    if total_non_ws > 0 && common_non_ws * 4 < total_non_ws {
        return (Vec::new(), Vec::new());
    }
    let left = merge_unmatched(old, &old_tokens, &old_matched);
    let right = merge_unmatched(new, &new_tokens, &new_matched);
    (left, right)
}

#[derive(Clone, Copy)]
struct Token<'a> {
    start: usize,
    end: usize,
    text: &'a str,
}

fn tokenize_bounded(line: &str, tokens_max: usize) -> Option<Vec<Token<'_>>> {
    assert!(tokens_max > 0, "token limit must be positive");
    let mut tokens = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if tokens.len() >= tokens_max {
            return None;
        }
        let mut end = start + ch.len_utf8();
        if ch.is_alphanumeric() || ch == '_' {
            while let Some(&(pos, next)) = chars.peek() {
                if next.is_alphanumeric() || next == '_' {
                    end = pos + next.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
        } else if ch.is_whitespace() {
            while let Some(&(pos, next)) = chars.peek() {
                if next.is_whitespace() {
                    end = pos + next.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
        }
        assert!(start < end, "every token is non-empty");
        assert!(end <= line.len(), "token stays within line bytes");
        tokens.push(Token {
            start,
            end,
            text: &line[start..end],
        });
    }
    assert!(tokens.len() <= tokens_max, "returned tokens respect bound");
    Some(tokens)
}

fn merge_unmatched(line: &str, tokens: &[Token<'_>], matched: &[bool]) -> Vec<ByteSpan> {
    assert_eq!(tokens.len(), matched.len(), "token and match slices align");
    let mut spans: Vec<ByteSpan> = Vec::new();
    for (token, &is_matched) in tokens.iter().zip(matched) {
        if is_matched {
            continue;
        }
        if let Some(last) = spans.last_mut()
            && last.end == token.start
        {
            last.end = token.end;
        } else {
            spans.push(ByteSpan {
                start: token.start,
                end: token.end,
            });
        }
    }
    assert_valid_byte_spans(line, &spans);
    spans
}

fn fallback_affix(old: &str, new: &str) -> (Vec<ByteSpan>, Vec<ByteSpan>) {
    let prefix = old
        .char_indices()
        .zip(new.char_indices())
        .take_while(|((_, a), (_, b))| a == b)
        .map(|((index, ch), _)| index + ch.len_utf8())
        .last()
        .unwrap_or(0);
    assert!(prefix <= old.len(), "common prefix fits old line");
    assert!(prefix <= new.len(), "common prefix fits new line");
    let old_rest = &old[prefix..];
    let new_rest = &new[prefix..];
    let suffix = old_rest
        .chars()
        .rev()
        .zip(new_rest.chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    let old_end = old.len().saturating_sub(suffix);
    let new_end = new.len().saturating_sub(suffix);
    let left = if prefix < old_end {
        vec![ByteSpan {
            start: prefix,
            end: old_end,
        }]
    } else {
        Vec::new()
    };
    let right = if prefix < new_end {
        vec![ByteSpan {
            start: prefix,
            end: new_end,
        }]
    } else {
        Vec::new()
    };
    assert_valid_byte_spans(old, &left);
    assert_valid_byte_spans(new, &right);
    (left, right)
}

fn assert_valid_byte_spans(line: &str, spans: &[ByteSpan]) {
    let mut previous_end = 0usize;
    for span in spans {
        assert!(span.start < span.end, "emphasis span is non-empty");
        assert!(span.start >= previous_end, "emphasis spans are sorted and non-overlapping");
        assert!(span.end <= line.len(), "emphasis span stays in line");
        assert!(line.is_char_boundary(span.start), "emphasis start is UTF-8 aligned");
        assert!(line.is_char_boundary(span.end), "emphasis end is UTF-8 aligned");
        previous_end = span.end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_factor_is_highlighted_on_right_alone() {
        let old = "const MAX_RESPONSE_BYTES: usize = 1024 * 1024;";
        let new = "const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;";
        let (left, right) = line_emphasis(old, new);
        assert_eq!(left, []);
        assert_eq!(right.len(), 1);
        assert_eq!(&new[right[0].start..right[0].end], "4 * ");
    }

    #[test]
    fn renamed_struct_highlights_only_the_identifier() {
        let old = "pub(crate) struct RepoCommits {";
        let new = "pub(crate) struct CommitGroup {";
        let (left, right) = line_emphasis(old, new);
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_eq!(&old[left[0].start..left[0].end], "RepoCommits");
        assert_eq!(&new[right[0].start..right[0].end], "CommitGroup");
    }

    #[test]
    fn unrelated_lines_produce_no_noisy_emphasis() {
        let (left, right) = line_emphasis(
            "    #[serde(rename = \"repoCount\")]",
            "    groups: Vec<CommitGroup>,",
        );
        assert_eq!(left, []);
        assert_eq!(right, []);
    }

    #[test]
    fn long_lines_past_tokens_max_fall_back_to_affix_without_panic() {
        let old = format!("{}old{}", "a ".repeat(250), " b".repeat(250));
        let new = format!("{}new{}", "a ".repeat(250), " b".repeat(250));
        let (left, right) = line_emphasis(&old, &new);
        assert_eq!(left.len(), 1);
        assert_eq!(right.len(), 1);
        assert_eq!(&old[left[0].start..left[0].end], "old");
        assert_eq!(&new[right[0].start..right[0].end], "new");
    }
}
