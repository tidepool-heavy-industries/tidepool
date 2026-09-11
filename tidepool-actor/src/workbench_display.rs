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

/// Bound model-visible output in bytes, retaining useful beginning/end context.
pub fn bounded_output(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let boundary = |mut offset: usize| {
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    if budget < 192 {
        return text[..boundary(budget)].to_owned();
    }
    let available = budget - 192;
    let mut head = boundary(available / 2);
    if let Some(newline) = text[..head].rfind('\n') {
        head = newline + 1;
    }
    let mut tail = boundary(text.len() - available / 2);
    if let Some(newline) = text[tail..].find('\n') {
        if tail + newline + 1 < text.len() {
            tail += newline + 1;
        }
    }
    let omitted = tail - head;
    format!(
        "{}\n[… {omitted} output bytes not displayed]\n{}",
        &text[..head],
        &text[tail..]
    )
}

/// Command output remains literal text; stream positions explain omissions
/// without claiming an ordering between stdout and stderr.
pub(crate) fn command_pages(
    pages: &[(
        tidepool_bridge_effects::CommandStream,
        tidepool_bridge_effects::CommandPage,
    )],
) -> String {
    let mut output = String::new();
    let mut ends = [None, None];
    for (stream, page) in pages {
        let stream_index = match stream {
            tidepool_bridge_effects::CommandStream::Stdout => 0,
            tidepool_bridge_effects::CommandStream::Stderr => 1,
        };
        if let Some(previous) = ends[stream_index] {
            if page.start > previous {
                output.push_str(&format!(
                    "\n[… {} retained bytes between displayed pages.]\n",
                    page.start - previous
                ));
            }
        }
        ends[stream_index] = Some(page.end);
        let label = match stream {
            tidepool_bridge_effects::CommandStream::Stdout => "stdout",
            tidepool_bridge_effects::CommandStream::Stderr => "stderr",
        };
        output.push_str(&format!(
            "\n{label} · bytes {}–{} of {}",
            page.start, page.end, page.available_end
        ));
        if page.lost_bytes > 0 {
            output.push_str(&format!(" · {} bytes no longer retained", page.lost_bytes));
        }
        if page.lossy {
            output.push_str(" · lossy UTF-8");
        }
        if page.leading_fragment {
            output.push_str(" · leading fragment");
        }
        if page.trailing_fragment {
            output.push_str(" · trailing fragment");
        }
        output.push_str(":\n");
        output.push_str(&page.text);
    }
    output
}
