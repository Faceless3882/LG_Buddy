#!/usr/bin/env python3

from __future__ import annotations

import io
import struct
import tarfile
import tempfile
import unittest
from argparse import Namespace
from dataclasses import replace
from pathlib import Path

from release_bundle_manifest import (
    EMBEDDED_IDENTITY_PREFIX,
    EMBEDDED_IDENTITY_SECTION,
    EMBEDDED_IDENTITY_SUFFIX,
    GUI_TARGET_FIELD,
    IDENTITY_FIELDS,
    MANIFEST_NAME,
    ManifestError,
    ReleaseIdentity,
    embedded_binary_identity,
    gui_bundle_path,
    parse_binary_identity,
    parse_manifest,
    render_manifest,
    validate_archive,
    validate_binary_matches,
    validate_expected,
    validate_input_binary,
    validate_manifest,
)


COMMIT = "0123456789abcdef0123456789abcdef01234567"
GUI_TARGET = "x86_64-unknown-linux-gnu"
IDENTITY = ReleaseIdentity(
    release_tag="v1.4.0-beta.1",
    version="1.4.0-beta.1",
    channel="prerelease",
    target="x86_64-unknown-linux-musl",
    commit=COMMIT,
)


def manifest_value(**overrides: object) -> dict[str, object]:
    value: dict[str, object] = {
        "schema_version": 1,
        "critical": list(IDENTITY_FIELDS),
        "release_tag": IDENTITY.release_tag,
        "version": IDENTITY.version,
        "channel": IDENTITY.channel,
        "target": IDENTITY.target,
        "commit": IDENTITY.commit,
    }
    value.update(overrides)
    return value


