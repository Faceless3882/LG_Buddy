#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import io
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

from release_bundle_manifest import MANIFEST_NAME, ReleaseIdentity, render_manifest


VERSION = "9.8.7-beta.1"
TAG = f"v{VERSION}"
TARGET = "x86_64-unknown-linux-musl"


FAKE_GH = r"""#!/usr/bin/env python3
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

state_path = Path(os.environ["FAKE_GH_STATE"])
asset_dir = Path(os.environ["FAKE_GH_ASSETS"])
log_path = Path(os.environ["FAKE_GH_LOG"])
args = sys.argv[1:]

with log_path.open("a", encoding="utf-8") as log:
    log.write(json.dumps(args) + "\n")

state = json.loads(state_path.read_text(encoding="utf-8"))

def save():
    state_path.write_text(json.dumps(state), encoding="utf-8")

if len(args) < 3 or args[0] != "release":
    sys.exit("unsupported fake gh invocation")

command = args[1]
tag = args[2]
rest = args[3:]

if command == "view":
    if not state["exists"]:
        sys.exit(1)
    if "--json" not in rest:
        sys.exit(0)
    fields = rest[rest.index("--json") + 1]
    if fields == "assets":
        if "--jq" in rest:
            print("\n".join(sorted(state["assets"])))
        else:
            assets = []
            for name in sorted(state["assets"]):
                content = (asset_dir / name).read_bytes()
                assets.append({
                    "name": name,
                    "state": "uploaded",
                    "size": len(content),
                    "digest": f"sha256:{hashlib.sha256(content).hexdigest()}",
                })
            print(json.dumps({"assets": assets}))
    else:
        print(json.dumps({
            "isDraft": state["draft"],
            "isPrerelease": state["prerelease"],
        }))
    sys.exit(0)

if command == "create":
    if state["exists"]:
        sys.exit("release already exists")
    state.update({
        "exists": True,
        "draft": "--draft" in rest,
        "prerelease": "--prerelease" in rest,
        "assets": [],
    })
    save()
    sys.exit(0)

if not state["exists"]:
    sys.exit("release does not exist")

if command == "upload":
    source = Path(rest[0])
    if os.environ.get("FAKE_GH_FAIL_UPLOAD") == source.name:
        sys.exit("injected upload failure")
    asset_dir.mkdir(parents=True, exist_ok=True)
    if os.environ.get("FAKE_GH_CORRUPT_UPLOAD") == source.name:
        content = bytearray(source.read_bytes())
        content[0] ^= 1
        (asset_dir / source.name).write_bytes(content)
    else:
        shutil.copyfile(source, asset_dir / source.name)
    if source.name not in state["assets"]:
        state["assets"].append(source.name)
    save()
    sys.exit(0)

if command == "download":
    pattern = rest[rest.index("--pattern") + 1]
    destination = Path(rest[rest.index("--dir") + 1])
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(asset_dir / pattern, destination / pattern)
    sys.exit(0)

if command == "edit":
    for argument in rest:
        if argument.startswith("--draft="):
            state["draft"] = argument.split("=", 1)[1] == "true"
        elif argument.startswith("--prerelease="):
            state["prerelease"] = argument.split("=", 1)[1] == "true"
    save()
    sys.exit(0)

sys.exit("unsupported fake gh release command")
"""


class PublishReleaseAssetsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.repository = self.root / "repository"
        self.repository.mkdir()
        self.dist = self.repository / "dist"
        self.dist.mkdir()
        self.fake_bin = self.root / "bin"
        self.fake_bin.mkdir()
        self.remote_assets = self.root / "remote-assets"
        self.state_path = self.root / "state.json"
        self.log_path = self.root / "gh-calls.jsonl"

        self.git("init", "--quiet")
        self.git("config", "user.name", "Release test")
        self.git("config", "user.email", "release-test@example.invalid")
        (self.repository / "marker").write_text("release\n", encoding="utf-8")
        self.git("add", "marker")
        self.git("commit", "--quiet", "-m", "release")
        self.commit = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("tag", TAG)

        identity = ReleaseIdentity(
            release_tag=TAG,
            version=VERSION,
            channel="prerelease",
            target=TARGET,
            commit=self.commit,
        )
        bundle_name = f"lg-buddy-{VERSION}-{TARGET}"
        self.archive = self.dist / f"{bundle_name}.tar.gz"
        manifest = render_manifest(identity)
        with tarfile.open(self.archive, mode="w:gz") as bundle:
            info = tarfile.TarInfo(f"{bundle_name}/{MANIFEST_NAME}")
            info.size = len(manifest)
            bundle.addfile(info, io.BytesIO(manifest))

        digest = hashlib.sha256(self.archive.read_bytes()).hexdigest()
        self.checksums = self.dist / "sha256sums.txt"
        self.checksums.write_text(
            f"{digest}  {self.archive.name}\n", encoding="utf-8"
        )

        fake_gh = self.fake_bin / "gh"
        fake_gh.write_text(FAKE_GH, encoding="utf-8")
        fake_gh.chmod(0o755)
        self.write_state()

        self.environment = os.environ.copy()
        self.environment.update(
            {
                "PATH": f"{self.fake_bin}:{self.environment['PATH']}",
                "FAKE_GH_STATE": str(self.state_path),
                "FAKE_GH_ASSETS": str(self.remote_assets),
                "FAKE_GH_LOG": str(self.log_path),
            }
        )
        self.publisher = Path(__file__).with_name("publish-release-assets.sh")

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def git(self, *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=self.repository,
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )

    def write_state(
        self,
        *,
        exists: bool = False,
        draft: bool = True,
        prerelease: bool = True,
        assets: dict[str, bytes] | None = None,
    ) -> None:
        asset_values = assets or {}
        self.remote_assets.mkdir(parents=True, exist_ok=True)
        for name, content in asset_values.items():
            (self.remote_assets / name).write_bytes(content)
        self.state_path.write_text(
            json.dumps(
                {
                    "exists": exists,
                    "draft": draft,
                    "prerelease": prerelease,
                    "assets": list(asset_values),
                }
            ),
            encoding="utf-8",
        )

    def run_publisher(
        self,
        *,
        fail_upload: str | None = None,
        corrupt_upload: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = self.environment.copy()
        if fail_upload is not None:
            environment["FAKE_GH_FAIL_UPLOAD"] = fail_upload
        if corrupt_upload is not None:
            environment["FAKE_GH_CORRUPT_UPLOAD"] = corrupt_upload
        return subprocess.run(
            [
                "bash",
                self.publisher,
                "--dist-dir",
                self.dist,
                "--tag",
                TAG,
                "--commit",
                self.commit,
            ],
            cwd=self.repository,
            env=environment,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def state(self) -> dict[str, object]:
        return json.loads(self.state_path.read_text(encoding="utf-8"))

    def calls(self) -> list[list[str]]:
        if not self.log_path.exists():
            return []
        return [
            json.loads(line)
            for line in self.log_path.read_text(encoding="utf-8").splitlines()
        ]

    def mutations(self, command: str) -> list[list[str]]:
        return [call for call in self.calls() if call[:2] == ["release", command]]

    def complete_assets(self) -> dict[str, bytes]:
        return {
            self.archive.name: self.archive.read_bytes(),
            self.checksums.name: self.checksums.read_bytes(),
        }

    def test_new_release_is_published_only_after_assets_are_uploaded(self) -> None:
        result = self.run_publisher()

        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(set(self.state()["assets"]), set(self.complete_assets()))
        self.assertFalse(self.state()["draft"])
        create = self.mutations("create")
        uploads = self.mutations("upload")
        edit = self.mutations("edit")
        self.assertEqual(len(create), 1)
        self.assertIn("--draft", create[0])
        self.assertEqual(len(uploads), 2)
        self.assertEqual(len(edit), 1)
        self.assertIn("--draft=false", edit[0])
        self.assertLess(self.calls().index(create[0]), self.calls().index(uploads[0]))
        self.assertLess(self.calls().index(uploads[-1]), self.calls().index(edit[0]))

    def test_retry_resumes_a_partially_uploaded_draft(self) -> None:
        first = self.run_publisher(fail_upload=self.checksums.name)
        self.assertNotEqual(first.returncode, 0)
        self.assertTrue(self.state()["draft"])

        second = self.run_publisher()

        self.assertEqual(second.returncode, 0, second.stdout)
        self.assertFalse(self.state()["draft"])
        self.assertEqual(len(self.mutations("create")), 1)
        archive_uploads = [
            call for call in self.mutations("upload") if call[-1] == str(self.archive)
        ]
        self.assertEqual(len(archive_uploads), 1)
        self.assertEqual(len(self.mutations("edit")), 1)

    def test_unexpected_draft_asset_blocks_publication_without_mutation(self) -> None:
        self.write_state(exists=True, assets={"unexpected.txt": b"unexpected"})

        result = self.run_publisher()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("contains unexpected asset", result.stdout)
        self.assertTrue(self.state()["draft"])
        self.assertEqual(self.mutations("upload"), [])
        self.assertEqual(self.mutations("edit"), [])

    def test_published_release_missing_asset_is_not_modified(self) -> None:
        self.write_state(
            exists=True,
            draft=False,
            assets={self.archive.name: self.archive.read_bytes()},
        )

        result = self.run_publisher()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("is missing required asset", result.stdout)
        self.assertEqual(self.mutations("upload"), [])
        self.assertEqual(self.mutations("edit"), [])

    def test_mismatched_draft_asset_blocks_publication(self) -> None:
        self.write_state(
            exists=True,
            assets={self.archive.name: b"different archive"},
        )

        result = self.run_publisher()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"Release asset {self.archive.name} has size", result.stdout)
        self.assertTrue(self.state()["draft"])
        self.assertEqual(self.mutations("edit"), [])

    def test_successful_but_corrupted_upload_blocks_publication(self) -> None:
        result = self.run_publisher(corrupt_upload=self.checksums.name)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(f"Release asset {self.checksums.name} has digest", result.stdout)
        self.assertTrue(self.state()["draft"])
        self.assertEqual(self.mutations("edit"), [])

    def test_complete_published_release_is_verification_only(self) -> None:
        self.write_state(exists=True, draft=False, assets=self.complete_assets())

        result = self.run_publisher()

        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.mutations("create"), [])
        self.assertEqual(self.mutations("upload"), [])
        self.assertEqual(self.mutations("edit"), [])


if __name__ == "__main__":
    unittest.main()
