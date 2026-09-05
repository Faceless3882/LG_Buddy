#!/usr/bin/env python3

"""Create and validate the identity manifest embedded in LG Buddy release bundles."""

from __future__ import annotations

import argparse
import json
import re
import stat
import struct
import subprocess
import tarfile
from dataclasses import dataclass, replace
from pathlib import Path, PurePosixPath
from typing import Any

from release_promotion import PromotionError, SemVer


MANIFEST_NAME = "release-manifest.json"
SCHEMA_VERSION = 1
IDENTITY_FIELDS = ("release_tag", "version", "channel", "target", "commit")
GUI_TARGET_FIELD = "gui_target"
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
TARGET_RE = re.compile(r"^[a-z0-9][a-z0-9_.-]*$")
MAX_MANIFEST_BYTES = 64 * 1024
MAX_BINARY_BYTES = 128 * 1024 * 1024
EMBEDDED_IDENTITY_PREFIX = b"LG_BUDDY_RELEASE_IDENTITY_V1\0"
EMBEDDED_IDENTITY_SUFFIX = b"\0LG_BUDDY_RELEASE_IDENTITY_END\0"
EMBEDDED_IDENTITY_SECTION = b".lg_buddy.identity"


class ManifestError(RuntimeError):
    pass


@dataclass(frozen=True)
class ReleaseIdentity:
    release_tag: str
    version: str
    channel: str
    target: str
    commit: str
    gui_target: str | None = None


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ManifestError(f"duplicate manifest field: {key}")
        result[key] = value
    return result


def parse_manifest(content: bytes) -> dict[str, Any]:
    if len(content) > MAX_MANIFEST_BYTES:
        raise ManifestError("release manifest exceeds the 64 KiB size limit")
    try:
        text = content.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ManifestError("release manifest is not valid UTF-8") from error
    try:
        value = json.loads(text, object_pairs_hook=reject_duplicate_keys)
    except ManifestError:
        raise
    except json.JSONDecodeError as error:
        raise ManifestError(
            f"release manifest is not valid JSON: {error.msg}"
        ) from error
    if not isinstance(value, dict):
        raise ManifestError("release manifest root must be a JSON object")
    return value


def validate_manifest(value: dict[str, Any]) -> ReleaseIdentity:
    schema_version = value.get("schema_version")
    if type(schema_version) is not int or schema_version != SCHEMA_VERSION:
        raise ManifestError(
            f"unsupported release manifest schema_version: {schema_version!r}"
        )

    critical = value.get("critical")
    if not isinstance(critical, list) or any(
        not isinstance(field, str) for field in critical
    ):
        raise ManifestError("release manifest critical must be an array of field names")
    if len(critical) != len(set(critical)):
        raise ManifestError("release manifest critical contains a duplicate field name")

    unknown_critical = sorted(set(critical) - set(IDENTITY_FIELDS))
    if unknown_critical:
        raise ManifestError(
            f"unknown critical release manifest field: {unknown_critical[0]}"
        )
    missing_critical = sorted(set(IDENTITY_FIELDS) - set(critical))
    if missing_critical:
        raise ManifestError(
            f"required identity field is not marked critical: {missing_critical[0]}"
        )

    fields: dict[str, str] = {}
    for field in IDENTITY_FIELDS:
        field_value = value.get(field)
        if not isinstance(field_value, str) or not field_value:
            raise ManifestError(f"missing or invalid release manifest field: {field}")
        fields[field] = field_value

    version_text = fields["version"]
    try:
        version = SemVer.parse(version_text)
    except PromotionError as error:
        raise ManifestError(str(error)) from error
    if version.build:
        raise ManifestError("release manifest version must not contain build metadata")

    if fields["release_tag"] != f"v{version_text}":
        raise ManifestError(
            "release manifest tag must be exactly v followed by the manifest version"
        )

    expected_channel = "prerelease" if version.prerelease else "stable"
    if fields["channel"] != expected_channel:
        raise ManifestError(
            f"release manifest channel must be {expected_channel} for version {version_text}"
        )

    if TARGET_RE.fullmatch(fields["target"]) is None:
        raise ManifestError(f"invalid release manifest target: {fields['target']}")
    if COMMIT_RE.fullmatch(fields["commit"]) is None:
        raise ManifestError(
            "release manifest commit must be a full lowercase 40-character SHA"
        )

    gui_target = value.get(GUI_TARGET_FIELD)
    if gui_target is not None:
        if not isinstance(gui_target, str) or TARGET_RE.fullmatch(gui_target) is None:
            raise ManifestError(f"invalid release manifest gui_target: {gui_target!r}")
        if gui_target == fields["target"]:
            raise ManifestError("release manifest runtime and GUI targets must be distinct")

    return ReleaseIdentity(**fields, gui_target=gui_target)


