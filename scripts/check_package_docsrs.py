#!/usr/bin/env python3
from __future__ import annotations

import argparse
import os
import pathlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
CRATES = [
    "lean-rs-sys",
    "lean-toolchain",
    "lean-rs",
    "lean-rs-host",
    "lean-rs-worker",
]


def fail(message: str) -> None:
    print(f"package check: {message}", file=sys.stderr)
    raise SystemExit(1)


def run(command: list[str], *, env: dict[str, str] | None = None, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
        check=True,
    )


def read_toml(path: pathlib.Path) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def package_workspace(allow_dirty: bool) -> str:
    command = ["cargo", "package", "--workspace", "--no-verify"]
    if allow_dirty:
        command.append("--allow-dirty")
    result = run(command, capture=True)
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    return result.stdout + result.stderr


def check_docs_rs_metadata() -> str:
    root_manifest = read_toml(ROOT / "Cargo.toml")
    version = root_manifest["workspace"]["package"]["version"]

    for crate in CRATES:
        manifest = read_toml(ROOT / "crates" / crate / "Cargo.toml")
        metadata = manifest["package"].get("metadata", {})
        docs_rs = metadata.get("docs", {}).get("rs")
        if not docs_rs:
            fail(f"{crate} is missing [package.metadata.docs.rs]")
        if docs_rs.get("default-target") != "x86_64-unknown-linux-gnu":
            fail(f"{crate} must set docs.rs default-target to x86_64-unknown-linux-gnu")
        if docs_rs.get("targets") != []:
            fail(f"{crate} must set docs.rs targets = [] to avoid cross-target doc builds")

    return version


def package_contents(version: str) -> None:
    required = {
        "lean-rs-sys": [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "src/lib.rs",
            "src/supported.rs",
            "tests/linkage.rs",
        ],
        "lean-toolchain": [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "src/build_helpers.rs",
            "src/diagnostics.rs",
            "src/fingerprint.rs",
        ],
        "lean-rs": [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "benches/hot_paths.rs",
            "examples/interop_callback.rs",
            "examples/string_streaming.rs",
            "shims/lean-rs-interop-shims/LeanRsInterop.lean",
            "shims/lean-rs-interop-shims/LeanRsInterop/Worker/Stream.lean",
            "shims/lean-rs-interop-shims/c/interop_callback.c",
            "shims/lean-rs-interop-shims/lakefile.lean",
            "shims/lean-rs-interop-shims/lean-toolchain",
            "src/module/preflight.rs",
        ],
        "lean-rs-host": [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "benches/session.rs",
            "examples/tour.rs",
            "shims/lean-rs-host-shims/LeanRsHostShims.lean",
            "shims/lean-rs-host-shims/lakefile.lean",
            "shims/lean-rs-interop-shims/LeanRsInterop.lean",
            "src/host/mod.rs",
            "tests/progress.rs",
        ],
        "lean-rs-worker": [
            "Cargo.toml",
            "README.md",
            "build.rs",
            "benches/row_payload.rs",
            "benches/worker_capability.rs",
            "examples/worker_capability_runner.rs",
            "src/bin/lean-rs-worker-child.rs",
            "src/capability.rs",
            "tests/typed_command.rs",
        ],
    }

    for crate in CRATES:
        package = ROOT / "target" / "package" / f"{crate}-{version}.crate"
        if not package.is_file():
            fail(f"missing packaged tarball {package}")
        with tarfile.open(package) as tar:
            names = set(tar.getnames())
        prefix = f"{crate}-{version}/"
        for rel in required[crate]:
            if prefix + rel not in names:
                fail(f"{crate} package is missing {rel}")


def template_package_contents() -> None:
    result = run(
        [
            "cargo",
            "package",
            "--manifest-path",
            "templates/shipped-lean-crate/Cargo.toml",
            "--allow-dirty",
            "--list",
        ],
        capture=True,
    )
    sys.stdout.write(result.stderr)
    template_files = set(result.stdout.splitlines())
    for rel in [
        "build.rs",
        "examples/worker.rs",
        "lean/ShipLeanDemo.lean",
        "lean/lakefile.lean",
        "lean/lean-toolchain",
        "lean/lake-manifest.json",
        "src/bin/shipped_lean_crate_worker.rs",
        "src/main.rs",
    ]:
        if rel not in template_files:
            fail(f"shipped-lean-crate package list is missing {rel}")


def release_docs_exist() -> None:
    for rel in [
        "docs/api-review/lean-rs-public.txt",
        "docs/api-review/lean-rs-worker-public.txt",
        "docs/recipes/ship-crate-with-lean.md",
        "docs/architecture/29-loader-and-artifact-boundary.md",
    ]:
        if not (ROOT / rel).is_file():
            fail(f"release documentation is missing {rel}")


def docs_rs_tarball_simulation(version: str) -> None:
    with tempfile.TemporaryDirectory(prefix="lean-rs-package-docsrs-") as tmp:
        workspace = pathlib.Path(tmp) / "workspace"
        workspace.mkdir()

        for crate in CRATES:
            package = ROOT / "target" / "package" / f"{crate}-{version}.crate"
            with tarfile.open(package) as tar:
                tar.extractall(workspace)

        members = ",\n  ".join(f'"{crate}-{version}"' for crate in CRATES)
        patches = "\n".join(f'{crate} = {{ path = "{crate}-{version}" }}' for crate in CRATES)
        (workspace / "Cargo.toml").write_text(
            f"""[workspace]
resolver = "3"
members = [
  {members},
]

[patch.crates-io]
{patches}
""",
            encoding="utf-8",
        )

        cargo_path = shutil.which("cargo")
        if not cargo_path:
            fail("could not locate cargo for packaged docs.rs simulation")
        cargo = pathlib.Path(cargo_path)
        cargo_bin = str(cargo.parent)
        safe_path = os.pathsep.join([cargo_bin, "/usr/bin", "/bin"])
        env = {
            "CARGO_HOME": os.environ.get("CARGO_HOME", str(pathlib.Path.home() / ".cargo")),
            "RUSTUP_HOME": os.environ.get("RUSTUP_HOME", str(pathlib.Path.home() / ".rustup")),
            "HOME": os.environ.get("HOME", str(pathlib.Path.home())),
            "PATH": safe_path,
            "DOCS_RS": "1",
            "RUSTDOCFLAGS": "-D warnings",
            "CARGO_TERM_COLOR": os.environ.get("CARGO_TERM_COLOR", "always"),
        }

        for crate in CRATES:
            subprocess.run(
                [
                    str(cargo),
                    "doc",
                    "--manifest-path",
                    str(workspace / "Cargo.toml"),
                    "--no-deps",
                    "-p",
                    crate,
                ],
                env=env,
                check=True,
            )


def main() -> None:
    parser = argparse.ArgumentParser(description="Check lean-rs package contents and docs.rs-mode docs from tarballs.")
    parser.add_argument(
        "--allow-dirty",
        action="store_true",
        help="pass --allow-dirty to cargo package; intended for local verification before committing",
    )
    args = parser.parse_args()

    version = check_docs_rs_metadata()
    package_log = package_workspace(args.allow_dirty)
    if "warning:" in package_log:
        fail("cargo package emitted an unexpected warning")
    package_contents(version)
    template_package_contents()
    release_docs_exist()
    docs_rs_tarball_simulation(version)
    print("package check: docs.rs tarball simulation passed")


if __name__ == "__main__":
    main()
