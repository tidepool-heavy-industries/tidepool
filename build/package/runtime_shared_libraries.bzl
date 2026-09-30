load(
    "@prelude//linking:shared_libraries.bzl",
    "SharedLibraryInfo",
    "create_shlib_symlink_tree",
    "traverse_shared_library_info",
)

def _tidepool_runtime_shared_libraries_impl(ctx):
    libraries = traverse_shared_library_info(
        ctx.attrs.library[SharedLibraryInfo],
        transformation_provider = None,
    )
    if not libraries:
        fail("{} has no runtime shared libraries".format(ctx.attrs.library.label))

    tree = create_shlib_symlink_tree(
        actions = ctx.actions,
        out = "runtime_shared_libraries",
        shared_libs = libraries,
    )
    return [DefaultInfo(default_output = tree)]

tidepool_runtime_shared_libraries = rule(
    impl = _tidepool_runtime_shared_libraries_impl,
    attrs = {
        "library": attrs.dep(providers = [SharedLibraryInfo]),
    },
)
