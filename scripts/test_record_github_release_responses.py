#!/usr/bin/env python3

from __future__ import annotations

import unittest

from record_github_release_responses import (
    API_VERSION,
    GitHubRecorder,
    project_git_object,
    project_release,
    sanitize_location,
)


class StubResponse:
    status = 200
    headers = {"Content-Type": "application/json"}

    @staticmethod
    def read() -> bytes:
        return b"{}"


class RecordingOpener:
    def __init__(self) -> None:
        self.request = None

    def open(self, request, *, timeout):  # type: ignore[no-untyped-def]
        self.request = request
        self.timeout = timeout
        return StubResponse()


class ResponseRecordingTests(unittest.TestCase):
    def test_request_uses_the_production_github_api_version(self) -> None:
        recorder = GitHubRecorder(api_root="https://api.github.test", token=None)
        opener = RecordingOpener()
        recorder.opener = opener

        recorder.request(
            "https://api.github.test/releases",
            accept="application/vnd.github+json",
        )

        self.assertEqual(API_VERSION, "2026-03-10")
        self.assertEqual(
            opener.request.get_header("X-github-api-version"),
            API_VERSION,
        )

    def test_location_record_drops_userinfo_query_and_fragment(self) -> None:
        recorded = sanitize_location(
            "https://user:secret@example.test:8443/releases/asset?token=secret#fragment"
        )

        self.assertEqual(
            recorded,
            {
                "scheme": "https",
                "host": "example.test",
                "port": 8443,
                "path": "/releases/asset",
                "query_present": True,
                "fragment_present": True,
            },
        )
        self.assertNotIn("secret", repr(recorded))

    def test_release_projection_keeps_only_update_client_fields(self) -> None:
        projected = project_release(
            {
                "tag_name": "v1.4.0-beta.2",
                "html_url": "https://user:secret@github.test/releases/v1.4.0-beta.2?token=secret",
                "draft": False,
                "prerelease": True,
                "author": {"login": "ignored"},
                "assets": [
                    {
                        "id": 42,
                        "name": "sha256sums.txt",
                        "state": "uploaded",
                        "size": 123,
                        "digest": "sha256:abc",
                        "url": "https://user:secret@api.github.test/assets/42?token=secret",
                        "browser_download_url": "https://github.test/assets/42#secret",
                        "uploader": {"login": "ignored"},
                    }
                ],
            }
        )

        self.assertNotIn("author", projected)
        self.assertNotIn("uploader", projected["assets"][0])
        self.assertEqual(projected["assets"][0]["id"], 42)
        self.assertNotIn("secret", repr(projected))

    def test_git_object_projection_keeps_only_tag_peeling_fields(self) -> None:
        projected = project_git_object(
            {
                "ref": "refs/tags/v1.4.0-beta.2",
                "node_id": "ignored",
                "object": {
                    "type": "commit",
                    "sha": "77c8f46c66b9e385f3d90c15dee33d775639bbeb",
                    "url": "https://api.github.test/commits/secret",
                },
            }
        )

        self.assertEqual(
            projected,
            {
                "object": {
                    "type": "commit",
                    "sha": "77c8f46c66b9e385f3d90c15dee33d775639bbeb",
                }
            },
        )


if __name__ == "__main__":
    unittest.main()
