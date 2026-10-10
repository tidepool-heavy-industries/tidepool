"""Resolve Cargo feature syntax for the native Buck generator slices."""

import hashlib
from pathlib import Path
import re
import tomllib


CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"


def supports_native_facade_build_dependency(package_name, dependency):
    """Admit only the dependencies of the declared facade build-script action.

    Admission does not replace resolution of the exact Cargo kind, target and
    source identity by each graph projection.
    """
    return (
        package_name == "tidepool"
        and dependency["kind"] == "build"
        and dependency["name"] in {"tidepool-toolchain", "serde", "serde_json"}
        and dependency["rename"] is None
        and dependency["target"] is None
    )


def parse_locked_git_source(source):
    """Accept only HTTPS git sources pinned by a full Cargo.lock object id."""
    if not isinstance(source, str) or not source.startswith("git+https://"):
        raise ValueError(f"unsupported Cargo dependency source {source!r}")
    value = source[4:]
    before_fragment, marker, fragment = value.partition("#")
    repository, separator, query = before_fragment.partition("?")
    if not repository.startswith("https://"):
        raise ValueError(f"unsupported Cargo git repository URL {repository!r}")
    revision = None
    if separator:
        if not query.startswith("rev=") or "&" in query:
            raise ValueError(f"Cargo git source must use only a locked rev: {source!r}")
        revision = query.removeprefix("rev=")
    if marker:
        if revision is not None and revision != fragment:
            raise ValueError(f"Cargo git rev differs from locked source commit: {source!r}")
        revision = fragment
    if revision is None or re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", revision) is None:
        raise ValueError(f"Cargo git source lacks a full locked object id: {source!r}")
    return repository, revision.lower()


def dependency_aliases(identities):
    """Choose stable manifest aliases without merging distinct source identities."""
    by_name = {}
    for name, version, source in identities:
        by_name.setdefault(name, []).append((name, version, source))
    aliases = {}
    for name, items in by_name.items():
        unique = sorted(set(items))
        if len(unique) == 1:
            aliases[unique[0]] = name
            continue
        same_versions = {}
        for identity in unique:
            same_versions.setdefault(identity[1], []).append(identity)
        for identity in unique:
            _, version, source = identity
            alias = name + "-" + version.replace(".", "_")
            if len(same_versions[version]) > 1:
                suffix = hashlib.sha256((source or "local").encode()).hexdigest()[:10]
                alias += "-" + suffix
            if alias in aliases.values():
                raise ValueError(f"ambiguous generated Cargo dependency alias {alias!r}")
            aliases[identity] = alias
    return aliases


class FeatureSelectionError(ValueError):
    pass


PROFILE_NAME = "embedded-native"

# These workspace roots have native library, binary and test owners. The graph
# generators share this roster so a new development dependency cannot disappear
# from the Reindeer inputs while remaining present in a first-party test target.
NATIVE_PACKAGES = frozenset({
    "tidepool-atomic-write", "tidepool-repr", "tidepool-heap", "tidepool-bignum",
    "tidepool-bridge", "tidepool-effect", "tidepool-codegen", "tidepool-extract-cmd",
    "tidepool-extract-report", "tidepool-toolchain", "tidepool-bridge-derive",
    "tidepool-runtime", "tidepool-mcp", "tidepool-handlers", "tidepool",
    "exomonad-model", "exomonad-tool", "tidepool-bridge-effects",
    "exomonad-node", "exomonad-worktree", "exomonad-actor", "jev-integration",
    "tidepool-testing", "tidepool-test-data", "tidepool-prepared-corpus", "tidepool-protocol",
})


