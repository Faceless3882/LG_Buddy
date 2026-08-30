#!/usr/bin/env python3

"""Validate an LG Buddy release promotion against the persistent branch contract."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import tomllib
from dataclasses import dataclass
from functools import total_ordering
from pathlib import Path


MANIFEST_PATH = "crates/lg-buddy/Cargo.toml"
LOCK_PATH = "Cargo.lock"
SEMVER_RE = re.compile(
    r"^(?P<major>0|[1-9][0-9]*)\."
    r"(?P<minor>0|[1-9][0-9]*)\."
    r"(?P<patch>0|[1-9][0-9]*)"
    r"(?:-(?P<prerelease>(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*))?"
    r"(?:\+(?P<build>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$"
)


class PromotionError(RuntimeError):
    pass


@total_ordering
@dataclass(frozen=True)
class SemVer:
    major: int
    minor: int
    patch: int
    prerelease: tuple[str, ...]
    build: tuple[str, ...]

    @classmethod
    def parse(cls, value: str) -> "SemVer":
        match = SEMVER_RE.fullmatch(value)
        if match is None:
            raise PromotionError(f"invalid semantic version: {value}")

        prerelease = tuple((match.group("prerelease") or "").split("."))
        build = tuple((match.group("build") or "").split("."))
        return cls(
            int(match.group("major")),
            int(match.group("minor")),
            int(match.group("patch")),
            tuple(identifier for identifier in prerelease if identifier),
            tuple(identifier for identifier in build if identifier),
        )

    def _prerelease_key(self) -> tuple[object, ...]:
        if not self.prerelease:
            return (1,)

        identifiers: list[tuple[int, object]] = []
        for identifier in self.prerelease:
            if identifier.isdigit():
                identifiers.append((0, int(identifier)))
            else:
                identifiers.append((1, identifier))
        return (0, *identifiers)

    def _precedence_key(self) -> tuple[object, ...]:
        return (self.major, self.minor, self.patch, self._prerelease_key())

    def __lt__(self, other: object) -> bool:
        if not isinstance(other, SemVer):
            return NotImplemented
        return self._precedence_key() < other._precedence_key()

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, SemVer):
            return NotImplemented
        return self._precedence_key() == other._precedence_key()


def git(repository: Path, *arguments: str, check: bool = True) -> str:
    result = subprocess.run(
        ["git", *arguments],
        cwd=repository,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip()
        raise PromotionError(f"git {' '.join(arguments)} failed: {detail}")
    return result.stdout.strip()


def object_at_ref(repository: Path, ref: str, path: str) -> bytes:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=repository,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        detail = result.stderr.decode(errors="replace").strip()
        raise PromotionError(f"cannot read {path} at {ref}: {detail}")
    return result.stdout


def package_version_at_ref(repository: Path, ref: str) -> str:
    manifest = tomllib.loads(object_at_ref(repository, ref, MANIFEST_PATH).decode())
    try:
        version = manifest["package"]["version"]
    except (KeyError, TypeError) as error:
        raise PromotionError(f"missing package.version in {MANIFEST_PATH} at {ref}") from error
    if not isinstance(version, str):
        raise PromotionError(f"package.version in {MANIFEST_PATH} at {ref} is not a string")
    SemVer.parse(version)
    return version


def lock_version_at_ref(repository: Path, ref: str) -> str:
    lock = tomllib.loads(object_at_ref(repository, ref, LOCK_PATH).decode())
    matches = [
        package.get("version")
        for package in lock.get("package", [])
        if package.get("name") == "lg-buddy"
    ]
    if len(matches) != 1 or not isinstance(matches[0], str):
        raise PromotionError(f"expected exactly one lg-buddy package in {LOCK_PATH} at {ref}")
    SemVer.parse(matches[0])
    return matches[0]


def resolve(repository: Path, ref: str) -> str:
    return git(repository, "rev-parse", "--verify", f"{ref}^{{commit}}")


def require_ancestor(repository: Path, ancestor: str, descendant: str) -> None:
    result = subprocess.run(
        ["git", "merge-base", "--is-ancestor", ancestor, descendant],
        cwd=repository,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    if result.returncode == 1:
        raise PromotionError(f"{ancestor} is not an ancestor of {descendant}")
    if result.returncode != 0:
        raise PromotionError(result.stderr.strip() or "git merge-base failed")


def validate_promotion(
    repository: Path,
    *,
    target: str,
    head_ref: str,
    base_sha: str,
    main_ref: str,
    prerelease_ref: str,
    dev_ref: str,
) -> dict[str, object]:
    if target not in {"main", "prerelease"}:
        raise PromotionError(f"unsupported promotion target: {target}")

    head_sha = resolve(repository, head_ref)
    dev_sha = resolve(repository, dev_ref)
    main_sha = resolve(repository, main_ref)
    prerelease_sha = resolve(repository, prerelease_ref)
    target_sha = main_sha if target == "main" else prerelease_sha

    if head_sha != dev_sha:
        raise PromotionError(f"reviewed head {head_sha} is not the current dev commit {dev_sha}")
    if target_sha != resolve(repository, base_sha):
        raise PromotionError(f"promotion target moved from reviewed base {base_sha} to {target_sha}")

    require_ancestor(repository, main_sha, prerelease_sha)
    require_ancestor(repository, prerelease_sha, head_sha)

    version_text = package_version_at_ref(repository, head_sha)
    lock_version = lock_version_at_ref(repository, head_sha)
    if lock_version != version_text:
        raise PromotionError(
            f"crate version {version_text} does not match Cargo.lock version {lock_version}"
        )

    version = SemVer.parse(version_text)
    if version.build:
        raise PromotionError("official release versions must not contain build metadata")
    if target == "main" and version.prerelease:
        raise PromotionError(f"main requires a stable version, found {version_text}")
    if target == "prerelease" and not version.prerelease:
        raise PromotionError(f"prerelease requires a prerelease version, found {version_text}")

    current_main_text = package_version_at_ref(repository, main_sha)
    current_prerelease_text = package_version_at_ref(repository, prerelease_sha)
    for channel, current_text in (
        ("main", current_main_text),
        ("prerelease", current_prerelease_text),
    ):
        if version <= SemVer.parse(current_text):
            raise PromotionError(
                f"candidate {version_text} must advance {channel} from {current_text}"
            )

    tag = f"v{version_text}"
    tag_result = subprocess.run(
        ["git", "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}"],
        cwd=repository,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    retry = tag_result.returncode == 0
    if retry and tag_result.stdout.strip() != head_sha:
        raise PromotionError(
            f"tag {tag} already points to {tag_result.stdout.strip()}, not {head_sha}"
        )

    return {
        "version": version_text,
        "tag": tag,
        "channel": "stable" if target == "main" else "prerelease",
        "head_sha": head_sha,
        "base_sha": target_sha,
        "main_sha": main_sha,
        "prerelease_sha": prerelease_sha,
        "retry": retry,
    }


def write_github_output(path: Path, result: dict[str, object]) -> None:
    with path.open("a", encoding="utf-8") as output:
        for key, value in result.items():
            rendered = str(value).lower() if isinstance(value, bool) else str(value)
            output.write(f"{key}={rendered}\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", type=Path, default=Path.cwd())
    parser.add_argument("--target", required=True, choices=("main", "prerelease"))
    parser.add_argument("--head-ref", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--main-ref", default="refs/remotes/origin/main")
    parser.add_argument("--prerelease-ref", default="refs/remotes/origin/prerelease")
    parser.add_argument("--dev-ref", default="refs/remotes/origin/dev")
    parser.add_argument("--github-output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        result = validate_promotion(
            args.repository.resolve(),
            target=args.target,
            head_ref=args.head_ref,
            base_sha=args.base_sha,
            main_ref=args.main_ref,
            prerelease_ref=args.prerelease_ref,
            dev_ref=args.dev_ref,
        )
    except PromotionError as error:
        raise SystemExit(f"release promotion validation failed: {error}") from error

    if args.github_output is not None:
        write_github_output(args.github_output, result)
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
