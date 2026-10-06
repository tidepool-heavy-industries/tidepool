#!/usr/bin/env python3
"""Validate the native boot-library settings and their whole-argument transport."""

from __future__ import annotations

import argparse
from collections import Counter
import json
from pathlib import Path
import re
import sys
import unittest


class SettingsError(ValueError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SettingsError(message)


def validate(packages_source: str, plan: dict, arguments: list[str]) -> list[str]:
    """Check names against pinned Hadrian and each setting against real argv.

    Hadrian owns parsing and builder selection. This guard reads its explicit
    package declarations; an unsupported source layout fails closed rather than
    treating a missing/renamed package as a silently unmatched setting.
    """
    require(isinstance(plan, dict) and set(plan) == {"packages", "options"}, "invalid boot settings plan")
    packages, options = plan["packages"], plan["options"]
    require(
        isinstance(packages, list) and bool(packages)
        and all(isinstance(p, str) and re.fullmatch(r"[A-Za-z0-9-]+", p) for p in packages),
        "invalid boot package names",
    )
    require(len(packages) == len(set(packages)), "duplicate boot package")
    require(
        isinstance(options, list) and bool(options)
        and all(isinstance(o, str) and o.startswith("-") and not re.search(r"\s", o) for o in options),
        "invalid boot compiler options",
    )
    roster = re.search(r"^ghcPackages\s*=\s*\[(.*?)\]", packages_source, re.MULTILINE | re.DOTALL)
    require(roster is not None, "unsupported Hadrian ghcPackages declaration")
    members = roster.group(1)
    require(
        re.fullmatch(r"\s*[A-Za-z][A-Za-z0-9]*(?:\s*,\s*[A-Za-z][A-Za-z0-9]*)*\s*", members) is not None,
        "unsupported Hadrian ghcPackages members",
    )
    identifiers = set(re.findall(r"[A-Za-z][A-Za-z0-9]*", members))
    declarations = re.findall(r'^([A-Za-z][A-Za-z0-9]*)\s*=\s*lib\s+"([^"]+)"', packages_source, re.MULTILINE)
    known = {name for identifier, name in declarations if identifier in identifiers}
    missing = set(packages) - known
    require(not missing, f"unknown Hadrian library packages: {sorted(missing)}")
    expected = [f"*.{package}.ghc.hs.opts+={' '.join(options)}" for package in packages]
    counts = Counter(arguments)
    for setting in expected:
        require(counts[setting] == 1, f"setting must be one whole argument exactly once: {setting!r}")
    # Refuse a second conflicting setting for the selected package scope. Other
    # inherited Hadrian arguments and settings retain their own owner.
    selected_prefixes = tuple(f"*.{package}.ghc.hs.opts" for package in packages)
    selected = [argument for argument in arguments if argument.startswith(selected_prefixes)]
    require(Counter(selected) == Counter(expected), "conflicting native boot settings")
    return expected


class SettingsTests(unittest.TestCase):
    source = '''ghcPackages = [ base, cabalSyntax ]
base = lib "base"
cabalSyntax = lib "Cabal-syntax" `setPath` "libraries/Cabal/Cabal-syntax"
unused = lib "unused"
'''
    plan = {"packages": ["base", "Cabal-syntax"], "options": ["-fwrite-if-simplified-core", "-fexpose-all-unfoldings"]}
    arguments = [
        "*.base.ghc.hs.opts+=-fwrite-if-simplified-core -fexpose-all-unfoldings",
        "*.Cabal-syntax.ghc.hs.opts+=-fwrite-if-simplified-core -fexpose-all-unfoldings",
    ]

    def test_complete_arguments_preserve_inherited_settings(self):
        self.assertEqual(validate(self.source, self.plan, ["-j2", "stage1.*.ghc.link.opts+=-optl-pthread", *self.arguments]), self.arguments)

    def test_split_argument_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, self.plan, [*self.arguments[0].split(), self.arguments[1]])

    def test_missing_argument_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, self.plan, self.arguments[:1])

    def test_duplicate_argument_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, self.plan, [*self.arguments, self.arguments[0]])

    def test_conflicting_option_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, self.plan, [*self.arguments, "*.base.ghc.hs.opts=-O0"])

    def test_unknown_package_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, {**self.plan, "packages": ["missing"]}, [])

    def test_unregistered_definition_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, {**self.plan, "packages": ["unused"]}, [])

    def test_changed_primary_layout_refuses(self):
        with self.assertRaises(SettingsError):
            validate("ghcPackages = makePackages", self.plan, self.arguments)

    def test_comment_is_not_package_membership(self):
        source = self.source.replace("[ base, cabalSyntax ]", "[ base -- cabalSyntax\n ]")
        with self.assertRaises(SettingsError):
            validate(source, self.plan, self.arguments)

    def test_duplicate_package_refuses(self):
        with self.assertRaises(SettingsError):
            validate(self.source, {**self.plan, "packages": ["base", "base"]}, self.arguments)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--plan")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(SettingsTests)
        return 0 if unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful() else 1
    if args.source is None or args.plan is None:
        parser.error("--source and --plan are required")
    arguments = args.arguments
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    try:
        source = (args.source / "hadrian/src/Packages.hs").read_text()
        settings = validate(source, json.loads(args.plan), arguments)
    except (SettingsError, OSError, json.JSONDecodeError) as error:
        print(f"native boot settings refused: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"native_boot_settings": settings}, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