def reject_local_feature_requests(metadata, root_names, no_default=(), overrides=None):
    """Require one first-party configuration instead of workspace feature union.

    Empty default markers are harmless. Every other local feature request needs
    an explicit native target variant; development features cannot silently
    change the production library used by another root.
    """
    packages = {package["id"]: package for package in metadata["packages"]}
    members = set(metadata["workspace_members"])
    local = {package["name"]: package for package in packages.values()
             if package["id"] in members and package.get("source") is None}
    overrides = overrides or {}
    for root_name in sorted(root_names):
        root = local[root_name]
        pending = [(root, root_name, set(overrides.get(root_name, ())),
                    root_name not in no_default)]
        visited = set()
        while pending:
            package, edge, requested, defaults = pending.pop()
            definitions = package.get("features", {})
            if defaults and "default" in definitions:
                requested.add("default")
            unsupported = {feature for feature in requested
                           if feature != "default" or definitions.get("default")}
            if unsupported:
                raise FeatureSelectionError(
                    f"unsupported local Cargo feature(s) {', '.join(sorted(unsupported))} "
                    f"for package {package['name']}; native root {root_name}; edge {edge}; "
                    "declare a native target variant rather than unifying development features"
                )
            if package["id"] in visited:
                continue
            visited.add(package["id"])
            # Only roots own development edges. Local libraries reached through
            # them contribute their normal/build dependencies, never their tests.
            for dependency in package["dependencies"]:
                if dependency["kind"] == "dev" and package["name"] != root_name:
                    continue
                if dependency.get("optional", False):
                    continue
                if dependency["target"] not in (None, "cfg(unix)"):
                    continue
                target = local.get(dependency["name"])
                if target is None:
                    continue
                alias = dependency_key(dependency)
                kind = dependency["kind"] or "normal"
                pending.append((target,
                    f"{package['name']} --{kind}:{alias}--> {target['name']}",
                    set(dependency.get("features", ())),
                    dependency.get("uses_default_features", True)))



def load_profile(root):
    """Load the checked-in feature selection shared by the Buck generators."""
    path = Path(root) / "scripts/native-profile.toml"
    try:
        profile = tomllib.loads(path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise FeatureSelectionError(f"cannot load native Cargo profile {path}: {error}") from error
    if profile.get("profile") != PROFILE_NAME:
        raise FeatureSelectionError(f"unsupported native Cargo profile in {path}")
    packages = profile.get("no-default-features")
    if not isinstance(packages, list) or not all(isinstance(item, str) for item in packages):
        raise FeatureSelectionError(f"invalid no-default-features list in {path}")
    return frozenset(packages)


def effective_feature_selection(root, workspace_packages, no_default_features, feature_overrides):
    """Combine the checked-in profile with explicit generator overrides."""
    profile_defaults = load_profile(root)
    no_default = set(no_default_features) | set(profile_defaults)
    unknown = (no_default | set(feature_overrides)) - set(workspace_packages)
    if unknown:
        raise FeatureSelectionError(
            "feature selection names an unknown workspace package: " + ", ".join(sorted(unknown))
        )
    return no_default, feature_overrides


def reject_forbidden_closure(metadata, root_names):
    """Reject activated Codex packages anywhere below selected Cargo roots."""
    packages = {package["id"]: package for package in metadata["packages"]}
    workspace_ids = set(metadata.get("workspace_members", []))
    by_name = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in workspace_ids and package.get("source") is None
    }
    nodes = {node["id"]: node for node in metadata.get("resolve", {}).get("nodes", [])}
    roots = [by_name[name] for name in root_names if name in by_name]
    pending = [package["id"] for package in roots]
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        package = packages.get(package_id)
        if package is None:
            raise FeatureSelectionError(f"Cargo resolve references missing package {package_id}")
        manifest = package.get("manifest_path", "").replace("\\", "/")
        name = package["name"].lower().replace("_", "-")
        codex_package = name.startswith("codex-") or "/vendor/codex/" in manifest
        codex_target = any(
            target.get("name", "").lower().replace("-", "_").startswith("codex_")
            for target in package.get("targets", [])
        )
        if codex_package or codex_target:
            raise FeatureSelectionError(
                f"embedded-native Cargo closure reaches forbidden Codex package {package['name']}"
            )
        node = nodes.get(package_id)
        if node is None:
            raise FeatureSelectionError(f"Cargo resolve is missing node for {package['name']}")
        pending.extend(edge["pkg"] for edge in node.get("deps", []))


def dependency_key(dependency):
    return dependency["rename"] or dependency["name"]


def metadata_feature_args(metadata, no_default_features=(), feature_overrides=None):
    """Return Cargo CLI feature flags preserving workspace defaults exactly."""
    no_default = set(no_default_features)
    overrides = feature_overrides or {}
    members = set(metadata["workspace_members"])
    flags = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        enabled = set(overrides.get(package["name"], ()))
        if package["name"] not in no_default and "default" in package.get("features", {}):
            enabled.add("default")
        for feature in sorted(enabled):
            flags.extend(["--features", f"{package['name']}/{feature}"])
    return flags


