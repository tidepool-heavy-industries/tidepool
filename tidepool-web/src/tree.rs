//! The d3 tree view: `GET /` serves a full-viewport, zoomable/pannable SVG
//! canvas — one circle per registered node, laid out by `d3.hierarchy` +
//! `d3.tree`, colored by the SAME derived status [`crate::render::status`]
//! computes for the `/legacy` outline. Clicking a node opens a side pane
//! showing that node's full server-rendered panel (`GET
//! /node/{node}/panel`) — the exact fragment bytes the SSE stream patches,
//! reused verbatim rather than re-derived.
//!
//! # Vendored d3
//!
//! d3 v7.9.0 (<https://d3js.org>, single minified bundle, upstream file's own
//! header: `// https://d3js.org v7.9.0 Copyright 2010-2023 Mike Bostock`) is
//! vendored at `tidepool-web/assets/d3.v7.9.0.min.js` and served locally at
//! [`D3_ASSET_PATH`] — no CDN, no runtime fetch of third-party script. The
//! page loads it via an ordinary same-origin `<script src>`.
//!
//! # d3 idioms only — no hand-rolled layout/zoom math
//!
//! [`TREE_JS`] builds the hierarchy with `d3.stratify`, lays it out with
//! `d3.tree`, draws links with `d3.linkHorizontal`, joins data with
//! `selection.join`-style enter/update/exit (keyed by node id), and wires
//! pan/zoom with `d3.zoom` — every geometric computation is a d3 built-in.
//! The tree re-renders from freshly fetched `/api/tree` data on every SSE
//! tick rather than being hand-patched node by node.
//!
//! # Reusing [`crate::shell`]'s shared client plumbing
//!
//! [`TREE_JS`] calls straight into [`crate::shell::CORE_JS`]'s `wire`,
//! `toast`, `setConn`, and `applyPatch` — the SAME form-submission and
//! SSE-patch-apply code the `/legacy` outline uses, embedded as its own
//! `<script>` tag right before this one. `applyPatch`'s existing same-id
//! replace already does exactly what the side pane needs when the open
//! node's panel is patched over SSE (see `crate::shell::JS`'s docs on the
//! `data-rev` gate) — no page-specific reimplementation needed.
//!
//! # Escaping
//!
//! Every value the tree draws from `/api/tree`/`/node/{node}/panel` is
//! rendered with `.text()` (labels) or used only as `d3` data (never
//! interpolated into markup) — [`TREE_JS`] never calls `.html()` or sets
//! `.innerHTML` on a live, mounted DOM node with fetched content. The one
//! place raw HTML text is parsed (loading a panel fragment into the side
//! pane) uses the same detached-`<template>` parse-then-move idiom
//! [`crate::shell::CORE_JS`]'s `applyPatch` already uses for SSE frames —
//! the fragment itself is server-rendered, maud-escaped markup (see
//! `render.rs`'s injection story), not raw model text.

use maud::{html, Markup, PreEscaped, DOCTYPE};

use crate::shell;

/// Where the vendored d3 bundle is served from — versioned in the path so a
/// future d3 upgrade is a new URL, not a cache-invalidation problem.
pub const D3_ASSET_PATH: &str = "/assets/d3.v7.9.0.min.js";

/// The vendored d3 v7.9.0 bundle itself — see the module docs for
/// provenance. Bytes are exactly upstream's; nothing here modifies them.
pub const D3_JS: &str = include_str!("../assets/d3.v7.9.0.min.js");

