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
                        button type="button" id="fit-reset" class="fit-reset" { "fit" }
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
.fit-reset {
  font: inherit; font-size: var(--text-micro); font-weight: 600; text-transform: uppercase;
  letter-spacing: var(--tracking-wide); color: var(--muted);
  border: var(--hair-faint); background: transparent; cursor: pointer;
  padding: calc(0.5 * var(--unit)) calc(1.5 * var(--unit));
}
.fit-reset:hover { color: var(--ink); border-color: var(--line); }
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
  var NODE_RADIUS = 9;
  var LABEL_OFFSET = 14;
  // Rough advance width for the 600-weight, text-micro (11px) label font —
  // used only to estimate a label's on-screen extent for fitToContent's
  // bounding box, never for layout itself (that stays d3's job).
  var CHAR_WIDTH_PX = 6.5;
  var MIN_ZOOM = 0.15, MAX_ZOOM = 3;
  var FIT_PADDING = 48;

  var selectedId = null;
  var svg, viewport, gLinks, gNodes, zoomBehavior;
  var allNodes = [];
  // Set the moment a REAL pan/zoom gesture fires (see ensureCanvas's zoom
  // handler) — once true, fitToContent never runs again on its own; only the
  // #fit-reset control (which clears this) brings it back. Never fought: an
  // operator's deliberate pan/zoom is never auto-overridden.
  var userInteracted = false;
  // The node count fitToContent last ran against — renderTree re-fits only
  // when the tree has GROWN past this (nodes streaming in via SSE), never on
  // every tick, so an unchanged tree never yanks the view.
  var fittedCount = 0;

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
    var rows = [{ id: SYN_ROOT, parent: null, title: '', label: '', status: '', rev: 0 }];
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
    zoomBehavior = d3.zoom().scaleExtent([MIN_ZOOM, MAX_ZOOM]).on('zoom', function (event) {
      viewport.attr('transform', event.transform);
      // A real pan/zoom/wheel/touch gesture always carries the triggering
      // DOM event as sourceEvent; fitToContent's own programmatic
      // `.call(zoomBehavior.transform, ...)` never does — this is the
      // standard d3 idiom for telling an operator's own input apart from an
      // auto-fit, and the ONLY thing that latches userInteracted.
      if (event.sourceEvent) userInteracted = true;
    });
    svg.call(zoomBehavior);
    var fitBtn = document.getElementById('fit-reset');
    if (fitBtn) fitBtn.addEventListener('click', function () {
      userInteracted = false;
      fitToContent();
    });
  }

  // The bounding box of every node's rendered extent — circle plus its
  // last-segment label, estimated from character count — in SCREEN space:
  // d.y is the horizontal/depth axis, d.x the vertical/sibling axis (see
  // linkGen and every node's transform below, which swap x/y to lay the
  // tree out left-to-right).
  function computeBounds(nodes) {
    var minSx = Infinity, maxSx = -Infinity, minSy = Infinity, maxSy = -Infinity;
    nodes.forEach(function (d) {
      var labelChars = (d.data.label || '').length;
      var rightEdge = d.y + NODE_RADIUS + LABEL_OFFSET + labelChars * CHAR_WIDTH_PX;
      minSx = Math.min(minSx, d.y - NODE_RADIUS);
      maxSx = Math.max(maxSx, rightEdge);
      minSy = Math.min(minSy, d.x - NODE_RADIUS);
      maxSy = Math.max(maxSy, d.x + NODE_RADIUS);
    });
    return { minSx: minSx, maxSx: maxSx, minSy: minSy, maxSy: maxSy };
  }

  // Set the zoom transform so every node (plus its label) fits the viewport
  // with padding — the "where's the root" paper cut this fixes on first
  // render, and what the #fit-reset control re-runs on demand. Never called
  // from renderTree once the operator has taken deliberate control of the
  // view (see the userInteracted gate at that one call site); the reset
  // control is the only path back, and it clears the flag itself first.
  function fitToContent() {
    if (!svg || !allNodes.length) return;
    var b = computeBounds(allNodes);
    var rect = svg.node().getBoundingClientRect();
    var w = rect.width || 800, h = rect.height || 600;
    var treeW = Math.max(1, b.maxSx - b.minSx);
    var treeH = Math.max(1, b.maxSy - b.minSy);
    var scale = Math.min((w - FIT_PADDING * 2) / treeW, (h - FIT_PADDING * 2) / treeH, MAX_ZOOM);
    scale = Math.max(scale, MIN_ZOOM);
    var tx = FIT_PADDING - b.minSx * scale + Math.max(0, (w - FIT_PADDING * 2 - treeW * scale) / 2);
    var ty = h / 2 - ((b.minSy + b.maxSy) / 2) * scale;
    svg.transition().duration(300)
      .call(zoomBehavior.transform, d3.zoomIdentity.translate(tx, ty).scale(scale));
    fittedCount = allNodes.length;
  }

  function renderTree(nodes) {
    ensureCanvas();
    var root = stratifyTree(nodes);
    d3.tree().nodeSize([36, 200])(root);

    allNodes = root.descendants().filter(function (d) { return d.id !== SYN_ROOT; });
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
    nodeEnter.append('circle').attr('r', NODE_RADIUS);
    nodeEnter.append('text').attr('x', LABEL_OFFSET).attr('dy', '0.32em');
    // The native browser hover tooltip — the full path stays reachable here
    // even though the drawn label is only the last segment.
    nodeEnter.append('title');

    var merged = nodeEnter.merge(node);
    merged.attr('class', function (d) {
      return 'node status-' + d.data.status + (d.id === selectedId ? ' selected' : '');
    });
    merged.transition().duration(300)
      .style('opacity', 1)
      .attr('transform', function (d) { return 'translate(' + d.y + ',' + d.x + ')'; });
    // .text() only — node labels/paths are substrate identifiers (tree
    // paths), and every other model-authored string this page ever shows
    // lives inside the side-pane panel fragment, itself already
    // maud-escaped server-side. The drawn label is the node's OWN last path
    // segment (short, truncated server-side to fit the depth column) —
    // never the full slash path, which is what used to print straight
    // through sibling/child labels; the full path stays reachable via this
    // <title> hover tooltip and the (unchanged) side panel.
    merged.select('text').text(function (d) { return d.data.label; });
    merged.select('title').text(function (d) { return d.data.path; });

    // Fit on first render, and again whenever the tree has grown past what
    // was last fitted (nodes streaming in via SSE) — but never once the
    // operator has taken deliberate control of the view. The #fit-reset
    // control is the only way back after that.
    if (!userInteracted && allNodes.length > fittedCount) {
      fitToContent();
    }
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
    /// escaping invariant as it applies to d3: a node's drawn label reaches
    /// the DOM as text content only.
    #[test]
    fn tree_js_sets_labels_via_text_only() {
        assert!(
            TREE_JS.contains(".text(function (d) { return d.data.label; })"),
            "{TREE_JS}"
        );
    }

    /// The drawn label is the tree's short `label` field (last path segment,
    /// truncated — see `render::tree_label`), NEVER the full `title`/`path` —
    /// the paper cut this fixes (a parent's full-path label printing
    /// straight through its children's) regresses if this ever points back
    /// at the full id.
    #[test]
    fn tree_js_labels_use_the_short_label_field_not_the_full_path() {
        assert!(
            !TREE_JS.contains(".data.title"),
            "the tree view must not draw the full-path title as a label: {TREE_JS}"
        );
        // `.data.path` (the full, untruncated id) may feed exactly one
        // thing: the hover `<title>` tooltip — never the drawn node label.
        assert_eq!(
            TREE_JS.matches(".data.path").count(),
            1,
            "the full path must feed only the hover tooltip: {TREE_JS}"
        );
    }

    /// Full path stays reachable on hover: each node mounts a native SVG
    /// `<title>` element (browser tooltip), set from `d.data.path` — the
    /// untruncated id.
    #[test]
    fn tree_js_hover_tooltip_carries_the_full_path() {
        assert!(TREE_JS.contains("nodeEnter.append('title')"), "{TREE_JS}");
        assert!(
            TREE_JS.contains("merged.select('title').text(function (d) { return d.data.path; })"),
            "{TREE_JS}"
        );
    }

    /// Fit-to-content: a `fitToContent` routine exists, is driven by a
    /// computed bounding box (not a fixed/guessed transform), and is called
    /// from `renderTree` — i.e. on every fresh `/api/tree` fetch, which
    /// includes the very first page load.
    #[test]
    fn tree_js_defines_fit_to_content_driven_by_computed_bounds() {
        assert!(TREE_JS.contains("function fitToContent()"), "{TREE_JS}");
        assert!(TREE_JS.contains("function computeBounds("), "{TREE_JS}");
        assert!(
            TREE_JS.contains("zoomBehavior.transform, d3.zoomIdentity.translate("),
            "fitToContent must set the transform via d3.zoom's own API: {TREE_JS}"
        );
        assert!(
            TREE_JS.contains(
                "if (!userInteracted && allNodes.length > fittedCount) {\n      fitToContent();"
            ),
            "renderTree must fit on first render / growth: {TREE_JS}"
        );
    }

    /// A real operator pan/zoom gesture (one carrying a DOM `sourceEvent`)
    /// latches `userInteracted`, which gates every auto-fit call — the
    /// "never fight a deliberate pan/zoom" requirement, pinned at the
    /// mechanism level rather than by behavior we can't execute here.
    #[test]
    fn tree_js_suppresses_autofit_after_a_real_interaction() {
        assert!(
            TREE_JS.contains("if (event.sourceEvent) userInteracted = true;"),
            "only a real gesture (carrying sourceEvent) may set userInteracted: {TREE_JS}"
        );
        assert!(
            TREE_JS.contains("if (!userInteracted &&"),
            "every auto-fit site must be gated on userInteracted: {TREE_JS}"
        );
    }

    /// The `#fit-reset` control resets `userInteracted` and re-fits — the
    /// operator's way back after taking manual control of the view.
    #[test]
    fn tree_js_wires_the_fit_reset_control() {
        assert!(
            TREE_JS.contains("document.getElementById('fit-reset')"),
            "{TREE_JS}"
        );
        assert!(
            TREE_JS.contains("userInteracted = false;\n      fitToContent();"),
            "the reset control must clear userInteracted before re-fitting: {TREE_JS}"
        );
    }

    /// `tree_page()` renders the fit/reset affordance the operator asked
    /// for, in the masthead alongside the outline-view link.
    #[test]
    fn tree_page_has_a_fit_reset_button() {
        let doc = tree_page().into_string();
        assert!(doc.contains("id=\"fit-reset\""), "{doc}");
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
