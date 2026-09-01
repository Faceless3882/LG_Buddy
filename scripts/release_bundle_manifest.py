#!/usr/bin/env python3

"""Create and validate the identity manifest embedded in LG Buddy release bundles."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import tarfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

from release_promotion import PromotionError, SemVer


MANIFEST_NAME = "release-manifest.json"
SCHEMA_VERSION = 1
IDENTITY_FIELDS = ("release_tag", "version", "channel", "target", "commit")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
TARGET_RE = re.compile(r"^[a-z0-9][a-z0-9_.-]*$")
MAX_MANIFEST_BYTES = 64 * 1024


class ManifestError(RuntimeError):
    pass


@dataclass(frozen=True)
class ReleaseIdentity:
    release_tag: str
    version: str
    channel: str
    target: str
    commit: str


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

    return ReleaseIdentity(**fields)


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
    return identity


def validate_expected(identity: ReleaseIdentity, args: argparse.Namespace) -> None:
    for field in IDENTITY_FIELDS:
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
    for field in IDENTITY_FIELDS:
        parser.add_argument(f"--expected-{field.replace('_', '-')}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)

    create = subparsers.add_parser("create")
    create.add_argument("--output", type=Path, required=True)
    create.add_argument("--release-tag", required=True)
    create.add_argument("--target", required=True)
    create.add_argument("--binary", type=Path, required=True)

    validate = subparsers.add_parser("validate")
    source = validate.add_mutually_exclusive_group(required=True)
    source.add_argument("--manifest", type=Path)
    source.add_argument("--archive", type=Path)
    validate.add_argument("--binary", type=Path)
    add_expected_arguments(validate)

    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.command == "create":
            identity = binary_identity(
                args.binary,
                target=args.target,
                release_tag=args.release_tag,
            )
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
