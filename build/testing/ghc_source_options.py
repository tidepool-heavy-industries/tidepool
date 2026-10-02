"""Verify archived worker Haskell source contains no per-module compiler flags."""

import hashlib
from pathlib import Path
import re
import tarfile

SOURCE_ROOTS = ("bridge/haskell/src/", "bridge/haskell/app/")
HASKELL_SUFFIXES = (".hs", ".lhs")
GENERATED_SUFFIXES = (".hsc", ".chs", ".x", ".y")
OPTIONS_PRAGMA = re.compile(r"\{-#\s*OPTIONS(?:_GHC)?\b(.*?)#-\}", re.DOTALL | re.IGNORECASE)
CPP_DIRECTIVE = re.compile(r"(?m)^[ \t]*#[ \t]*(?:if|ifdef|ifndef|elif|else|endif|define|undef|include|line)\b")
CPP_ENABLE = re.compile(r"(?<![A-Za-z0-9_])(?:-cpp|-XCPP|-F|-pgmP|-pgmF|-optP)(?![A-Za-z0-9_])")

COMPILING = re.compile(
    r"^(?:\[[^]]+\]\s+)?Compiling\s+.+?\s+\(\s*(.+?),\s*(.+?)(?:,\s*.+?)?\s*\)$"
)


def _canonical(path):
    return Path(path).resolve(strict=False)


def _under(path, root):
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _option_value(argv, option, *, required=True):
    positions = [index for index, value in enumerate(argv) if value == option]
    if not positions and not required:
        return None
    if len(positions) != 1 or positions[0] + 1 >= len(argv):
        raise ValueError(f"GHC action must contain exactly one {option} with a value")
    return argv[positions[0] + 1]


def _effective_profile(argv, build_root):
    """Remove only link-mode and its output path from a --make profile."""
    if argv.count("-no-link") > 1 or argv.count("-o") > 1 or ("-no-link" in argv and "-o" in argv):
        raise ValueError("GHC --make action has conflicting link-mode arguments")
    result = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "-no-link":
            index += 1
            continue
        if arg == "-o":
            if index + 1 >= len(argv):
                raise ValueError("GHC --make link output has no path")
            output = _canonical(argv[index + 1])
            if not _under(output, build_root):
                raise ValueError("GHC --make link output is outside the fresh build directory")
            index += 2
            continue
        result.append(arg)
        index += 1
    return tuple(result)


def _ghc_action(row, compiler, build_root):
    """Validate one raw --make row and return its effective output directory."""
    argv = row.get("argv")
    if (not isinstance(argv, list) or not argv
            or _canonical(argv[0]) != compiler):
        raise ValueError("Haskell --make action did not invoke the pinned compiler")
    if row.get("exit_code") != 0:
        raise ValueError("Haskell --make action did not exit successfully")
    if "--make" not in argv:
        raise ValueError("internal verifier error: non --make action")
    if any(arg.startswith("@") for arg in argv):
        raise ValueError("Haskell --make action retains an unexpanded response file")
    if any(arg in ("-fno-code", "-E", "-M", "-S") for arg in argv):
        raise ValueError("Haskell --make action does not produce object code")

    optimization = [arg for arg in argv if arg.startswith("-O") and not arg.startswith("-opt")]
    if not optimization or any(arg != "-O2" for arg in optimization):
        raise ValueError("Haskell --make action does not have effective -O2")

    forbidden_exact = {"-cpp", "-XCPP", "-F", "-fvia-C", "-x"}
    forbidden_prefixes = ("-pgmF", "-pgmP", "-pgmL", "-optF", "-optL")
    if any(arg in forbidden_exact or arg.startswith(forbidden_prefixes)
           or (arg.startswith("-x") and len(arg) > 2) for arg in argv):
        raise ValueError("Haskell --make action enables unsupported preprocessing or phase override")

    # Cabal's generated macro header is inert when CPP is off. Admit only its
    # exact adjacent pair, and only from this invocation's fresh build tree.
    preprocessor_args = [index for index, arg in enumerate(argv) if arg.startswith("-optP")]
    if preprocessor_args:
        if len(preprocessor_args) != 2:
            raise ValueError("unexpected GHC preprocessor arguments")
        index, header_index = preprocessor_args
        if (argv[index] != "-optP-include" or header_index != index + 1
                or not argv[header_index].startswith("-optP/")):
            raise ValueError("unexpected GHC preprocessor arguments")
        header = _canonical(argv[header_index][len("-optP"):])
        if (header.name != "cabal_macros.h" or header.parent.name != "autogen"
                or not _under(header, build_root)):
            raise ValueError("Cabal macro include is outside the fresh build directory")

    cwd_value = row.get("cwd")
    if not isinstance(cwd_value, str) or not cwd_value:
        raise ValueError("Haskell --make action has no recorded working directory")
    if not Path(cwd_value).is_absolute():
        raise ValueError("Haskell --make action working directory is not absolute")
    cwd = _canonical(cwd_value)
    outputdir = _canonical(_option_value(argv, "-outputdir"))
    odir_value = _option_value(argv, "-odir", required=False)
    odir = _canonical(odir_value) if odir_value else outputdir
    if odir != outputdir:
        raise ValueError("GHC outputdir and object directory differ; compilation attribution is ambiguous")
    if not _under(outputdir, build_root):
        raise ValueError("GHC output directory is outside the fresh build directory")
    return {"cwd": cwd, "outputdir": outputdir, "optimization": optimization[0],
            "preprocessor": tuple(argv[index:index + 2] for index in preprocessor_args),
            "profile": _effective_profile(argv, build_root)}


