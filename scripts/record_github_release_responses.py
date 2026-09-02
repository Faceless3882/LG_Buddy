#!/usr/bin/env python3

"""Record sanitized GitHub release responses for deterministic update mocks."""

from __future__ import annotations

import argparse
import json
import os
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


API_VERSION = "2026-03-10"


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def sanitize_url(value: str) -> str:
    parsed = urllib.parse.urlsplit(value)
    host = parsed.hostname or ""
    if ":" in host:
        host = f"[{host}]"
    if parsed.port is not None:
        host = f"{host}:{parsed.port}"
    return urllib.parse.urlunsplit((parsed.scheme, host, parsed.path, "", ""))


def sanitize_location(value: str) -> dict[str, Any]:
    parsed = urllib.parse.urlsplit(value)
    return {
        "scheme": parsed.scheme,
        "host": parsed.hostname,
        "port": parsed.port,
        "path": parsed.path,
        "query_present": bool(parsed.query),
        "fragment_present": bool(parsed.fragment),
    }


def project_asset(asset: dict[str, Any]) -> dict[str, Any]:
    projected = {
        field: asset.get(field)
        for field in (
            "id",
            "name",
            "state",
            "size",
            "digest",
            "url",
            "browser_download_url",
        )
    }
    for field in ("url", "browser_download_url"):
        if isinstance(projected[field], str):
            projected[field] = sanitize_url(projected[field])
    return projected


def project_release(release: dict[str, Any]) -> dict[str, Any]:
    return {
        "tag_name": release.get("tag_name"),
        "html_url": sanitize_url(release["html_url"]),
        "draft": release.get("draft"),
        "prerelease": release.get("prerelease"),
        "assets": [project_asset(asset) for asset in release.get("assets", [])],
    }


def project_git_object(response: dict[str, Any]) -> dict[str, Any]:
    object_data = response.get("object", {})
    return {
        "object": {
            "type": object_data.get("type"),
            "sha": object_data.get("sha"),
        }
    }


class GitHubRecorder:
    def __init__(self, *, api_root: str, token: str | None) -> None:
        self.api_root = api_root.rstrip("/")
        self.token = token
        self.opener = urllib.request.build_opener(NoRedirect)

    def request(self, url: str, *, accept: str) -> tuple[int, Any, dict[str, str]]:
        headers = {
            "Accept": accept,
            "User-Agent": "lg-buddy-release-response-recorder",
            "X-GitHub-Api-Version": API_VERSION,
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(url, headers=headers)
        try:
            response = self.opener.open(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        body = response.read()
        response_headers = {
            key.lower(): value for key, value in response.headers.items()
        }
        parsed: Any = None
        if body:
            content_type = response_headers.get("content-type", "")
            if "json" in content_type:
                parsed = json.loads(body)
            else:
                parsed = {"bytes": len(body)}
        return response.status, parsed, response_headers

    def api_json(self, path: str) -> tuple[Any, dict[str, Any]]:
        status, body, headers = self.request(
            f"{self.api_root}/{path.lstrip('/')}",
            accept="application/vnd.github+json",
        )
        if status != 200:
            raise RuntimeError(f"GitHub API request for {path} returned {status}")
        return body, {
            "status": status,
            "etag": headers.get("etag"),
            "content_type": headers.get("content-type"),
        }

    def asset_redirect(self, asset: dict[str, Any]) -> dict[str, Any]:
        status, _, headers = self.request(
            str(asset["url"]),
            accept="application/octet-stream",
        )
        location = headers.get("location")
        return {
            "asset": project_asset(asset),
            "response": {
                "status": status,
                "content_type": headers.get("content-type"),
                "content_length": headers.get("content-length"),
                "location": sanitize_location(location) if location else None,
            },
        }


def record_responses(
    *, recorder: GitHubRecorder, repository: str, tag: str, target: str
) -> dict[str, Any]:
    encoded_repository = "/".join(
        urllib.parse.quote(part, safe="") for part in repository.split("/")
    )
    encoded_tag = urllib.parse.quote(tag, safe="")
    releases, releases_response = recorder.api_json(
        f"repos/{encoded_repository}/releases?per_page=1"
    )
    latest_release = (
        releases[0] if isinstance(releases, list) and len(releases) == 1 else None
    )
    if not isinstance(latest_release, dict) or latest_release.get("tag_name") != tag:
        raise RuntimeError(
            f"newly published release {tag} is not GitHub's newest release"
        )
    release, release_response = recorder.api_json(
        f"repos/{encoded_repository}/releases/tags/{encoded_tag}"
    )
    tag_ref, tag_ref_response = recorder.api_json(
        f"repos/{encoded_repository}/git/ref/tags/{encoded_tag}"
    )

    version = tag.removeprefix("v")
    expected_assets = {
        f"lg-buddy-{version}-{target}.tar.gz",
        "sha256sums.txt",
    }
    selected_assets = [
        asset
        for asset in release.get("assets", [])
        if asset.get("name") in expected_assets
    ]
    if {asset.get("name") for asset in selected_assets} != expected_assets:
        raise RuntimeError(f"release {tag} does not expose the expected upgrade assets")

    annotated_tag = None
    if tag_ref.get("object", {}).get("type") == "tag":
        tag_sha = urllib.parse.quote(str(tag_ref["object"]["sha"]), safe="")
        tag_object, tag_object_response = recorder.api_json(
            f"repos/{encoded_repository}/git/tags/{tag_sha}"
        )
        annotated_tag = {
            "response": tag_object_response,
            "body": project_git_object(tag_object),
        }

    return {
        "schema_version": 1,
        "repository": repository,
        "tag": tag,
        "release_list": {
            "response": releases_response,
            "body": [project_release(item) for item in releases],
        },
        "release_by_tag": {
            "response": release_response,
            "body": project_release(release),
        },
        "tag_ref": {
            "response": tag_ref_response,
            "body": project_git_object(tag_ref),
        },
        "annotated_tag": annotated_tag,
        "asset_redirects": [
            recorder.asset_redirect(asset) for asset in selected_assets
        ],
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target", default="x86_64-unknown-linux-musl")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--api-root", default="https://api.github.com")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    observation = record_responses(
        recorder=GitHubRecorder(api_root=args.api_root, token=token),
        repository=args.repository,
        tag=args.tag,
        target=args.target,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(observation, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(f"Recorded sanitized GitHub responses in {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
