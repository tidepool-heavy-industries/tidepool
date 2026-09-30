"""Resolve Cargo feature syntax for the native Buck generator slices."""


class FeatureSelectionError(ValueError):
    pass


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
