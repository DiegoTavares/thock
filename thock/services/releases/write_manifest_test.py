#!/usr/bin/env python3
"""Tests for write-manifest. Run with: python3 write_manifest_test.py"""

import hashlib
import json
import os
import subprocess
import tempfile
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "write-manifest")


def run_writer(tag, artifact_dir, bucket="thock-releases"):
    env = dict(os.environ, BUCKET=bucket) if bucket else {
        key: value for key, value in os.environ.items() if key != "BUCKET"
    }
    return subprocess.run(
        [SCRIPT, tag, artifact_dir],
        capture_output=True,
        text=True,
        env=env,
    )


class WriteManifestTest(unittest.TestCase):
    def setUp(self):
        self.artifact_dir = tempfile.mkdtemp()

    def write_artifact(self, name, content=b""):
        with open(os.path.join(self.artifact_dir, name), "wb") as artifact:
            artifact.write(content)

    def test_maps_platforms_and_hashes(self):
        self.write_artifact("Thock-aarch64.dmg", b"dmg bytes")
        self.write_artifact("thock-linux-x86_64.tar.gz", b"tar bytes")

        result = run_writer("v1.16.0", self.artifact_dir)
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads(result.stdout)

        self.assertEqual(manifest["version"], "1.16.0")
        self.assertEqual(
            manifest["notes_url"],
            "https://github.com/DiegoTavares/thock/releases/tag/v1.16.0",
        )
        self.assertTrue(manifest["released_at"].endswith("Z"))

        by_platform = {(a["os"], a["arch"]): a for a in manifest["assets"]}
        self.assertEqual(
            set(by_platform), {("macos", "aarch64"), ("linux", "x86_64")}
        )

        macos = by_platform[("macos", "aarch64")]
        self.assertEqual(macos["asset"], "thock")
        self.assertEqual(
            macos["url"],
            "https://storage.googleapis.com/thock-releases/dist/v1.16.0/Thock-aarch64.dmg",
        )
        self.assertEqual(macos["sha256"], hashlib.sha256(b"dmg bytes").hexdigest())
        linux = by_platform[("linux", "x86_64")]
        self.assertEqual(linux["sha256"], hashlib.sha256(b"tar bytes").hexdigest())

    def test_unrecognized_artifact_is_a_hard_error(self):
        self.write_artifact("Thock-aarch64.dmg")
        self.write_artifact("thock-remote-server-linux-x86_64.gz")

        result = run_writer("v1.16.0", self.artifact_dir)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unrecognized artifact", result.stderr)

    def test_empty_artifact_dir_fails(self):
        result = run_writer("v1.16.0", self.artifact_dir)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no artifacts", result.stderr)

    def test_bad_tag_fails(self):
        self.write_artifact("Thock-aarch64.dmg")
        result = run_writer("1.16.0", self.artifact_dir)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("version tag", result.stderr)

    def test_missing_bucket_fails(self):
        self.write_artifact("Thock-aarch64.dmg")
        result = run_writer("v1.16.0", self.artifact_dir, bucket=None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BUCKET", result.stderr)


if __name__ == "__main__":
    unittest.main()
