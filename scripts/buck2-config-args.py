#!/usr/bin/env python3
"""Merge a private JSON Buck config map into one Buck command argv."""

import json
import os
import re
import stat
import sys

MAX_CONFIG_BYTES = 64 * 1024
KEY = re.compile(r"[A-Za-z0-9_.-]+\Z")


def read_config(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o077 or info.st_size > MAX_CONFIG_BYTES):
            raise ValueError("file must be a private, current-user regular file of at most 64 KiB")
        chunks = []
        remaining = MAX_CONFIG_BYTES + 1
        while remaining:
            chunk = os.read(descriptor, min(8192, remaining))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if len(data) > MAX_CONFIG_BYTES:
            raise ValueError("file exceeds 64 KiB")
    finally:
        os.close(descriptor)

    def unique_pairs(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate Buck config key")
            result[key] = value
        return result

    config = json.loads(data, object_pairs_hook=unique_pairs)
    if (not isinstance(config, dict) or not config
            or any(not isinstance(key, str) or not KEY.fullmatch(key) for key in config)
            or any(not isinstance(value, str) or "\0" in value for value in config.values())):
        raise ValueError("expected a nonempty JSON object of string Buck config values")
    return config


def explicit_configs(args):
    selected = {}
    index = 1
    while index < len(args) and args[index] != "--":
        argument = args[index]
        config = None
        if argument in ("-c", "--config"):
            index += 1
            if index >= len(args):
                raise ValueError(f"{argument} requires KEY=VALUE")
            config = args[index]
        elif argument.startswith("--config="):
            config = argument[len("--config="):]
        elif argument.startswith("-c") and len(argument) > 2:
            config = argument[2:]
        if config is not None:
            key, separator, value = config.partition("=")
            if not separator or not key:
                raise ValueError("Buck config arguments must use KEY=VALUE")
            if key in selected and selected[key] != value:
                raise ValueError(f"conflicting explicit Buck config for {key}")
            selected[key] = value
        index += 1
    return selected


def merged_argv(config, args):
    if not args or args[0] == "--":
        raise ValueError("missing Buck command")
    explicit = explicit_configs(args)
    for key, value in config.items():
        if key in explicit and explicit[key] != value:
            raise ValueError(f"explicit Buck config conflicts with shared value for {key}")
    missing = [(key, value) for key, value in config.items() if key not in explicit]
    return [args[0], *[item for key, value in missing for item in ("-c", f"{key}={value}")], *args[1:]]


def main(argv):
    try:
        separator = argv.index("--")
        path, args = argv[0], argv[separator + 1:]
        result = merged_argv(read_config(path), args)
        sys.stdout.buffer.write(b"\0".join(item.encode() for item in result) + b"\0")
        return 0
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        print(f"shared Buck configuration: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
