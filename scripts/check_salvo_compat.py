"""Test real, pinned Salvo middleware with its saysion dependency aligned.

This stages unchanged upstream Rust source outside the repository. It does not
claim that the upstream 0.2 dependency already accepts 0.4, or publish a package.
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
from urllib.request import urlopen

ROOT = Path(__file__).resolve().parents[1]
SALVO_REV = "9eacc0f8a5de8de23a151ebdedd3a7c6243eb644"
SOURCE_SHA256 = "2317418b0ced75ab60c2b54a47c928905ac5d65b58387a1f02c5b808a2b370a7"


def main():
    target = Path(os.environ["CARGO_TARGET_DIR"]).resolve()
    if os.name == "nt" and target.drive.upper() != "D:":
        raise RuntimeError("Windows build storage must be on D:")
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
    command = [
        "cargo", "metadata", "--format-version", "1",
        "--manifest-path", str(stage / "Cargo.toml"),
    ]
    metadata = json.loads(subprocess.check_output(command, text=True))
    if Path(metadata["target_directory"]).resolve() != target:
        raise RuntimeError("Cargo target directory differs from configured storage")
    saysion = [package for package in metadata["packages"] if package["name"] == "saysion"]
    if len(saysion) != 1 or Path(saysion[0]["manifest_path"]).resolve() != ROOT / "Cargo.toml":
        raise RuntimeError("Middleware and application must resolve one saysion source")
    print(f"Salvo session {SALVO_REV}, unchanged source; dependency aligned to {ROOT}", flush=True)
    subprocess.run(["cargo", "test", "--manifest-path", str(stage / "Cargo.toml"), "--test", "compatibility"], check=True)


if __name__ == "__main__":
    main()
