"""Test real, pinned Salvo middleware with its saysion dependency aligned.

This stages unchanged upstream Rust source outside the repository. It does not
claim that the upstream 0.2 dependency already accepts 0.4, or publish a package.
"""

import hashlib
import json
from pathlib import Path
import shutil
import subprocess
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
SALVO_REV = "9eacc0f8a5de8de23a151ebdedd3a7c6243eb644"
SOURCE_SHA256 = "2317418b0ced75ab60c2b54a47c928905ac5d65b58387a1f02c5b808a2b370a7"


def cargo_metadata(manifest, *, no_deps=False):
    """Resolve a workspace using the caller's ordinary Cargo configuration."""
    command = ["cargo", "metadata", "--format-version", "1", "--manifest-path", str(manifest)]
    if no_deps:
        command.append("--no-deps")
    return json.loads(subprocess.check_output(command, cwd=ROOT, text=True))


def main():
    target = Path(cargo_metadata(ROOT / "Cargo.toml", no_deps=True)["target_directory"]).resolve()
    stage = target / "salvo-compat-source"
    stage.mkdir(parents=True, exist_ok=True)
    url = f"https://raw.githubusercontent.com/salvo-rs/salvo/{SALVO_REV}/crates/session/src/lib.rs"
    with urlopen(url, timeout=30) as response:
        source = response.read()
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise RuntimeError("Pinned Salvo middleware source checksum mismatch")
    (stage / "src").mkdir(exist_ok=True)
    (stage / "src/lib.rs").write_bytes(source)
    fixture = ROOT / "tests/fixtures/salvo-compat"
    manifest = (fixture / "Cargo.toml.in").read_text(encoding="utf-8")
    manifest = manifest.replace("@SAYSION_PATH@", ROOT.as_posix())
    (stage / "Cargo.toml").write_text(manifest, encoding="utf-8")
    (stage / "tests").mkdir(exist_ok=True)
    shutil.copyfile(fixture / "compatibility.rs", stage / "tests/compatibility.rs")
    metadata = cargo_metadata(stage / "Cargo.toml")
    saysion = [package for package in metadata["packages"] if package["name"] == "saysion"]
    if len(saysion) != 1 or Path(saysion[0]["manifest_path"]).resolve() != ROOT / "Cargo.toml":
        raise RuntimeError("Middleware and application must resolve one saysion source")
    print(f"Salvo session {SALVO_REV}, unchanged source; dependency aligned to {ROOT}", flush=True)
    print(f"Fixture Cargo target: {metadata['target_directory']}", flush=True)
    subprocess.run(["cargo", "test", "--manifest-path", str(stage / "Cargo.toml"), "--test", "compatibility"], cwd=ROOT, check=True)


if __name__ == "__main__":
    main()
