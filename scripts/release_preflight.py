"""Read-only crates.io release availability check; never publishes a package."""

from pathlib import Path
import sys
import tomllib
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen


def ensure_unpublished(name: str, version: str) -> None:
    """Reject an existing version, and fail closed on lookup failures."""
    url = f"https://crates.io/api/v1/crates/{quote(name, safe='')}/{quote(version, safe='')}"
    request = Request(url, headers={"User-Agent": "saysion-release-preflight"})
    try:
        with urlopen(request, timeout=30) as response:
            if response.status != 200:
                raise RuntimeError(f"Unexpected registry response: {response.status}")
    except HTTPError as error:
        if error.code == 404:
            return
        raise RuntimeError(f"Cannot confirm registry availability: HTTP {error.code}") from error
    except (URLError, TimeoutError, OSError) as error:
        raise RuntimeError(f"Cannot confirm registry availability: {error}") from error
    raise RuntimeError(f"{name} {version} already exists on crates.io; choose a new version")


def main() -> int:
    manifest = Path(__file__).resolve().parents[1] / "Cargo.toml"
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
    try:
        ensure_unpublished(package["name"], package["version"])
    except RuntimeError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1
    print(f"{package['name']} {package['version']} is not published on crates.io")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