def render_manifest(identity: ReleaseIdentity) -> bytes:
    value = {
        "schema_version": SCHEMA_VERSION,
        "critical": list(IDENTITY_FIELDS),
        "release_tag": identity.release_tag,
        "version": identity.version,
        "channel": identity.channel,
        "target": identity.target,
        "commit": identity.commit,
    }
    if identity.gui_target is not None:
        value[GUI_TARGET_FIELD] = identity.gui_target
    validate_manifest(value)
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("utf-8")


def parse_binary_identity(
    output: str, *, target: str, release_tag: str
) -> ReleaseIdentity:
    lines = output.splitlines()
    if len(lines) != 4:
        raise ManifestError("lg-buddy --version must emit exactly four identity lines")

    prefixes = ("lg-buddy ", "version: ", "channel: ", "commit: ")
    if any(not line.startswith(prefix) for line, prefix in zip(lines, prefixes)):
        raise ManifestError("lg-buddy --version output has an unexpected format")

    headline_version = lines[0][len(prefixes[0]) :]
    version = lines[1][len(prefixes[1]) :]
    if headline_version != version:
        raise ManifestError("lg-buddy --version headline and version field disagree")

    identity = ReleaseIdentity(
        release_tag=release_tag,
        version=version,
        channel=lines[2][len(prefixes[2]) :],
        target=target,
        commit=lines[3][len(prefixes[3]) :],
    )
    validate_manifest(parse_manifest(render_manifest(identity)))
    return identity


