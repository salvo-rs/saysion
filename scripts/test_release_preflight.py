"""Offline release and installation-guide regressions; no registry requests."""

from pathlib import Path
import re
import tomllib
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

import release_preflight

ROOT = Path(__file__).resolve().parents[1]


class RegistryPreflightTests(unittest.TestCase):
    @patch("release_preflight.urlopen")
    def test_existing_version_is_rejected(self, request):
        request.return_value.__enter__.return_value.status = 200
        with self.assertRaisesRegex(RuntimeError, "already exists"):
            release_preflight.ensure_unpublished("saysion", "0.2.0")
        self.assertEqual(request.call_args.args[0].get_method(), "GET")

    @patch("release_preflight.urlopen")
    def test_absent_version_is_available(self, request):
        request.side_effect = HTTPError("registry", 404, "not found", {}, None)
        release_preflight.ensure_unpublished("saysion", "0.4.0")

    @patch("release_preflight.urlopen")
    def test_registry_errors_do_not_authorize_release(self, request):
        for status in (401, 403, 429, 500):
            with self.subTest(status=status):
                request.side_effect = HTTPError("registry", status, "failed", {}, None)
                with self.assertRaisesRegex(RuntimeError, "Cannot confirm"):
                    release_preflight.ensure_unpublished("saysion", "0.4.0")

    @patch("release_preflight.urlopen")
    def test_network_failure_does_not_authorize_release(self, request):
        request.side_effect = URLError("offline")
        with self.assertRaisesRegex(RuntimeError, "Cannot confirm"):
            release_preflight.ensure_unpublished("saysion", "0.4.0")


class InstallationGuideTests(unittest.TestCase):
    def test_release_guides_use_the_registry_version(self):
        package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
        self.assertEqual(package["version"], "0.4.0")
        self.assertEqual(package["rust-version"], "1.95")
        for guide in ("README.md", "docs/en/stores.md", "docs/zh/stores.md", "docs/migration-0.4.md"):
            with self.subTest(guide=guide):
                text = (ROOT / guide).read_text(encoding="utf-8")
                self.assertNotIn("unreleased", text.lower())
                dependency_count = 0
                for block in re.findall(r"```toml\s*\n(.*?)```", text, re.S):
                    dependency = tomllib.loads(block).get("dependencies", {}).get("saysion")
                    if dependency is None:
                        continue
                    dependency_count += 1
                    self.assertIsInstance(dependency, dict)
                    release_series = ".".join(package["version"].split(".")[:2])
                    self.assertEqual(dependency.get("version"), release_series)
                    self.assertNotIn("git", dependency)
                    self.assertNotIn("branch", dependency)
                self.assertGreater(dependency_count, 0)

    def test_consumer_fixture_checks_this_same_release_package(self):
        package = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]
        manifest_path = ROOT / "tests/fixtures/unreleased-consumer/Cargo.toml"
        fixture = tomllib.loads(manifest_path.read_text())
        dependency = fixture["dependencies"]["saysion"]
        self.assertEqual(dependency["version"], f"={package['version']}")
        self.assertEqual((manifest_path.parent / dependency["path"]).resolve(), ROOT)
        self.assertFalse(fixture["package"]["publish"])


if __name__ == "__main__":
    unittest.main()