/// The full tree-view document: a full-viewport `<svg>` canvas, a side pane
/// for the selected node's panel, `/legacy`'s [`shell::CSS`] (the side pane
/// renders that same panel markup, so it needs those same rules) plus
/// [`TREE_CSS`]'s tree/pane-specific additions, and three scripts in order:
/// the vendored d3 bundle, the shared [`shell::CORE_JS`], then this page's
/// own [`TREE_JS`].
pub fn tree_page() -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "tidepool — tree" }
                style { (PreEscaped(shell::CSS)) (PreEscaped(TREE_CSS)) }
                script src=(D3_ASSET_PATH) {}
                script { (PreEscaped(shell::CORE_JS)) }
                script { (PreEscaped(TREE_JS)) }
            }
            body {
                div id="app" {
                    header class="tree-masthead" {
                        span class="mark" { "tidepool" }
                        a class="legacy-link" href="/legacy" { "outline view" }
                        span id="conn" class="conn ok" { "live" }
                    }
                    svg id="tree-canvas" {
                        g class="viewport" {
                            g class="links" {}
                            g class="nodes" {}
                        }
                    }
                    aside id="side-pane" class="side-pane" {
                        button type="button" id="side-pane-close" aria-label="close" { "×" }
                        div id="side-pane-body" {}
                    }
                }
            }
        }
    }
}

/// Tree-canvas and side-pane styling, layered on top of [`shell::CSS`] (whose
/// `:root` tokens and `.node-panel`/`.field`/`.btn`/etc. rules this page
/// still needs — the side pane renders the exact same panel markup as
/// `/legacy`).
pub const TREE_CSS: &str = r#"
html, body { height: 100%; overflow: hidden; }
#app { position: relative; height: 100vh; width: 100vw; }

.tree-masthead {
  position: fixed; top: 0; left: 0; z-index: 5;
  display: flex; align-items: baseline; gap: calc(3 * var(--unit));
  padding: calc(2 * var(--unit)) calc(3 * var(--unit));
}
.tree-masthead .mark { font-weight: 700; letter-spacing: -0.02em; }
.legacy-link {
  font-size: var(--text-micro); text-transform: uppercase; letter-spacing: var(--tracking-wide);
  color: var(--muted); text-decoration: none; border-bottom: var(--hair-faint);
}
.tree-masthead .conn { margin-left: auto; }

#tree-canvas { display: block; width: 100%; height: 100%; background: var(--paper); cursor: grab; }
#tree-canvas:active { cursor: grabbing; }

.node { cursor: pointer; }
.node circle {
  fill: var(--paper); stroke: var(--ink); stroke-width: 1.5px;
  transition: stroke-width 0.12s ease;
}
.node.status-needs-you circle { fill: var(--accent); stroke: var(--accent); }
.node.status-running circle { stroke-width: 2px; }
.node.status-done circle, .node.status-ended circle { fill: var(--line-faint); stroke: var(--line-faint); }
.node.status-failed circle { stroke: var(--accent); stroke-width: 2px; }
.node.selected circle { stroke-width: 3px; }
.node text {
  font: 600 var(--text-micro) ui-sans-serif, "Helvetica Neue", Helvetica, Arial, sans-serif;
  fill: var(--ink); user-select: none; pointer-events: none;
}
.link { fill: none; stroke: var(--line-faint); stroke-width: 1.5px; }

.side-pane {
  position: fixed; top: 0; right: 0; bottom: 0; z-index: 10;
  width: min(480px, 92vw); background: var(--paper); border-left: var(--hair);
  padding: calc(6 * var(--unit)) calc(4 * var(--unit)) calc(4 * var(--unit));
  overflow-y: auto;
  transform: translateX(100%); transition: transform 0.18s ease;
}
.side-pane.open { transform: translateX(0); }
#side-pane-close {
  position: absolute; top: calc(2 * var(--unit)); right: calc(2 * var(--unit));
  font: inherit; font-size: 1.5rem; line-height: 1; border: none; background: transparent;
  color: var(--muted); cursor: pointer; padding: calc(1 * var(--unit));
}
"#;

