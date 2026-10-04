"""Native compiler profile policy and installed Nix package flag rendering."""

load("//build:native_profile.bzl", "haskell_optimization_flags")

def _package_flags(packages):
    flags = []
    for package in packages:
        flags.extend(["-package", package])
    return flags

def haskell_component_flags(packages, flags = []):
    return haskell_optimization_flags() + _package_flags(packages) + flags

def haskell_component_link_flags(packages, flags = [], dynamic = True):
    return (["-dynamic"] if dynamic else []) + _package_flags(packages) + flags
