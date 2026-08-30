#!/usr/bin/env python3

from __future__ import annotations

import io
import tarfile
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

from release_bundle_manifest import (
    IDENTITY_FIELDS,
    MANIFEST_NAME,
    ManifestError,
    ReleaseIdentity,
    parse_binary_identity,
    parse_manifest,
    render_manifest,
    validate_archive,
    validate_binary_matches,
    validate_expected,
    validate_manifest,
)


COMMIT = "0123456789abcdef0123456789abcdef01234567"
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


if __name__ == "__main__":
    unittest.main()
