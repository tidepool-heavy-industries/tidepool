//! `spawn_blocking` loses tracing span context: the blocking-pool thread
//! tokio schedules the closure onto has no `tracing::Span::current()` of its
//! own, so any span the caller was inside when it called `spawn_blocking`
//! does not automatically cover the closure. Fixing this per call site is
//! easy to forget at exactly the site (a compile blocking call) whose
//! diagnostics matter most. This is the one place that captures the calling
//! span and re-enters it inside the blocking closure, so every compile
//! blocking site shares the same fix instead of copying it.

use tracing::Span;

/// As [`tokio::task::spawn_blocking`], but the blocking closure runs inside
/// the span active at the call site (captured via [`Span::current`]) rather
/// than with no span at all. Any `tracing` line the closure emits — directly
/// or through code it calls — is attributed to the caller's span tree
/// (its `compile_request`/cell-execution ancestry included), exactly as if
/// the closure had run inline on the calling task.
pub fn spawn_blocking_in_span<F, R>(f: F) -> tokio::task::JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let span = Span::current();
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the one sanctioned call site the disallowed-methods reason points to"
    )]
    {
        tokio::task::spawn_blocking(move || span.in_scope(f))
    }
}

#[cfg(test)]
mod tests {
    use super::spawn_blocking_in_span;
    use tracing::Level;

    /// `spawn_blocking_in_span` must carry the calling span's identity into
    /// the blocking closure — which runs on a tokio blocking-pool thread, a
    /// thread `Span::current()` was never entered on through the ordinary
    /// per-thread stack. Global-default the subscriber (visible from any
    /// thread, unlike `with_default`'s thread-local override) and read the
    /// span name back through `tracing_subscriber::Registry`'s own
    /// current-span lookup from inside the closure.
    #[tokio::test]
    async fn carries_the_calling_span_into_the_blocking_closure() {
        let registry = tracing_subscriber::registry();
        // best-effort: another test in the process may have already installed
        // the global subscriber; that's fine, this test only needs one present.
        drop(tracing::subscriber::set_global_default(registry));

        let span = tracing::span!(Level::INFO, "compile_request", actor = "probe");
        let _entered = span.enter();

        let seen_name = spawn_blocking_in_span(|| {
            tracing::Span::current().metadata().map(|m| m.name())
        })
        .await
        .unwrap();

        assert_eq!(
            seen_name,
            Some("compile_request"),
            "the blocking closure must observe the caller's span as its current span"
        );
    }

    /// With no span active at the call site, the closure observes no span
    /// either — the helper propagates whatever was current, it does not
    /// invent one.
    #[tokio::test]
    async fn propagates_no_span_when_none_was_current() {
        let seen_name = spawn_blocking_in_span(|| {
            tracing::Span::current().metadata().map(|m| m.name())
        })
        .await
        .unwrap();
        assert_eq!(seen_name, None);
    }
}
