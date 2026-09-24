//! Transport output limits and literal command-page presentation.

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

/// The token `Project.Shell.recoveryFor` (bridge/haskell, `.exomonad/workspace/Project/Shell.hs`)
/// writes in place of the retained `Cmd.Job` binding it cannot name itself:
/// the host only mints and announces that binding ("retained as ... ::
/// Cmd.Job") after the tool body has already returned its text. Must match
/// `jobBindingPlaceholder` there exactly.
pub(crate) const JOB_BINDING_PLACEHOLDER: &str = "{{job_binding}}";

/// Fill in the real binding name once the host knows it, the same place and
/// the same fact it already names in the "retained as ... :: Cmd.Job" line.
/// A no-op when the placeholder is absent (most workbench output never
/// mentions it), and left unresolved only when no binding was retained for
/// this step, which should not happen for a command tool's own presenter.
pub(crate) fn resolve_job_binding_placeholder(output: String, installed_bindings: &[String]) -> String {
    match installed_bindings.first() {
        Some(binding) if output.contains(JOB_BINDING_PLACEHOLDER) => {
            output.replace(JOB_BINDING_PLACEHOLDER, binding)
        }
        _ => output,
    }
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