/// This page's own script: fetches `/api/tree`, builds a `d3.hierarchy` via
/// `d3.stratify` (synthesizing an invisible super-root so multiple
/// independent top-level nodes never trip stratify's single-root
/// requirement), lays it out with `d3.tree`, and joins nodes/links into the
/// SVG canvas with a keyed enter/update/exit — status-colored circles,
/// `.text()`-only labels, `d3.linkHorizontal` paths, `d3.zoom` pan/zoom,
/// transitions on every join. Clicking a node fetches and mounts its panel
/// fragment into the side pane; the existing `/sse` stream is the ONLY
/// change signal — on every frame this refetches `/api/tree` and re-joins
/// (d3 handles the diff), and also hands the frame to the shared
/// `applyPatch` (from `shell::CORE_JS`) so an open side-pane panel patches
/// in place exactly like an outline section does.
pub const TREE_JS: &str = r#"
(function () {
  var SYN_ROOT = '__tidepool_root__';
  var selectedId = null;
  var svg, viewport, gLinks, gNodes, zoomBehavior;
  var linkGen = d3.linkHorizontal()
    .x(function (d) { return d.y; })
    .y(function (d) { return d.x; });

  // Every node lacking a resolvable parent (a real tree root, or a
  // registered node with no natural parent at all) becomes a child of one
  // synthetic, never-rendered super-root — d3.stratify requires EXACTLY one
  // null-parent row, and this codebase's node ids don't guarantee that on
  // their own (see server.rs: any caller can register a bare top-level id).
  function stratifyTree(nodes) {
    var ids = new Set(nodes.map(function (n) { return n.id; }));
    var rows = [{ id: SYN_ROOT, parent: null, title: '', status: '', rev: 0 }];
    nodes.forEach(function (n) {
      var parent = (n.parent && ids.has(n.parent)) ? n.parent : SYN_ROOT;
      rows.push(Object.assign({}, n, { parent: parent }));
    });
    return d3.stratify()
      .id(function (d) { return d.id; })
      .parentId(function (d) { return d.parent; })
      (rows);
  }

  function ensureCanvas() {
    if (svg) return;
    svg = d3.select('#tree-canvas');
    viewport = svg.select('g.viewport');
    gLinks = viewport.select('g.links');
    gNodes = viewport.select('g.nodes');
    zoomBehavior = d3.zoom().scaleExtent([0.15, 3]).on('zoom', function (event) {
      viewport.attr('transform', event.transform);
    });
    svg.call(zoomBehavior);
  }

  function renderTree(nodes) {
    ensureCanvas();
    var root = stratifyTree(nodes);
    d3.tree().nodeSize([32, 180])(root);

    var allNodes = root.descendants().filter(function (d) { return d.id !== SYN_ROOT; });
    var allLinks = root.links().filter(function (l) { return l.source.id !== SYN_ROOT; });

    var link = gLinks.selectAll('path.link').data(allLinks, function (l) { return l.target.id; });
    link.exit().transition().duration(200).style('opacity', 0).remove();
    link.enter().append('path')
        .attr('class', 'link')
        .style('opacity', 0)
      .merge(link)
        .transition().duration(300)
        .style('opacity', 1)
        .attr('d', function (l) { return linkGen(l); });

    var node = gNodes.selectAll('g.node').data(allNodes, function (d) { return d.id; });
    node.exit().transition().duration(200).style('opacity', 0).remove();

    var nodeEnter = node.enter().append('g')
      .style('opacity', 0)
      .attr('transform', function (d) { return 'translate(' + d.y + ',' + d.x + ')'; })
      .on('click', function (event, d) { openPanel(d.id); });
    nodeEnter.append('circle').attr('r', 9);
    nodeEnter.append('text').attr('x', 14).attr('dy', '0.32em');

    var merged = nodeEnter.merge(node);
    merged.attr('class', function (d) {
      return 'node status-' + d.data.status + (d.id === selectedId ? ' selected' : '');
    });
    merged.transition().duration(300)
      .style('opacity', 1)
      .attr('transform', function (d) { return 'translate(' + d.y + ',' + d.x + ')'; });
    // .text() only — node titles are substrate identifiers (tree paths), and
    // every other model-authored string this page ever shows lives inside
    // the side-pane panel fragment, itself already maud-escaped server-side.
    merged.select('text').text(function (d) { return d.data.title; });
  }

  function refreshTree() {
    fetch('/api/tree')
      .then(function (r) { return r.json(); })
      .then(renderTree)
      .catch(function (e) { toast('failed to load tree: ' + e.message, true); });
  }

  // Load a node's full panel into the side pane. The fetched HTML is
  // server-rendered, maud-escaped markup (the exact bytes the SSE stream
  // patches) — parsed via a detached <template>, same idiom as
  // shell::CORE_JS's applyPatch, then MOVED into the live pane as a DOM
  // node (never assigned onto the mounted pane itself as raw markup).
  function openPanel(id) {
    fetch('/node/' + encodeURIComponent(id) + '/panel')
      .then(function (r) {
        if (!r.ok) throw new Error(r.status + ' ' + r.statusText);
        return r.text();
      })
      .then(function (html) {
        var tpl = document.createElement('template');
        tpl.innerHTML = html.trim();
        var panel = tpl.content.firstElementChild;
        if (!panel) return;
        var body = document.getElementById('side-pane-body');
        body.replaceChildren(panel);
        wire(body);
        selectedId = id;
        document.getElementById('side-pane').classList.add('open');
        refreshTree();
      })
      .catch(function (e) { toast('failed to load panel: ' + e.message, true); });
  }

  function closePanel() {
    selectedId = null;
    document.getElementById('side-pane').classList.remove('open');
    document.getElementById('side-pane-body').replaceChildren();
    refreshTree();
  }

  // The `/sse` stream is the ONLY change signal here: any tick means SOME
  // node's state changed, so refetch the whole tree and let d3's keyed join
  // work out what actually moved. The frame is also handed to the shared
  // applyPatch — if it's the currently open side-pane panel, this patches it
  // in place exactly like an outline section (same data-rev gate); otherwise
  // applyPatch's mount fallback no-ops (there is no #tree element here).
  function connect() {
    var es = new EventSource('/sse');
    es.onopen = function () { setConn(true); };
    es.addEventListener('datastar-patch-elements', function (ev) {
      var html = ev.data.split('\n')
        .map(function (l) { return l.replace(/^elements /, ''); })
        .join('\n');
      applyPatch(html);
      refreshTree();
    });
    es.onerror = function () { setConn(false); };
  }

  window.addEventListener('DOMContentLoaded', function () {
    var closeBtn = document.getElementById('side-pane-close');
    if (closeBtn) closeBtn.addEventListener('click', closePanel);
    refreshTree();
    connect();
  });
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_page_wires_d3_asset_and_scripts() {
        let doc = tree_page().into_string();
        assert!(doc.contains(&format!("src=\"{D3_ASSET_PATH}\"")), "{doc}");
        assert!(doc.contains("id=\"tree-canvas\""), "{doc}");
        assert!(doc.contains("id=\"side-pane\""), "{doc}");
        assert!(doc.contains("id=\"side-pane-body\""), "{doc}");
        assert!(doc.contains(CORE_JS_MARKER), "{doc}");
        assert!(doc.contains("function renderTree"), "{doc}");
        // No CDN, no external host — d3 is same-origin only.
        assert!(!doc.contains("http://"), "{doc}");
        assert!(!doc.contains("https://"), "{doc}");
        assert!(!doc.contains("cdn"), "{doc}");
    }

    /// A marker unique to `shell::CORE_JS`, used only to prove it's embedded
    /// (rather than importing `shell` into this assertion and risking a
    /// false pass from an unrelated substring).
    const CORE_JS_MARKER: &str = "function applyPatch(html)";

    #[test]
    fn tree_page_embeds_shared_core_js_verbatim() {
        let doc = tree_page().into_string();
        assert!(
            doc.contains(shell::CORE_JS),
            "the tree page must embed shell::CORE_JS verbatim, not a reimplementation: {doc}"
        );
    }

    /// d3 built-ins only — no hand-rolled layout/zoom math anywhere in this
    /// page's own script.
    #[test]
    fn tree_js_uses_d3_builtins_for_layout_join_and_zoom() {
        assert!(TREE_JS.contains("d3.stratify()"), "{TREE_JS}");
        assert!(TREE_JS.contains("d3.tree()"), "{TREE_JS}");
        assert!(TREE_JS.contains("d3.linkHorizontal()"), "{TREE_JS}");
        assert!(TREE_JS.contains("d3.zoom()"), "{TREE_JS}");
        assert!(TREE_JS.contains(".data(allNodes"), "{TREE_JS}");
        assert!(TREE_JS.contains(".data(allLinks"), "{TREE_JS}");
    }

    /// The data join is keyed by node id — a stable key function, not
    /// positional/index-based joining, is what lets d3 diff on every refetch
    /// instead of tearing down and rebuilding the whole canvas.
    #[test]
    fn tree_js_data_join_is_keyed_by_node_id() {
        assert!(
            TREE_JS.contains("function (d) { return d.id; }"),
            "node join must be keyed by id: {TREE_JS}"
        );
        assert!(
            TREE_JS.contains("function (l) { return l.target.id; }"),
            "link join must be keyed by the target node's id: {TREE_JS}"
        );
    }

    /// The boundary rule: no `.html(`/`.innerHTML =` assignment onto an
    /// already-mounted, LIVE DOM node anywhere in this page's own script —
    /// the one HTML-parsing call site (`openPanel`) targets a freshly
    /// created, detached `<template>`, then moves the resulting DOM node
    /// into place, never assigning serialized HTML onto a live element.
    #[test]
    fn tree_js_never_assigns_html_onto_a_live_mounted_node() {
        assert!(!TREE_JS.contains(".html("), "{TREE_JS}");
        // The one `.innerHTML = ` ASSIGNMENT targets a detached `<template>`
        // (`tpl.innerHTML = ...`), never a live/mounted element. Matches only
        // the assignment form (`.innerHTML =`), not the module doc's prose
        // mentions of `.innerHTML` with no trailing `=`.
        let sites: Vec<&str> = TREE_JS
            .match_indices(".innerHTML =")
            .map(|(i, _)| {
                let start = TREE_JS[..i].rfind(['\n', ';', '{']).map_or(0, |p| p + 1);
                TREE_JS[start..i].trim()
            })
            .collect();
        assert!(
            !sites.is_empty(),
            "expected the template-parse idiom: {TREE_JS}"
        );
        for site in sites {
            assert_eq!(
                site, "tpl",
                "`.innerHTML` must only ever be assigned on a detached `tpl`, not a live node: {TREE_JS}"
            );
        }
    }

    /// Node/section labels are set via `.text()`, never markup — the
    /// escaping invariant as it applies to d3: a node's title (a substrate
    /// tree-path id) reaches the DOM as text content only.
    #[test]
    fn tree_js_sets_labels_via_text_only() {
        assert!(
            TREE_JS.contains(".text(function (d) { return d.data.title; })"),
            "{TREE_JS}"
        );
    }

    /// The tree/pane CSS is layered on top of `shell::CSS` (not a
    /// replacement) — the side pane renders the same node-panel markup as
    /// `/legacy` and needs those same rules (`.field`, `.btn`, `.timeline`,
    /// …), so `tree_page()` must embed both.
    #[test]
    fn tree_page_embeds_shell_css_and_tree_css() {
        let doc = tree_page().into_string();
        assert!(doc.contains(shell::CSS), "{doc}");
        assert!(doc.contains(TREE_CSS), "{doc}");
    }
}
