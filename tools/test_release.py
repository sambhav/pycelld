"""Exercise publication retries and ordering without writing to GitHub."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import release


class ReleaseTests(unittest.TestCase):
    def test_versions_track_upstream_and_use_numeric_build_numbers(self):
        self.assertEqual(release.release_version("0.4.1", 4), "0.4.1-pycelld.4")
        self.assertEqual(release.release_version("0.4.2", 12), "0.4.2-pycelld.12")
        self.assertLess(release.release_number("v0.4.1-pycelld.9"), release.release_number("v0.4.1-pycelld.10"))
        self.assertEqual(release.release_number("v0.4.1-pycelld.f4e42ce086d2"), 0)
        for upstream, number in [("0.04.1", 1), ("0.4.1-rc.1", 1), ("0.4.1", 0), ("0.4.1", "01")]:
            with self.subTest(upstream=upstream, number=number), self.assertRaises(ValueError):
                release.release_version(upstream, number)

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.assets = Path(self.tmp.name)
        self.source = "a" * 40
        self.tag = "v0.4.1-pycelld.4"
        build = dict(version=self.tag[1:], upstream_version="0.4.1", build_number=4,
                     source_sha=self.source, upstream_revision="b" * 40)
        (self.assets / "BUILD_INFO.json").write_text(json.dumps([build]))
        (self.assets / "celld-test.gz").write_bytes(b"test archive")
        self.existing = None
        self.ref = None
        self.commit = self.source
        self.latest = None
        self.commands = []
        self.enterContext(patch.dict(os.environ, GH_REPO="owner/pycelld", SOURCE_SHA=self.source, GITHUB_RUN_NUMBER="4"))
        self.enterContext(patch.object(release, "api", side_effect=self.api))
        self.enterContext(patch.object(release, "gh", side_effect=self.gh))

    def gh(self, *args):
        self.commands.append(args)
        if args[:2] == ("release", "upload"):
            self.existing = self.published(draft=True)

    def api(self, path, **kwargs):
        return {
            f"releases/tags/{self.tag}": self.existing,
            f"git/ref/tags/{self.tag}": self.ref,
            f"commits/{self.tag}": {"sha": self.commit},
            "releases/latest": self.latest,
        }[path]

    def published(self, *, draft=False):
        return dict(target_commitish=self.source, draft=draft, prerelease=False,
                    assets=[dict(name=p.name, digest="sha256:" + hashlib.sha256(p.read_bytes()).hexdigest())
                            for p in sorted(self.assets.iterdir())])

    def test_first_release_is_published_and_promoted(self):
        release.publish(self.assets)
        create, promote = self.commands
        self.assertEqual(create[:3], ("release", "create", self.tag))
        self.assertIn("--latest=false", create)
        self.assertNotIn("--draft", create)
        self.assertNotIn("--prerelease", create)
        self.assertIn(self.source, create)
        self.assertEqual(promote, ("release", "edit", self.tag, "--latest"))

    def test_older_build_finishing_later_cannot_replace_latest(self):
        self.latest = {"tag_name": "v0.4.2-pycelld.12"}
        release.publish(self.assets)
        self.assertEqual(len(self.commands), 1)
        self.assertEqual(self.commands[0][1], "create")

    def test_rerun_does_not_change_a_published_release(self):
        self.existing, self.ref = self.published(), {"object": {"sha": self.source}}
        self.latest = {"tag_name": self.tag}
        release.publish(self.assets)
        self.assertEqual(self.commands, [])

    def test_retry_can_finish_latest_promotion_without_reuploading(self):
        self.existing, self.ref = self.published(), {"object": {"sha": self.source}}
        release.publish(self.assets)
        self.assertEqual(self.commands, [("release", "edit", self.tag, "--latest")])

    def test_published_asset_mismatch_fails_without_mutation(self):
        self.existing, self.ref = self.published(), {"object": {"sha": self.source}}
        (self.assets / "celld-test.gz").write_bytes(b"different rebuild")
        with self.assertRaisesRegex(ValueError, "never be overwritten"):
            release.publish(self.assets)
        self.assertEqual(self.commands, [])

    def test_conflicting_tag_fails_before_publication(self):
        self.ref, self.commit = {"object": {}}, "c" * 40
        with self.assertRaisesRegex(ValueError, "different source commit"):
            release.publish(self.assets)
        self.assertEqual(self.commands, [])

    def test_interrupted_draft_upload_can_resume(self):
        self.existing = self.published(draft=True)
        self.existing["assets"] = self.existing["assets"][:1]
        self.latest = {"tag_name": "v0.4.1-pycelld.5"}
        release.publish(self.assets)
        self.assertEqual(self.commands[0][:3], ("release", "upload", self.tag))
        self.assertIn("--clobber", self.commands[0])
        self.assertEqual(self.commands[1], ("release", "edit", self.tag, "--draft=false", "--prerelease=false", "--latest=false"))

    def test_foreign_draft_is_not_modified(self):
        self.existing = self.published(draft=True)
        self.existing["target_commitish"] = "c" * 40
        with self.assertRaisesRegex(ValueError, "different source commit"):
            release.publish(self.assets)
        self.assertEqual(self.commands, [])

    def test_unexpected_draft_assets_are_not_removed(self):
        self.existing = self.published(draft=True)
        self.existing["assets"].append({"name": "unexpected.txt"})
        with self.assertRaisesRegex(ValueError, "unexpected assets"):
            release.publish(self.assets)
        self.assertEqual(self.commands, [])


class ApiTests(unittest.TestCase):
    def test_only_not_found_means_a_release_does_not_exist(self):
        with patch.dict(os.environ, GH_REPO="owner/pycelld"):
            for status in [404, 403, 500]:
                result = subprocess.CompletedProcess([], 1, "", f"gh: failed (HTTP {status})")
                with self.subTest(status=status), patch.object(subprocess, "run", return_value=result):
                    if status == 404:
                        self.assertIsNone(release.api("releases/latest", missing_ok=True))
                    else:
                        with self.assertRaises(RuntimeError):
                            release.api("releases/latest", missing_ok=True)


if __name__ == "__main__":
    unittest.main()
