#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import tempfile
import unittest
from pathlib import Path

from release_promotion import (
    PromotionError,
    SemVer,
    validate_merged_promotion,
    validate_promotion,
)


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

    def write_version(
        self,
        version: str,
        lock_version: str | None = None,
        gui_version: str | None = None,
        gui_lock_version: str | None = None,
    ) -> None:
        manifest = self.path / "crates/lg-buddy/Cargo.toml"
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(
            f'[package]\nname = "lg-buddy"\nversion = "{version}"\n',
            encoding="utf-8",
        )
        gui_manifest = self.path / "crates/lg-buddy-gui/Cargo.toml"
        gui_manifest.parent.mkdir(parents=True, exist_ok=True)
        gui_manifest.write_text(
            '[package]\nname = "lg-buddy-gui"\n'
            f'version = "{gui_version or version}"\n',
            encoding="utf-8",
        )
        (self.path / "Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "lg-buddy"\n'
            f'version = "{lock_version or version}"\n\n'
            '[[package]]\nname = "lg-buddy-gui"\n'
            f'version = "{gui_lock_version or gui_version or version}"\n',
            encoding="utf-8",
        )

    def candidate(self, version: str, lock_version: str | None = None) -> str:
        self.git("switch", "dev")
        self.write_version(version, lock_version)
        self.git("add", ".")
        self.git("commit", "--allow-empty", "-m", f"prepare {version}")
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

    def merge_promotion(self, target: str) -> tuple[str, str, str]:
        source_sha = self.git("rev-parse", "dev")
        base_sha = self.git("rev-parse", target)
        self.git("switch", target)
        self.git("merge", "--no-ff", "dev", "-m", f"promote dev to {target}")
        return base_sha, source_sha, self.git("rev-parse", "HEAD")

    def validate_merged(
        self, target: str, head_sha: str, base_sha: str
    ) -> dict[str, object]:
        return validate_merged_promotion(
            self.path,
            target=target,
            head_ref=head_sha,
            base_sha=base_sha,
            main_ref="main",
            prerelease_ref="prerelease",
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

    def test_gui_version_mismatch_is_rejected(self) -> None:
        self.repository.git("switch", "dev")
        self.repository.write_version("1.4.0-beta.1", gui_version="1.3.0")
        self.repository.git("add", ".")
        self.repository.git("commit", "-m", "mismatch gui")
        head = self.repository.git("rev-parse", "HEAD")

        with self.assertRaisesRegex(PromotionError, "lg-buddy-gui"):
            self.repository.validate("prerelease", head)

    def test_stable_version_must_strictly_advance_main(self) -> None:
        for version in ("1.3.0", "1.2.9"):
            with self.subTest(version=version):
                head = self.repository.candidate(version)
                with self.assertRaisesRegex(
                    PromotionError, f"candidate {version} must advance main from 1.3.0"
                ):
                    self.repository.validate("main", head)

    def test_prerelease_version_must_strictly_advance_prerelease(self) -> None:
        current = self.repository.candidate("1.4.0-beta.2")
        self.repository.git("branch", "-f", "prerelease", current)

        for version in ("1.4.0-beta.2", "1.4.0-beta.1"):
            with self.subTest(version=version):
                head = self.repository.candidate(version)
                with self.assertRaisesRegex(
                    PromotionError,
                    f"candidate {version} must advance prerelease from 1.4.0-beta.2",
                ):
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

    def test_existing_candidate_tag_is_rejected_before_merge(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("tag", "v1.4.0-beta.1", head)

        with self.assertRaisesRegex(PromotionError, "already exists before promotion merge"):
            self.repository.validate("prerelease", head)

    def test_conflicting_existing_tag_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("tag", "v1.4.0-beta.1", self.repository.stable_sha)

        with self.assertRaisesRegex(PromotionError, "already points"):
            self.repository.validate("prerelease", head)

    def test_merged_prerelease_is_publishable(self) -> None:
        self.repository.candidate("1.4.0-beta.1")
        base, source, merged = self.repository.merge_promotion("prerelease")

        result = self.repository.validate_merged("prerelease", merged, base)

        self.assertTrue(result["publish"])
        self.assertEqual(result["head_sha"], merged)
        self.assertEqual(result["source_sha"], source)
        self.assertEqual(result["tag"], "v1.4.0-beta.1")
        self.assertEqual(result["channel"], "prerelease")

    def test_merged_stable_release_after_prerelease_is_publishable(self) -> None:
        prerelease = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("branch", "-f", "prerelease", prerelease)
        self.repository.write_version("1.4.0")
        self.repository.git("add", ".")
        self.repository.git("commit", "-m", "prepare stable")
        base, source, merged = self.repository.merge_promotion("main")

        result = self.repository.validate_merged("main", merged, base)

        self.assertTrue(result["publish"])
        self.assertEqual(result["source_sha"], source)
        self.assertEqual(result["tag"], "v1.4.0")
        self.assertEqual(result["channel"], "stable")

    def test_stable_alignment_push_to_prerelease_does_not_publish_again(self) -> None:
        prerelease = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("branch", "-f", "prerelease", prerelease)
        self.repository.write_version("1.4.0")
        self.repository.git("add", ".")
        self.repository.git("commit", "-m", "prepare stable")
        _, _, merged = self.repository.merge_promotion("main")
        self.repository.git("branch", "-f", "prerelease", merged)

        result = self.repository.validate_merged("prerelease", merged, prerelease)

        self.assertFalse(result["publish"])
        self.assertEqual(result["head_sha"], merged)
        self.assertEqual(result["base_sha"], prerelease)
        self.assertEqual(result["channel"], "stable")

    def test_release_branch_fast_forward_without_a_merge_is_rejected(self) -> None:
        head = self.repository.candidate("1.4.0-beta.1")
        self.repository.git("branch", "-f", "prerelease", head)

        with self.assertRaisesRegex(PromotionError, "two-parent merge commit"):
            self.repository.validate_merged("prerelease", head, self.repository.stable_sha)

    def test_matching_merged_release_tag_is_an_idempotent_retry(self) -> None:
        self.repository.candidate("1.4.0-beta.1")
        base, _, merged = self.repository.merge_promotion("prerelease")
        self.repository.git("tag", "v1.4.0-beta.1", merged)

        result = self.repository.validate_merged("prerelease", merged, base)

        self.assertTrue(result["retry"])

    def test_merged_release_lockfile_mismatch_is_rejected(self) -> None:
        self.repository.candidate("1.4.0-beta.1", "1.3.0")
        base, _, merged = self.repository.merge_promotion("prerelease")

        with self.assertRaisesRegex(PromotionError, "does not match Cargo.lock"):
            self.repository.validate_merged("prerelease", merged, base)

    def test_merged_release_channel_mismatch_is_rejected(self) -> None:
        self.repository.candidate("1.4.0")
        base, _, merged = self.repository.merge_promotion("prerelease")

        with self.assertRaisesRegex(PromotionError, "requires a prerelease version"):
            self.repository.validate_merged("prerelease", merged, base)

    def test_conflicting_merged_release_tag_is_rejected(self) -> None:
        self.repository.candidate("1.4.0-beta.1")
        base, _, merged = self.repository.merge_promotion("prerelease")
        self.repository.git("tag", "v1.4.0-beta.1", self.repository.stable_sha)

        with self.assertRaisesRegex(PromotionError, "already points"):
            self.repository.validate_merged("prerelease", merged, base)


if __name__ == "__main__":
    unittest.main()
