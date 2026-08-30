#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from release_promotion import PromotionError, SemVer, validate_promotion


class RepositoryFixture:
    def __init__(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.path = Path(self.temporary_directory.name)
        self.git("init", "--initial-branch=main")
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "release-test@example.invalid")
        self.write_version("1.3.0")
        self.git("add", ".")
        self.git("commit", "-m", "stable")
        self.stable_sha = self.git("rev-parse", "HEAD")
        self.git("branch", "prerelease")
        self.git("branch", "dev")

    def close(self) -> None:
        self.temporary_directory.cleanup()

    def git(self, *arguments: str) -> str:
        result = subprocess.run(
            ["git", *arguments],
            cwd=self.path,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        return result.stdout.strip()

    def write_version(self, version: str, lock_version: str | None = None) -> None:
        manifest = self.path / "crates/lg-buddy/Cargo.toml"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            f'[package]\nname = "lg-buddy"\nversion = "{version}"\n',
            encoding="utf-8",
        )
        (self.path / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "lg-buddy"\n'
            f'version = "{lock_version or version}"\n',
            encoding="utf-8",
        )

    def candidate(self, version: str, lock_version: str | None = None) -> str:
        self.git("switch", "dev")
        self.write_version(version, lock_version)
        self.git("add", ".")
        self.git("commit", "-m", f"prepare {version}")
        return self.git("rev-parse", "HEAD")

    def validate(self, target: str, head_sha: str) -> dict[str, object]:
        base_sha = self.git("rev-parse", target)
        return validate_promotion(
            self.path,
            target=target,
            head_ref=head_sha,
            base_sha=base_sha,
            main_ref="main",
            prerelease_ref="prerelease",
            dev_ref="dev",
        )


class SemVerTests(unittest.TestCase):
    def test_semver_precedence(self) -> None:
        ordered = [
            "1.4.0-alpha",
            "1.4.0-alpha.1",
            "1.4.0-beta.2",
            "1.4.0-beta.10",
            "1.4.0-rc.1",
            "1.4.0",
            "1.5.0",
        ]
        self.assertEqual(sorted(ordered, key=SemVer.parse), ordered)

    def test_invalid_semver_is_rejected(self) -> None:
        for value in ("1.4", "v1.4.0", "1.04.0", "1.4.0-beta.01"):
            with self.subTest(value=value), self.assertRaises(PromotionError):
                SemVer.parse(value)


class PromotionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.repository = RepositoryFixture()

    def tearDown(self) -> None:
        self.repository.close()

    def test_prerelease_promotion(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")

        result = self.repository.validate("prerelease", head)

        self.assertEqual(result["tag"], "v1.4.0-beta.1")
        self.assertEqual(result["channel"], "prerelease")
        self.assertFalse(result["retry"])

    def test_stable_promotion_after_prerelease(self) -> None:
        prerelease = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("branch", "-f", "prerelease", prerelease)
        self.repository.write_version("1.4.0")
        self.repository.git("add", ".")
        self.repository.git("commit", "-m", "prepare stable")
        head = self.repository.git("rev-parse", "HEAD")

        result = validate_promotion(
            self.repository.path,
            target="main",
            head_ref=head,
            base_sha=self.repository.stable_sha,
            main_ref="main",
            prerelease_ref="prerelease",
            dev_ref="dev",
        )

        self.assertEqual(result["tag"], "v1.4.0")
        self.assertEqual(result["channel"], "stable")

    def test_channel_mismatch_is_rejected(self) -> None:
        prerelease = self.repository.candidate("1.4.0-beta.1")
        with self.assertRaisesRegex(PromotionError, "main requires a stable version"):
            self.repository.validate("main", prerelease)

    def test_lockfile_mismatch_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1", "1.3.0")
        with self.assertRaisesRegex(PromotionError, "does not match Cargo.lock"):
            self.repository.validate("prerelease", head)

    def test_stale_target_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        with self.assertRaisesRegex(PromotionError, "promotion target moved"):
            validate_promotion(
                self.repository.path,
                target="prerelease",
                head_ref=head,
                base_sha=head,
                main_ref="main",
                prerelease_ref="prerelease",
                dev_ref="dev",
            )

    def test_diverged_prerelease_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("switch", "prerelease")
        (self.repository.path / "diverged").write_text("diverged\n", encoding="utf-8")
        self.repository.git("add", "diverged")
        self.repository.git("commit", "-m", "diverge")

        with self.assertRaisesRegex(PromotionError, "is not an ancestor"):
            self.repository.validate("prerelease", head)

    def test_matching_existing_tag_is_an_idempotent_retry(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("tag", "v1.4.0-beta.1", head)

        result = self.repository.validate("prerelease", head)

        self.assertTrue(result["retry"])

    def test_conflicting_existing_tag_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("tag", "v1.4.0-beta.1", self.repository.stable_sha)

        with self.assertRaisesRegex(PromotionError, "already points"):
            self.repository.validate("prerelease", head)


if __name__ == "__main__":
    unittest.main()
