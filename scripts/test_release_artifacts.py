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

    def test_restore_verified_inputs_from_a_separate_artifact_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            checkout, downloaded = Path(directory) / "checkout", Path(directory) / "downloaded"
            checkout.mkdir()
            downloaded.mkdir()
            (downloaded / "metadata.json").write_text(
                json.dumps({"commit": "verified", "rustc": "rustc 1.95.0"}), encoding="utf-8"
            )
            (downloaded / "Cargo.lock").write_bytes(b"verified lockfile")
            with patch("release_artifacts.ROOT", checkout), patch(
                "release_artifacts.command", side_effect=("verified", "rustc 1.95.0")
            ):
                release_artifacts.restore(downloaded)
            self.assertEqual((checkout / "Cargo.lock").read_bytes(), b"verified lockfile")
            self.assertFalse((checkout / "metadata.json").exists())

    def test_restore_already_in_checkout_keeps_the_verified_lockfile(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory)
            (destination / "metadata.json").write_text(
                json.dumps({"commit": "verified", "rustc": "rustc 1.95.0"}), encoding="utf-8"
            )
            lockfile = destination / "Cargo.lock"
            lockfile.write_bytes(b"verified lockfile")
            with patch("release_artifacts.ROOT", destination), patch(
                "release_artifacts.command", side_effect=("verified", "rustc 1.95.0")
            ):
                release_artifacts.restore(destination)
            self.assertEqual(lockfile.read_bytes(), b"verified lockfile")


if __name__ == "__main__":
    unittest.main()