def verify_worker_home_actions(rows, build_log_text, compiler, project_root,
                               build_directory, source_options):
    """Bind actual GHC --make actions and Compiling events to the captured HOME tree.

    `source_options` is the result of `audit_source_optimization_options`, not a
    caller-supplied module list. The build log provides GHC's actual source and
    object paths; module names and wrapper classification labels are ignored.
    """
    if (not isinstance(source_options, dict)
            or source_options.get("schema") != "ghc-source-options-v1"
            or not isinstance(source_options.get("sources"), dict)):
        raise ValueError("worker source-options evidence is missing or malformed")
    source_hashes = source_options["sources"]
    if not source_hashes or any(not isinstance(path, str) for path in source_hashes):
        raise ValueError("worker source-options evidence has no authenticated source paths")

    if not Path(project_root).is_absolute() or not Path(build_directory).is_absolute():
        raise ValueError("project root and fresh build directory must be absolute")
    if not Path(compiler).is_absolute():
        raise ValueError("pinned compiler path must be absolute")
    compiler = _canonical(compiler)
    project_root = _canonical(project_root)
    build_root = _canonical(build_directory)
    if not _under(build_root, project_root):
        raise ValueError("fresh build directory is outside the project checkout")

    actions_by_output = {}
    make_action_count = 0
    for row in rows:
        argv = row.get("argv") if isinstance(row, dict) else None
        if not isinstance(argv, list):
            raise ValueError("raw compiler invocation has no expanded argv")
        # Determine contribution from actual compiler argv, not recorder labels.
        if "--make" not in argv:
            standalone_haskell = [arg for arg in argv[1:]
                                  if not arg.startswith("-") and Path(arg).suffix.lower() in HASKELL_SUFFIXES]
            hidden_source = any(arg.startswith("@") or arg == "-x"
                                or (arg.startswith("-x") and len(arg) > 2) for arg in argv)
            if standalone_haskell or hidden_source:
                raise ValueError("standalone Haskell compile is outside the --make evidence audit")
            continue
        action = _ghc_action(row, compiler, build_root)
        make_action_count += 1
        previous = actions_by_output.setdefault(action["outputdir"], action)
        if previous != action:
            raise ValueError("conflicting GHC actions share an output directory")
    if not make_action_count:
        raise ValueError("no successful pinned GHC --make actions were recorded")

    expected_paths = set(source_hashes)
    expected_absolute = {}
    for relative in expected_paths:
        source = _canonical(project_root / relative)
        if not _under(source, project_root):
            raise ValueError(f"captured worker source path is outside the checkout: {relative}")
        expected_absolute[source] = relative

    compiled = {}
    for line_number, line in enumerate(build_log_text.splitlines(), 1):
        if "Compiling " not in line:
            continue
        match = COMPILING.match(line)
        if not match:
            raise ValueError(f"cannot parse GHC Compiling event on log line {line_number}")
        source_text, object_text = match.groups()
        object_argument = Path(object_text)
        if not object_argument.is_absolute():
            raise ValueError(f"GHC object path is not absolute: {object_text}")
        object_path = _canonical(object_argument)
        matching_outputs = [output for output in actions_by_output
                            if _under(object_path, output)]
        if len(matching_outputs) != 1:
            raise ValueError(f"GHC object path has missing or ambiguous --make attribution: {object_text}")
        action = actions_by_output[matching_outputs[0]]
        source_argument = Path(source_text)
        source = (_canonical(source_argument) if source_argument.is_absolute()
                  else _canonical(action["cwd"] / source_argument))
        relative = expected_absolute.get(source)
        if relative is None:
            raise ValueError(f"GHC compiled source outside captured HOME tree: {source_text}")
        if relative in compiled:
            raise ValueError(f"captured worker source was compiled more than once: {relative}")
        compiled[relative] = {"source": str(source), "object": str(object_path),
                              "outputdir": str(matching_outputs[0])}

    missing = expected_paths - compiled.keys()
    if missing:
        raise ValueError("captured worker sources were not compiled: " + ", ".join(sorted(missing)[:5]))
    return {"make_action_count": make_action_count,
            "compiled_source_count": len(compiled),
            "compiled_sources": compiled}