def resolve(package, selected_features=(), enable_defaults=True, resolved_node=None):
    """Return (rust_cfg_features, active_dependency_keys, forwarded_features)."""
    definitions = package.get("features", {})
    requested = set(selected_features)
    if enable_defaults and "default" in definitions:
        requested.add("default")
    unknown = requested - set(definitions)
    if unknown:
        raise FeatureSelectionError(
            f"unknown Cargo feature(s) for {package['name']}: {', '.join(sorted(unknown))}"
        )

    dependencies = package["dependencies"]
    aliases = {}
    for dependency in dependencies:
        alias = dependency_key(dependency)
        entries = aliases.setdefault(alias, [])
        entries.append(dependency)
    for alias, entries in aliases.items():
        identities = {
            (entry["name"], entry.get("optional", False), entry.get("target"))
            for entry in entries
        }
        if len(identities) != 1:
            raise FeatureSelectionError(
                f"ambiguous Cargo dependency alias {alias!r} in {package['name']}"
            )
        aliases[alias] = entries[0]

    hidden_implicit = {
        member[4:]
        for feature, members in definitions.items()
        for member in members
        if member.startswith("dep:")
        and feature != member[4:]
    }

    def lookup(alias):
        dependency = aliases.get(alias)
        if dependency is None:
            raise FeatureSelectionError(
                f"unknown dependency alias {alias!r} in {package['name']}"
            )
        return dependency

    active_features = set()
    active_optional = set()
    forwarded = {}
    weak_forwarded = {}
    pending = list(requested)
    while pending:
        feature = pending.pop()
        if feature in active_features:
            continue
        active_features.add(feature)
        for member in definitions.get(feature, []):
            if member.startswith("dep:"):
                alias = member[4:]
                dependency = lookup(alias)
                if not dependency.get("optional", False):
                    raise FeatureSelectionError(
                        f"unknown optional dependency feature {member!r} in {package['name']}"
                    )
                active_optional.add(dependency_key(dependency))
                continue

            if "/" in member:
                alias, dependency_feature = member.split("/", 1)
                weak = alias.endswith("?")
                alias = alias.removesuffix("?")
                dependency = lookup(alias)
                if not weak:
                    if dependency.get("optional", False):
                        key = dependency_key(dependency)
                        active_optional.add(key)
                        if key in definitions and key not in hidden_implicit:
                            active_features.add(key)
                    forwarded.setdefault(dependency_key(dependency), set()).add(dependency_feature)
                else:
                    weak_forwarded.setdefault(dependency_key(dependency), set()).add(dependency_feature)
                continue

            if member in definitions:
                pending.append(member)
                continue

            dependency = lookup(member)
            if not dependency.get("optional", False):
                raise FeatureSelectionError(
                    f"unknown Cargo feature member {member!r} in {package['name']}"
                )
            if dependency_key(dependency) in hidden_implicit:
                raise FeatureSelectionError(
                    f"implicit optional dependency feature {member!r} is disabled in {package['name']}"
                )
            active_optional.add(dependency_key(dependency))
            active_features.add(member)

    active_dependencies = {
        dependency_key(dependency) for dependency in aliases.values()
        if dependency.get("kind") in (None, "normal")
        and not dependency.get("optional", False)
    } | active_optional
    if resolved_node is not None:
        # Cargo's resolve node reports the exact feature set, while its `deps`
        # field can still contain weakly referenced optional edges. Keep
        # activation alias-aware from the Cargo feature expressions above and
        # use the graph as the authority for cfg(feature) values.
        if "features" in resolved_node:
            normalized = {}
            for alias in aliases:
                key = alias.replace("-", "_")
                if key in normalized and normalized[key] != alias:
                    raise FeatureSelectionError(
                        f"ambiguous Cargo dependency aliases {normalized[key]!r} and {alias!r} "
                        f"in {package['name']}"
                    )
                normalized[key] = alias
            graph_aliases = set()
            for edge in resolved_node.get("deps", []):
                if any(
                    dep_kind["target"] in (None, "cfg(unix)")
                    for dep_kind in edge["dep_kinds"]
                ):
                    edge_alias = edge["name"].replace("-", "_")
                    if edge_alias not in normalized:
                        raise FeatureSelectionError(
                            f"Cargo resolved unknown dependency alias {edge['name']!r} "
                            f"in {package['name']}"
                        )
                    graph_aliases.add(normalized[edge_alias])
            active_features = set(resolved_node["features"])
            missing_edges = active_dependencies - graph_aliases
            if missing_edges:
                raise FeatureSelectionError(
                    f"Cargo resolved missing dependency alias(es) in {package['name']}: "
                    + ", ".join(sorted(missing_edges))
                )
    for alias in active_dependencies:
        if alias in weak_forwarded:
            forwarded.setdefault(alias, set()).update(weak_forwarded[alias])
    return sorted(active_features), active_dependencies, forwarded