def binary_identity(binary: Path, *, target: str, release_tag: str) -> ReleaseIdentity:
    result = subprocess.run(
        [binary, "--version"],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise ManifestError(f"cannot read bundled binary identity: {detail}")
    return parse_binary_identity(result.stdout, target=target, release_tag=release_tag)


def validate_input_binary(binary: Path, *, label: str) -> None:
    try:
        metadata = binary.lstat()
    except FileNotFoundError as error:
        raise ManifestError(f"{label} not found: {binary}") from error
    if not stat.S_ISREG(metadata.st_mode):
        raise ManifestError(f"{label} must be a regular file: {binary}")
    if metadata.st_mode & 0o111 == 0:
        raise ManifestError(f"{label} is not executable: {binary}")
    if metadata.st_mode & 0o022:
        raise ManifestError(f"{label} is writable by its group or other users: {binary}")
    if metadata.st_size > MAX_BINARY_BYTES:
        raise ManifestError(f"{label} exceeds 128 MiB: {binary}")


def embedded_identity_section(content: bytes) -> bytes:
    if len(content) > MAX_BINARY_BYTES:
        raise ManifestError("bundled binary exceeds the 128 MiB size limit")
    if (
        content[:4] != b"\x7fELF"
        or content[4:6] != b"\x02\x01"
        or content[18:20] != struct.pack("<H", 62)
    ):
        raise ManifestError("bundled binary is not an x86-64 little-endian ELF file")

    try:
        section_offset = struct.unpack_from("<Q", content, 40)[0]
        section_entry_size = struct.unpack_from("<H", content, 58)[0]
        section_count = struct.unpack_from("<H", content, 60)[0]
        names_index = struct.unpack_from("<H", content, 62)[0]
    except struct.error as error:
        raise ManifestError("bundled ELF header is truncated") from error
    if section_entry_size < 64 or section_count == 0 or names_index >= section_count:
        raise ManifestError("bundled ELF section table is invalid")

    table_end = section_offset + section_entry_size * section_count
    if table_end > len(content):
        raise ManifestError("bundled ELF section table is out of bounds")

    def section_header(index: int) -> tuple[int, int, int]:
        header = section_offset + section_entry_size * index
        try:
            name = struct.unpack_from("<I", content, header)[0]
            offset = struct.unpack_from("<Q", content, header + 24)[0]
            size = struct.unpack_from("<Q", content, header + 32)[0]
        except struct.error as error:
            raise ManifestError("bundled ELF section header is truncated") from error
        if offset + size > len(content):
            raise ManifestError("bundled ELF section payload is out of bounds")
        return name, offset, size

    _, names_offset, names_size = section_header(names_index)
    names = content[names_offset : names_offset + names_size]
    matches: list[bytes] = []
    for index in range(section_count):
        name_offset, payload_offset, payload_size = section_header(index)
        if name_offset >= len(names):
            raise ManifestError("bundled ELF section name is out of bounds")
        name_end = names.find(b"\0", name_offset)
        if name_end < 0:
            raise ManifestError("bundled ELF section name is unterminated")
        if names[name_offset:name_end] == EMBEDDED_IDENTITY_SECTION:
            matches.append(content[payload_offset : payload_offset + payload_size])
    if len(matches) != 1:
        raise ManifestError(
            "bundled ELF must contain exactly one .lg_buddy.identity section"
        )
    return matches[0]


def embedded_binary_identity(content: bytes) -> ReleaseIdentity:
    record = embedded_identity_section(content)
    if not record.startswith(EMBEDDED_IDENTITY_PREFIX) or not record.endswith(
        EMBEDDED_IDENTITY_SUFFIX
    ):
        raise ManifestError("embedded identity section has an invalid envelope")
    payload = record[
        len(EMBEDDED_IDENTITY_PREFIX) : -len(EMBEDDED_IDENTITY_SUFFIX)
    ]
    identity = validate_manifest(parse_manifest(payload))
    if identity.gui_target is not None:
        raise ManifestError("embedded binary identity must not declare gui_target")
    return identity


def validate_embedded_binary_matches(
    identity: ReleaseIdentity, content: bytes, *, target: str, label: str
) -> None:
    observed = embedded_binary_identity(content)
    expected = replace(identity, target=target, gui_target=None)
    if observed != expected:
        raise ManifestError(
            f"{label} embedded identity {observed!r} does not match {expected!r}"
        )


def validate_embedded_binary_file_matches(
    identity: ReleaseIdentity, binary: Path, *, target: str, label: str
) -> None:
    if binary.stat().st_size > MAX_BINARY_BYTES:
        raise ManifestError(f"{label} exceeds 128 MiB: {binary}")
    with binary.open("rb") as handle:
        content = handle.read(MAX_BINARY_BYTES + 1)
    validate_embedded_binary_matches(
        identity, content, target=target, label=label
    )


def validate_gui_binary_matches(identity: ReleaseIdentity, binary: Path) -> None:
    if identity.gui_target is None:
        raise ManifestError("release manifest does not declare gui_target")
    observed = binary_identity(
        binary,
        target=identity.gui_target,
        release_tag=identity.release_tag,
    )
    for field in ("version", "channel", "commit"):
        if getattr(observed, field) != getattr(identity, field):
            raise ManifestError(
                f"bundled GUI {field} is {getattr(observed, field)}, "
                f"manifest records {getattr(identity, field)}"
            )
    validate_embedded_binary_file_matches(
        identity,
        binary,
        target=identity.gui_target,
        label="bundled GUI",
    )


def gui_bundle_path(gui_target: str) -> str:
    return f"docs/lg-buddy-gui-{gui_target}"


def manifest_from_archive(archive: Path) -> tuple[dict[str, Any], str]:
    try:
        with tarfile.open(archive, mode="r:gz") as bundle:
            matches = []
            for member in bundle.getmembers():
                path = PurePosixPath(member.name)
                if (
                    member.isfile()
                    and len(path.parts) == 2
                    and path.parts[1] == MANIFEST_NAME
                ):
                    matches.append(member)
            if len(matches) != 1:
                raise ManifestError(
                    f"release archive must contain exactly one top-level {MANIFEST_NAME}"
                )
            member = matches[0]
            if member.size > MAX_MANIFEST_BYTES:
                raise ManifestError("release manifest exceeds the 64 KiB size limit")
            extracted = bundle.extractfile(member)
            if extracted is None:
                raise ManifestError(f"cannot read {member.name} from release archive")
            return parse_manifest(extracted.read()), PurePosixPath(member.name).parts[0]
    except ManifestError:
        raise
    except (OSError, tarfile.TarError) as error:
        raise ManifestError(f"cannot read release archive: {error}") from error


def validate_archive(archive: Path) -> ReleaseIdentity:
    value, bundle_root = manifest_from_archive(archive)
    identity = validate_manifest(value)
    expected_bundle_name = f"lg-buddy-{identity.version}-{identity.target}"
    if bundle_root != expected_bundle_name:
        raise ManifestError(
            f"release manifest identity expects bundle root {expected_bundle_name}, found {bundle_root}"
        )
    expected_archive_name = f"{expected_bundle_name}.tar.gz"
    if archive.name != expected_archive_name:
        raise ManifestError(
            f"release manifest identity expects archive name {expected_archive_name}, found {archive.name}"
        )
    with tarfile.open(archive, mode="r:gz") as bundle:
        required = [("lg-buddy", identity.target, "bundled runtime")]
        if identity.gui_target is not None:
            required.append(
                (
                    gui_bundle_path(identity.gui_target),
                    identity.gui_target,
                    "bundled GUI",
                )
            )
        for relative, target, label in required:
            expected_path = f"{bundle_root}/{relative}"
            matches = [member for member in bundle.getmembers() if member.name == expected_path]
            if len(matches) != 1 or not matches[0].isfile():
                raise ManifestError(
                    f"release archive must contain exactly one regular {expected_path}"
                )
            member = matches[0]
            if member.mode & 0o111 == 0:
                raise ManifestError(f"release archive {relative} is not executable")
            if member.size > MAX_BINARY_BYTES:
                raise ManifestError(f"release archive {relative} exceeds 128 MiB")
            extracted = bundle.extractfile(member)
            if extracted is None:
                raise ManifestError(f"cannot read {expected_path} from release archive")
            validate_embedded_binary_matches(
                identity,
                extracted.read(),
                target=target,
                label=label,
            )
    return identity


def validate_expected(identity: ReleaseIdentity, args: argparse.Namespace) -> None:
    for field in (*IDENTITY_FIELDS, GUI_TARGET_FIELD):
        expected = getattr(args, f"expected_{field}", None)
        if expected is not None and getattr(identity, field) != expected:
            raise ManifestError(
                f"release manifest {field} is {getattr(identity, field)}, expected {expected}"
            )


def validate_binary_matches(identity: ReleaseIdentity, binary: Path) -> None:
    observed = binary_identity(
        binary,
        target=identity.target,
        release_tag=identity.release_tag,
    )
    for field in ("version", "channel", "commit"):
        if getattr(observed, field) != getattr(identity, field):
            raise ManifestError(
                f"bundled binary {field} is {getattr(observed, field)}, "
                f"manifest records {getattr(identity, field)}"
            )


def add_expected_arguments(parser: argparse.ArgumentParser) -> None:
    for field in (*IDENTITY_FIELDS, GUI_TARGET_FIELD):
        parser.add_argument(f"--expected-{field.replace('_', '-')}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create")
    create.add_argument("--output", type=Path, required=True)
    create.add_argument("--release-tag", required=True)
    create.add_argument("--target", required=True)
    create.add_argument("--binary", type=Path, required=True)
    create.add_argument("--gui-target", required=True)
    create.add_argument("--gui-binary", type=Path, required=True)

    validate = subparsers.add_parser("validate")
    source = validate.add_mutually_exclusive_group(required=True)
    source.add_argument("--manifest", type=Path)
    source.add_argument("--archive", type=Path)
    validate.add_argument("--binary", type=Path)
    validate.add_argument("--gui-binary", type=Path)
    add_expected_arguments(validate)

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "create":
            validate_input_binary(args.binary, label="release runtime binary")
            validate_input_binary(args.gui_binary, label="release GUI binary")
            runtime_identity = binary_identity(
                args.binary,
                target=args.target,
                release_tag=args.release_tag,
            )
            identity = replace(runtime_identity, gui_target=args.gui_target)
            validate_manifest(parse_manifest(render_manifest(identity)))
            validate_embedded_binary_file_matches(
                identity,
                args.binary,
                target=args.target,
                label="bundled runtime",
            )
            validate_gui_binary_matches(identity, args.gui_binary)
            args.output.write_bytes(render_manifest(identity))
            print(f"Created {args.output}")
            return 0

        if args.archive is not None:
            identity = validate_archive(args.archive)
        else:
            identity = validate_manifest(parse_manifest(args.manifest.read_bytes()))
        validate_expected(identity, args)
        if args.binary is not None:
            validate_binary_matches(identity, args.binary)
            validate_embedded_binary_file_matches(
                identity,
                args.binary,
                target=identity.target,
                label="bundled runtime",
            )
        if args.gui_binary is not None:
            validate_gui_binary_matches(identity, args.gui_binary)
    except (ManifestError, OSError) as error:
        raise SystemExit(
            f"release bundle manifest validation failed: {error}"
        ) from error

    print(
        f"Validated {identity.release_tag} for {identity.target} at {identity.commit}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