def reject_preprocessing_arguments(argv):
    """Reject source rewriting and phase overrides in recorded compile actions."""
    # CPP program/options are inert without CPP; archived source also refuses LANGUAGE CPP.
    prefixes = ("-pgmP", "-pgmF", "-pgmL", "-optP", "-optF", "-optL", "-x")
    if any(arg in ("-cpp", "-XCPP", "-F") or arg.startswith(prefixes) for arg in argv):
        raise ValueError("preprocessed GHC action is outside source pragma audit scope")


def audit_source_optimization_options(archive_path, before):
    """Audit the captured worker source bytes; fail closed on module flags and CPP."""
    archive_path = Path(archive_path)
    if before.get("version") != 1 or before.get("capture_changes") != []:
        raise ValueError("source option audit requires a complete source snapshot")
    with archive_path.open("rb") as stream:
        archive_digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if not isinstance(before.get("archive_sha256"), str) or archive_digest != before["archive_sha256"]:
        raise ValueError("source option audit archive digest differs from its manifest")
    entries = {entry["path"]: entry for entry in before.get("source", {}).get("entries", [])}
    candidates = {path: entry for path, entry in entries.items()
                  if any(path.startswith(prefix) for prefix in SOURCE_ROOTS)}
    if not candidates:
        raise ValueError("source snapshot contains no worker HOME source roots")
    expected = {path for path in candidates if Path(path).suffix in HASKELL_SUFFIXES}
    if not expected:
        raise ValueError("source snapshot contains no worker HOME Haskell files")
    scanned = {}
    try:
        with tarfile.open(archive_path, "r:") as archive:
            for member in archive:
                if member.name not in candidates:
                    continue
                if member.name in scanned:
                    raise ValueError(f"duplicate worker source archive member: {member.name}")
                entry = candidates[member.name]
                suffix = Path(member.name).suffix
                if suffix in GENERATED_SUFFIXES:
                    raise ValueError(f"unsupported generated Haskell source in worker source root: {member.name}")
                if suffix not in HASKELL_SUFFIXES:
                    continue
                if not member.isfile() or entry.get("kind") != "file":
                    raise ValueError(f"worker source is not a captured regular file: {member.name}")
                stream = archive.extractfile(member)
                if stream is None:
                    raise ValueError(f"worker source is absent from archive: {member.name}")
                source = stream.read()
                if len(source) != entry.get("bytes") or hashlib.sha256(source).hexdigest() != entry.get("sha256"):
                    raise ValueError(f"worker source archive member differs from manifest: {member.name}")
                scanned[member.name] = entry["sha256"]
                source_text = source.decode("utf-8")
                if CPP_DIRECTIVE.search(source_text):
                    raise ValueError(f"CPP source is outside pragma audit scope: {member.name}")
                if re.search(r"\{-#\s*LANGUAGE\b[^#}]*\bCPP\b", source_text, re.IGNORECASE):
                    raise ValueError(f"CPP-enabled source is outside pragma audit scope: {member.name}")
                for pragma in OPTIONS_PRAGMA.finditer(source_text):
                    if CPP_ENABLE.search(pragma.group(1)):
                        raise ValueError(f"OPTIONS/OPTIONS_GHC enables preprocessing outside audit scope: {member.name}")
                    # The effective optimizer switch family is too broad for an allowlist.
                    raise ValueError(f"OPTIONS/OPTIONS_GHC pragma is outside worker evidence contract: {member.name}")
    except tarfile.TarError as error:
        raise ValueError("source archive is not a readable tar file") from error
    if not expected.issubset(scanned):
        missing = sorted(expected - scanned.keys())
        raise ValueError("captured worker source is missing from archive: " + ", ".join(missing[:5]))
    return {"schema": "ghc-source-options-v1", "roots": list(SOURCE_ROOTS),
            "scanned_files": len(scanned), "sources": scanned,
            "policy": "reject OPTIONS/OPTIONS_GHC pragmas and CPP"}
