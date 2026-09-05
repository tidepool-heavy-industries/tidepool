//! Whitespace-only layout for ordinary single-line Haskell `Show` output.

/// Custom or ambiguous output is returned verbatim. This is presentation only:
/// neither constructor names nor rendered text determine runtime behavior.
pub(crate) fn layout(text: &str) -> String {
    if text.chars().count() <= 100 || text.contains(['\n', '\r']) {
        return text.to_owned();
    }
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut breaks = Vec::new();
    for (offset, ch) in text.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }
        match ch {
            '\"' | '\'' => quote = Some(ch),
            '(' | '[' | '{' => stack.push(ch),
            ')' | ']' | '}' => {
                let expected = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.pop() != Some(expected) {
                    return text.to_owned();
                }
            }
            ',' if !stack.is_empty() => breaks.push((offset + 1, stack.len())),
            _ => {}
        }
    }
    if quote.is_some() || !stack.is_empty() {
        return text.to_owned();
    }
    let mut output = String::new();
    let mut start = 0;
    let mut column = 0;
    for (end, depth) in breaks {
        let segment = &text[start..end];
        output.push_str(segment);
        column += segment.chars().count();
        if column >= 100 {
            output.push('\n');
            let indent = (depth * 2).min(20);
            output.extend(std::iter::repeat_n(' ', indent));
            start = end + text[end..].len() - text[end..].trim_start_matches(' ').len();
            column = indent;
        } else {
            start = end;
        }
    }
    output.push_str(&text[start..]);
    output
}

#[cfg(test)]
#[path = "workbench_display_tests.rs"]
mod tests;
