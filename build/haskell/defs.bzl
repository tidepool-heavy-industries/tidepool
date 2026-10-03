"""Pinned GHC options and package environments for native Haskell targets."""

load("//build:native_profile.bzl", "haskell_optimization_flags")

GHC_COMPONENT_FLAGS = [
    "-Wall",
    "-XGHC2024",
]

# The Prelude compile action exposes base automatically. These lists capture
# each Cabal component's other direct package dependencies.
EXTRACTOR_LIBRARY_PACKAGES = [
    "ghc",
    "ghc-boot",
    "bytestring",
    "containers",
    "deepseq",
    "filepath",
    "directory",
    "process",
    "template-haskell",
    "time",
    "text",
    "cborg",
    "cryptohash-sha256",
    "unix",
    "mtl",
    "syb",
    "lens",
    "errors",
    "witherable",
    "safe",
]

EXTRACTOR_BINARY_PACKAGES = [
    "ghc",
    "bytestring",
    "cborg",
    "containers",
    "cryptohash-sha256",
    "filepath",
    "directory",
    "text",
]

ENCODER_TEST_PACKAGES = [
    "bytestring",
    "cborg",
    "directory",
    "text",
]

WORKER_RESPONSE_TEST_PACKAGES = ["bytestring", "directory", "process", "unix"]

def _package_flags(packages):
    flags = []
    for package in packages:
        flags.extend(["-package", package])
    return flags

def _component_flags(packages, extra):
    return GHC_COMPONENT_FLAGS + haskell_optimization_flags() + _package_flags(packages) + list(extra)

def extractor_library_flags(*extra):
    return _component_flags(EXTRACTOR_LIBRARY_PACKAGES, extra)

def extractor_binary_flags(*extra):
    return _component_flags(EXTRACTOR_BINARY_PACKAGES, extra)

def encoder_test_flags(*extra):
    return _component_flags(ENCODER_TEST_PACKAGES, extra)

def worker_response_test_flags(*extra):
    return _component_flags(WORKER_RESPONSE_TEST_PACKAGES, extra)

def assignment_component_flags(*extra):
    return _component_flags(["text"], extra)

# Nix owns these installed package closures; GHC must select them at link time
# as well as during typechecking. Native project components remain Buck deps.
def extractor_library_link_flags():
    return _package_flags(EXTRACTOR_LIBRARY_PACKAGES)

def extractor_binary_link_flags():
    return ["-dynamic"] + _package_flags(EXTRACTOR_BINARY_PACKAGES)

def encoder_test_link_flags():
    return ["-dynamic"] + _package_flags(ENCODER_TEST_PACKAGES)

def worker_response_test_link_flags():
    return ["-dynamic"] + _package_flags(WORKER_RESPONSE_TEST_PACKAGES)
