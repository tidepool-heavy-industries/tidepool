"""Project declared Harness browser inputs and run their owning identity issuer."""
import ast
import pathlib
import shutil
import subprocess
import sys

source, output = map(pathlib.Path, sys.argv[1:])
output.mkdir(parents=True, exist_ok=True)
issuer = source / "scripts/browser-source-identity.py"


def declarations(relative):
    tree = ast.parse((source / relative).read_text(), filename=relative)
    assignments = {
        statement.targets[0].id: statement.value
        for statement in tree.body
        if isinstance(statement, ast.Assign)
        and len(statement.targets) == 1
        and isinstance(statement.targets[0], ast.Name)
    }
    return tree, assignments


def selected_paths(expression, assignments, root, resolving=()):
    if isinstance(expression, ast.Name):
        if expression.id in resolving:
            raise ValueError("cyclic Harness browser source declaration")
        return selected_paths(assignments[expression.id], assignments, root, (*resolving, expression.id))
    if isinstance(expression, ast.BinOp) and isinstance(expression.op, ast.Add):
        return selected_paths(expression.left, assignments, root, resolving) | selected_paths(expression.right, assignments, root, resolving)
    if isinstance(expression, ast.List):
        return set(ast.literal_eval(expression))
    if isinstance(expression, ast.Call) and isinstance(expression.func, ast.Name) and expression.func.id == "glob":
        if len(expression.args) != 1 or any(keyword.arg != "exclude" for keyword in expression.keywords):
            raise ValueError("unsupported Harness browser glob declaration")
        patterns = ast.literal_eval(expression.args[0])
        excluded = ast.literal_eval(expression.keywords[0].value) if expression.keywords else []
        included_paths = {path.relative_to(root).as_posix() for pattern in patterns for path in root.glob(pattern) if path.is_file()}
        excluded_paths = {path.relative_to(root).as_posix() for pattern in excluded for path in root.glob(pattern) if path.is_file()}
        return included_paths - excluded_paths
    raise ValueError("unsupported Harness browser source declaration")


def copy(relative, root, target):
    destination = root / target
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source / relative, destination)


_, web_declarations = declarations("web/BUCK")
web = output / "web-inputs"
for relative in sorted(selected_paths(web_declarations["BROWSER_IDENTITY_SRCS"], web_declarations, source / "web")):
    copy("web/" + relative, web, relative)
web_identity = output / "web-source-identity.json"
subprocess.run([sys.executable, str(issuer), str(web), str(web_identity)], check=True)

runtime_tree, runtime_declarations = declarations("crates/harness/BUCK")
runtime = output / "runtime-inputs"
source_map = runtime_declarations["BROWSER_SOURCE_INPUTS"]
if not isinstance(source_map, ast.DictComp) or ast.unparse(source_map) != "{source: source for source in BROWSER_RUNTIME_SRCS}":
    raise ValueError("unsupported Harness runtime source mapping")
for relative in sorted(selected_paths(runtime_declarations["BROWSER_RUNTIME_SRCS"], runtime_declarations, source / "crates/harness")):
    copy("crates/harness/" + relative, runtime, relative)

labels = {
    "//:workspace_cargo_toml": "Cargo.toml",
    "//:workspace_cargo_lock": "Cargo.lock",
    "//:workspace_flake": "flake.nix",
    "//:workspace_flake_lock": "flake.lock",
    "//:workspace_buckconfig": ".buckconfig",
    "//scripts:browser_source_identity_issuer": "scripts/browser-source-identity.py",
}
updates = [
    statement.value
    for statement in runtime_tree.body
    if isinstance(statement, ast.Expr) and isinstance(statement.value, ast.Call)
    and isinstance(statement.value.func, ast.Attribute)
    and isinstance(statement.value.func.value, ast.Name)
    and statement.value.func.value.id == "BROWSER_SOURCE_INPUTS"
]
if len(updates) != 1 or updates[0].func.attr != "update" or len(updates[0].args) != 1 or updates[0].keywords:
    raise ValueError("unsupported Harness runtime source additions")
for target, origin in ast.literal_eval(updates[0].args[0]).items():
    if origin == "//web:browser_source_identity":
        shutil.copyfile(web_identity, runtime / target)
    elif origin.startswith("//"):
        copy(labels[origin], runtime, target)
    else:
        copy("crates/harness/" + origin, runtime, target)
subprocess.run([sys.executable, str(issuer), str(runtime), str(output / "browser-source-identity.json")], check=True)
