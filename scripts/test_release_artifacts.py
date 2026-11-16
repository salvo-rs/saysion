"""Offline regressions for cross-job release artifact verification."""

import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import release_artifacts


class ReleaseArtifactTests(unittest.TestCase):
    def archive(self, path, data, timestamp):
        with tarfile.open(path, "w:gz") as archive:
            entry = tarfile.TarInfo("saysion-0.4.0/src/lib.rs")
            entry.mtime = timestamp
            entry.size = len(data)
            archive.addfile(entry, io.BytesIO(data))

    def test_equivalent_payloads_allow_different_archive_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / "first.crate", Path(directory) / "second.crate"
            self.archive(first, b"same source", 10)
            self.archive(second, b"same source", 20)
            release_artifacts.verify_package(first, second)

    def test_changed_payload_cannot_pass_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            first, second = Path(directory) / "first.crate", Path(directory) / "second.crate"
            self.archive(first, b"verified source", 10)
            self.archive(second, b"different source", 10)
            with self.assertRaisesRegex(RuntimeError, "differs"):
                release_artifacts.verify_package(first, second)

    def test_wrong_commit_or_toolchain_cannot_restore_lockfile(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory)
            (destination / "metadata.json").write_text(
                json.dumps({"commit": "verified", "rustc": "rustc 1.95.0"}), encoding="utf-8"
            )
            for outputs in [("other",), ("verified", "rustc other")]:
                with patch("release_artifacts.command", side_effect=outputs), self.subTest(outputs=outputs):
                    with self.assertRaises(RuntimeError):
                        release_artifacts.restore(destination)


if __name__ == "__main__":
    unittest.main()
