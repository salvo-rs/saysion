"""Carry verified release inputs between jobs; never publish a package."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def package_path():
    metadata = json.loads(command("cargo", "metadata", "--no-deps", "--format-version", "1"))
    package = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))["package"]
    return Path(metadata["target_directory"]) / "package" / f"{package['name']}-{package['version']}.crate"


def package_contents(path):
    """Compare payloads, independent of gzip headers or tar timestamps."""
    result = {}
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            if member.isfile():
                if member.name in result:
                    raise RuntimeError(f"Duplicate package entry: {member.name}")
                result[member.name] = archive.extractfile(member).read()
    return result


def verify_package(expected, actual):
    if package_contents(expected) != package_contents(actual):
        raise RuntimeError("Rebuilt release payload differs from the verified package")


def stage():
    package = package_path()
    destination = package.parent.parent / "verified-release"
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(package, destination / "package.crate")
    shutil.copyfile(ROOT / "Cargo.lock", destination / "Cargo.lock")
    metadata = {"commit": command("git", "rev-parse", "HEAD"), "rustc": command("rustc", "--version")}
    (destination / "metadata.json").write_text(json.dumps(metadata), encoding="utf-8")
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"path={destination}\n")
    print(destination)


def restore(directory):
    metadata = json.loads((directory / "metadata.json").read_text(encoding="utf-8"))
    if metadata["commit"] != command("git", "rev-parse", "HEAD"):
        raise RuntimeError("Release checkout does not match the verified commit")
    if metadata["rustc"] != command("rustc", "--version"):
        raise RuntimeError("Release toolchain does not match the verified toolchain")
    shutil.copyfile(directory / "Cargo.lock", ROOT / "Cargo.lock")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("stage", "restore", "compare"))
    parser.add_argument("directory", type=Path, nargs="?")
    arguments = parser.parse_args()
    if arguments.action == "stage":
        stage()
    elif arguments.directory is None:
        parser.error("restore and compare require the downloaded artifact directory")
    elif arguments.action == "restore":
        restore(arguments.directory)
    else:
        verify_package(arguments.directory / "package.crate", package_path())
        print("Rebuilt package payload matches the verified artifact")


if __name__ == "__main__":
    main()
