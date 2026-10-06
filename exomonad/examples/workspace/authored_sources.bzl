"""Package-owned projections for Cabal-declared authored modules."""
load("@prelude//:rules.bzl", "export_file")

def declare_authored_haskell_sources(sources):
    for source in sources:
        relative = source[len(".exomonad/"):]
        export_file(
            name = "authored_haskell_" + relative.replace("/", "_").replace(".", "_").replace("-", "_"),
            src = source,
            out = relative.split("/")[-1],
            visibility = ["PUBLIC"],
        )