def embedded_binary(identity: ReleaseIdentity, target: str) -> bytes:
    binary_identity = ReleaseIdentity(
        release_tag=identity.release_tag,
        version=identity.version,
        channel=identity.channel,
        target=target,
        commit=identity.commit,
    )
    record = (
        EMBEDDED_IDENTITY_PREFIX
        + render_manifest(binary_identity).strip()
        + EMBEDDED_IDENTITY_SUFFIX
    )
    names = b"\0.shstrtab\0" + EMBEDDED_IDENTITY_SECTION + b"\0"
    section_table_offset = 64
    section_count = 3
    names_offset = section_table_offset + section_count * 64
    record_offset = names_offset + len(names)
    content = bytearray(record_offset + len(record))
    content[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", content, 18, 62)
    struct.pack_into("<Q", content, 40, section_table_offset)
    struct.pack_into("<H", content, 58, 64)
    struct.pack_into("<H", content, 60, section_count)
    struct.pack_into("<H", content, 62, 1)
    names_header = section_table_offset + 64
    struct.pack_into("<I", content, names_header, 1)
    struct.pack_into("<I", content, names_header + 4, 3)
    struct.pack_into("<Q", content, names_header + 24, names_offset)
    struct.pack_into("<Q", content, names_header + 32, len(names))
    identity_header = section_table_offset + 128
    struct.pack_into("<I", content, identity_header, names.index(EMBEDDED_IDENTITY_SECTION))
    struct.pack_into("<I", content, identity_header + 4, 1)
    struct.pack_into("<Q", content, identity_header + 24, record_offset)
    struct.pack_into("<Q", content, identity_header + 32, len(record))
    content[names_offset:record_offset] = names
    content[record_offset:] = record
    return bytes(content)


class ManifestTests(unittest.TestCase):
    def test_render_is_deterministic_and_round_trips(self) -> None:
        first = render_manifest(IDENTITY)
        second = render_manifest(IDENTITY)

        self.assertEqual(first, second)
        self.assertEqual(validate_manifest(parse_manifest(first)), IDENTITY)
        self.assertTrue(first.endswith(b"\n"))

    def test_duplicate_json_field_is_rejected(self) -> None:
        content = render_manifest(IDENTITY).replace(
            b'  "version": "1.4.0-beta.1",',
            b'  "version": "1.4.0-beta.1",\n  "version": "1.4.0-beta.2",',
        )

        with self.assertRaisesRegex(ManifestError, "duplicate manifest field: version"):
            parse_manifest(content)

    def test_missing_identity_field_is_rejected(self) -> None:
        value = manifest_value()
        del value["target"]

        with self.assertRaisesRegex(ManifestError, "missing or invalid.*target"):
            validate_manifest(value)

    def test_unknown_critical_field_is_rejected(self) -> None:
        value = manifest_value(critical=[*IDENTITY_FIELDS, "signature"])
        value["signature"] = "future"

        with self.assertRaisesRegex(ManifestError, "unknown critical.*signature"):
            validate_manifest(value)

    def test_unknown_noncritical_field_is_ignored(self) -> None:
        value = manifest_value(annotation="future")

        self.assertEqual(validate_manifest(value), IDENTITY)

    def test_gui_target_is_a_backward_compatible_manifest_extension(self) -> None:
        identity = replace(IDENTITY, gui_target=GUI_TARGET)
        rendered = render_manifest(identity)
        value = parse_manifest(rendered)

        self.assertNotIn(GUI_TARGET_FIELD, value["critical"])
        self.assertEqual(validate_manifest(value), identity)

        with self.assertRaisesRegex(ManifestError, "must be distinct"):
            validate_manifest(manifest_value(gui_target=IDENTITY.target))

    def test_schema_version_must_be_supported_integer(self) -> None:
        for schema_version in (True, "1", 2):
            with (
                self.subTest(schema_version=schema_version),
                self.assertRaisesRegex(ManifestError, "unsupported.*schema_version"),
            ):
                validate_manifest(manifest_value(schema_version=schema_version))

    def test_tag_must_match_version(self) -> None:
        with self.assertRaisesRegex(ManifestError, "tag must be exactly"):
            validate_manifest(manifest_value(release_tag="v1.4.0-beta.2"))

    def test_channel_must_match_semver_stage(self) -> None:
        with self.assertRaisesRegex(ManifestError, "channel must be prerelease"):
            validate_manifest(manifest_value(channel="stable"))

        stable = manifest_value(
            release_tag="v1.4.0", version="1.4.0", channel="prerelease"
        )
        with self.assertRaisesRegex(ManifestError, "channel must be stable"):
            validate_manifest(stable)

    def test_version_must_be_canonical_release_semver(self) -> None:
        for version in ("1.4", "v1.4.0", "1.4.0+local"):
            value = manifest_value(release_tag=f"v{version}", version=version)
            with self.subTest(version=version), self.assertRaises(ManifestError):
                validate_manifest(value)

    def test_target_and_commit_have_canonical_formats(self) -> None:
        with self.assertRaisesRegex(ManifestError, "invalid release manifest target"):
            validate_manifest(manifest_value(target="../../host"))
        with self.assertRaisesRegex(ManifestError, "full lowercase 40-character SHA"):
            validate_manifest(manifest_value(commit="ABC123"))

    def test_external_identity_mismatches_are_rejected(self) -> None:
        for field in IDENTITY_FIELDS:
            expected = {f"expected_{name}": None for name in IDENTITY_FIELDS}
            expected[f"expected_{field}"] = "different"
            with (
                self.subTest(field=field),
                self.assertRaisesRegex(ManifestError, f"release manifest {field}"),
            ):
                validate_expected(IDENTITY, Namespace(**expected))

    def test_binary_identity_requires_exact_output(self) -> None:
        output = (
            "lg-buddy 1.4.0-beta.1\n"
            "version: 1.4.0-beta.1\n"
            "channel: prerelease\n"
            f"commit: {COMMIT}\n"
        )

        self.assertEqual(
            parse_binary_identity(
                output,
                target=IDENTITY.target,
                release_tag=IDENTITY.release_tag,
            ),
            IDENTITY,
        )

        with self.assertRaisesRegex(ManifestError, "exactly four"):
            parse_binary_identity(
                f"{output}extra\n",
                target=IDENTITY.target,
                release_tag=IDENTITY.release_tag,
            )


class ArchiveTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary_directory.name)

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def archive(
        self, manifest: bytes | None = None, *, duplicate: bool = False
    ) -> Path:
        bundle_name = f"lg-buddy-{IDENTITY.version}-{IDENTITY.target}"
        archive = self.path / f"{bundle_name}.tar.gz"
        with tarfile.open(archive, mode="w:gz") as bundle:
            if manifest is not None:
                for _ in range(2 if duplicate else 1):
                    info = tarfile.TarInfo(f"{bundle_name}/{MANIFEST_NAME}")
                    info.size = len(manifest)
                    bundle.addfile(info, io.BytesIO(manifest))
            runtime = embedded_binary(IDENTITY, IDENTITY.target)
            runtime_info = tarfile.TarInfo(f"{bundle_name}/lg-buddy")
            runtime_info.mode = 0o755
            runtime_info.size = len(runtime)
            bundle.addfile(runtime_info, io.BytesIO(runtime))
        return archive

    def test_archive_identity_round_trips(self) -> None:
        self.assertEqual(
            validate_archive(self.archive(render_manifest(IDENTITY))), IDENTITY
        )

    def test_missing_and_duplicate_archive_manifests_are_rejected(self) -> None:
        with self.assertRaisesRegex(ManifestError, "exactly one"):
            validate_archive(self.archive())
        with self.assertRaisesRegex(ManifestError, "exactly one"):
            validate_archive(self.archive(render_manifest(IDENTITY), duplicate=True))

    def test_archive_name_must_match_manifest_identity(self) -> None:
        archive = self.archive(render_manifest(IDENTITY))
        renamed = archive.with_name("renamed.tar.gz")
        archive.rename(renamed)

        with self.assertRaisesRegex(ManifestError, "expects archive name"):
            validate_archive(renamed)

    def test_malformed_archive_manifest_is_rejected(self) -> None:
        with self.assertRaisesRegex(ManifestError, "not valid JSON"):
            validate_archive(self.archive(b"not json\n"))

    def test_archive_verifies_the_declared_gui_payload(self) -> None:
        identity = replace(IDENTITY, gui_target=GUI_TARGET)
        bundle_name = f"lg-buddy-{identity.version}-{identity.target}"
        archive = self.path / f"{bundle_name}.tar.gz"
        with tarfile.open(archive, mode="w:gz") as bundle:
            for relative, content in (
                (MANIFEST_NAME, render_manifest(identity)),
                ("lg-buddy", embedded_binary(identity, identity.target)),
                (gui_bundle_path(GUI_TARGET), embedded_binary(identity, GUI_TARGET)),
            ):
                info = tarfile.TarInfo(f"{bundle_name}/{relative}")
                info.mode = 0o755 if relative != MANIFEST_NAME else 0o644
                info.size = len(content)
                bundle.addfile(info, io.BytesIO(content))

        self.assertEqual(validate_archive(archive), identity)

    def test_archive_rejects_missing_non_executable_and_wrong_target_gui(self) -> None:
        identity = replace(IDENTITY, gui_target=GUI_TARGET)
        bundle_name = f"lg-buddy-{identity.version}-{identity.target}"
        for label, include_gui, gui_mode, gui_target in (
            ("missing", False, 0o755, GUI_TARGET),
            ("non-executable", True, 0o644, GUI_TARGET),
            ("wrong-target", True, 0o755, "aarch64-unknown-linux-gnu"),
        ):
            archive = self.path / f"{bundle_name}.tar.gz"
            with tarfile.open(archive, mode="w:gz") as bundle:
                entries = [
                    (MANIFEST_NAME, render_manifest(identity), 0o644),
                    (
                        "lg-buddy",
                        embedded_binary(identity, identity.target),
                        0o755,
                    ),
                ]
                if include_gui:
                    entries.append(
                        (
                            gui_bundle_path(GUI_TARGET),
                            embedded_binary(identity, gui_target),
                            gui_mode,
                        )
                    )
                for relative, content, mode in entries:
                    info = tarfile.TarInfo(f"{bundle_name}/{relative}")
                    info.mode = mode
                    info.size = len(content)
                    bundle.addfile(info, io.BytesIO(content))

            with self.subTest(label=label), self.assertRaises(ManifestError):
                validate_archive(archive)

    def test_embedded_identity_reads_the_actual_target(self) -> None:
        observed = embedded_binary_identity(embedded_binary(IDENTITY, GUI_TARGET))

        self.assertEqual(observed.target, GUI_TARGET)


class BinaryComparisonTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.binary = Path(self.temporary_directory.name) / "lg-buddy"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def write_binary(self, *, version: str, channel: str, commit: str) -> None:
        self.binary.write_text(
            "#!/bin/sh\n"
            "cat <<'EOF'\n"
            f"lg-buddy {version}\n"
            f"version: {version}\n"
            f"channel: {channel}\n"
            f"commit: {commit}\n"
            "EOF\n",
            encoding="utf-8",
        )
        self.binary.chmod(0o755)

    def test_binary_identity_must_match_manifest(self) -> None:
        self.write_binary(
            version=IDENTITY.version,
            channel=IDENTITY.channel,
            commit=IDENTITY.commit,
        )
        validate_binary_matches(IDENTITY, self.binary)

        mismatches = (
            ("1.4.0-beta.2", IDENTITY.channel, IDENTITY.commit, "version"),
            (IDENTITY.version, "stable", IDENTITY.commit, "channel"),
            (IDENTITY.version, IDENTITY.channel, "f" * 40, "commit"),
        )
        for version, channel, commit, field in mismatches:
            with self.subTest(field=field):
                self.write_binary(version=version, channel=channel, commit=commit)
                with self.assertRaises(ManifestError):
                    validate_binary_matches(IDENTITY, self.binary)

    def test_release_input_must_be_a_safe_executable_regular_file(self) -> None:
        self.binary.write_bytes(embedded_binary(IDENTITY, IDENTITY.target))
        self.binary.chmod(0o755)
        validate_input_binary(self.binary, label="runtime")

        self.binary.chmod(0o644)
        with self.assertRaisesRegex(ManifestError, "not executable"):
            validate_input_binary(self.binary, label="runtime")

        self.binary.chmod(0o775)
        with self.assertRaisesRegex(ManifestError, "writable"):
            validate_input_binary(self.binary, label="runtime")


if __name__ == "__main__":
    unittest.main()
