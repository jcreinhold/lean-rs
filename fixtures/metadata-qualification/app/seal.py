"""Seal the tracked ordinary fixture and installed Lean release selected by the runner."""

import hashlib
import json
import os
import platform
import subprocess
import sys
from pathlib import Path

OPTIONAL_META = "lean-rs-host-shims/LeanRsHostShims/Meta.lean"
OPTIONAL_INFO_TREE = "lean-rs-host-shims/LeanRsHostShims/InfoTree.lean"
WHNF = "lean_rs_host_meta_whnf"
OPTIONAL_SHIM_SOURCES = (
    "lean-rs-host-shims/LeanRsHostShims.lean",
    "lean-rs-host-shims/LeanRsHostShims/Elaboration.lean",
    "lean-rs-host-shims/LeanRsHostShims/Environment.lean",
    OPTIONAL_INFO_TREE,
    "lean-rs-host-shims/LeanRsHostShims/Interop.lean",
    "lean-rs-host-shims/LeanRsHostShims/Meta.lean",
    "lean-rs-host-shims/lake-manifest.json",
    "lean-rs-host-shims/lakefile.lean",
    "lean-rs-host-shims/lean-toolchain",
    "lean-rs-interop-shims/LeanRsInterop.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback/String.lean",
    "lean-rs-interop-shims/LeanRsInterop/Callback/Tick.lean",
    "lean-rs-interop-shims/LeanRsInterop/Worker/Stream.lean",
    "lean-rs-interop-shims/c/interop_callback.c",
    "lean-rs-interop-shims/lake-manifest.json",
    "lean-rs-interop-shims/lakefile.lean",
    "lean-rs-interop-shims/lean-toolchain",
)


def optional_shim_source(sdk, name):
    path = sdk / "crates/lean-rs-host/shims" / name
    assert path == path.resolve(strict=True) and path.is_file()
    limit = (148 if name == OPTIONAL_INFO_TREE else 144) * 1024
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    assert len(data) <= limit, f"optional shim source exceeds {limit} bytes: {name}"
    if name == OPTIONAL_META:
        attribute = f"@[export {WHNF}]\n".encode()
        assert data.count(attribute) == 1
        data = data.replace(attribute, b"", 1)
    return data


def stage_optional_shims(sdk, state):
    assert state == sdk / "fixtures/metadata-qualification/app/target/optional-whnf"
    bundle = state / "shims"
    subprocess.run(
        [
            "/usr/bin/git",
            "-C",
            sdk,
            "ls-files",
            "--error-unmatch",
            "--",
            *(f"crates/lean-rs-host/shims/{name}" for name in OPTIONAL_SHIM_SOURCES),
        ],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    bundle.mkdir(mode=0o700)
    total = 0
    for name in OPTIONAL_SHIM_SOURCES:
        path = bundle / name
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        data = optional_shim_source(sdk, name)
        total += len(data)
        assert total <= 512 * 1024
        with path.open("xb") as stream:
            stream.write(data)


def native_name(name, system=None):
    system = platform.system() if system is None else system
    assert system in ("Darwin", "Linux"), system
    return "lib" + name + (".dylib" if system == "Darwin" else ".so")


def defined_exports(path, system=None):
    system = platform.system() if system is None else system
    if system == "Darwin":
        lines = subprocess.check_output(
            ["/usr/bin/nm", "-gU", path], text=True
        ).splitlines()
        return {line.split()[-1] for line in lines if line.strip()}
    assert system == "Linux", system
    lines = subprocess.check_output(
        ["/usr/bin/nm", "-D", "--defined-only", path], text=True
    ).splitlines()
    exports = set()
    for line in lines:
        if not line.strip():
            continue
        symbol = line.split()[-1]
        # A default-version ELF symbol is available through ordinary dlsym;
        # a nondefault-only `name@VERSION` symbol is not evidence for `name`.
        exports.add(symbol.split("@@", 1)[0] if "@@" in symbol else symbol)
    return exports


def elf_dynamic(path, release_lib):
    lines = subprocess.check_output(
        ["/usr/bin/readelf", "-dW", path], text=True
    ).splitlines()
    needed, search = [], []
    search_tags = 0
    for line in lines:
        assert not any(
            f"({tag})" in line for tag in ("FILTER", "AUXILIARY", "AUDIT", "DEPAUDIT")
        ), line
        if "(NEEDED)" in line:
            value = line.split("[", 1)[1].split("]", 1)[0]
            assert value == Path(value).name and "/" not in value, value
            needed.append(value)
        elif "(RPATH)" in line or "(RUNPATH)" in line:
            search_tags += 1
            value = line.split("[", 1)[1].split("]", 1)[0]
            search.extend(value.split(":"))
    # The fixture does not trust an ambient loader search path. A Linux artifact
    # may name only its selected installed Lean runtime directory.
    assert search_tags <= 1, search
    assert release_lib == release_lib.resolve(), release_lib
    assert all(entry == str(release_lib) for entry in search), search
    allowed = {
        "libleanshared.so",
        "libLake_shared.so",
        "libInit_shared.so",
        "libc.so.6",
        "libdl.so.2",
        "libgcc_s.so.1",
        "libm.so.6",
        "libpthread.so.0",
        "librt.so.1",
        "libstdc++.so.6",
    }
    assert set(needed) <= allowed, needed
    return needed, search


def check_native_image(path, symbols, release_lib, system=None):
    system = platform.system() if system is None else system
    exports = defined_exports(path, system)
    prefix = "_" if system == "Darwin" else ""
    assert {prefix + symbol for symbol in symbols} <= exports
    if system == "Darwin":
        deps = [
            line.split(" (compatibility")[0].strip()
            for line in subprocess.check_output(
                ["/usr/bin/otool", "-L", path], text=True
            ).splitlines()[2:]
        ]
        assert all(
            dep.startswith("/usr/lib/")
            or dep in ("@rpath/libLake_shared.dylib", "@rpath/libleanshared.dylib")
            for dep in deps
        ), deps
    else:
        elf_dynamic(path, release_lib)


def main():
    a = Path(__file__).resolve().parent
    sdk = a.parents[2]
    state = Path(sys.argv[1]).resolve(strict=True)
    rec = json.loads((state / "prepared.json").read_text())
    broken = rec.get("broken_import", False)
    scalar = rec.get("scalar_entry", False)
    typed = rec.get("typed_session", False)
    stream = rec.get("stream", False)
    optional_whnf = rec.get("optional_whnf", False)
    assert not optional_whnf or (
        typed and not stream and state == a / "target/optional-whnf"
    )
    assert not (broken and typed)
    assert not scalar or (state == a / "target/scalar-entry" and not (broken or typed))
    assert not stream or typed
    legacy = rec.get("legacy_interop", False)
    assert not legacy or stream
    signature_probe = rec.get("signature_probe")
    assert signature_probe is None or stream
    if typed:
        assert rec.get("verification") is False
    host_builder_case = rec.get("host_builder_case")
    builder_profiles = {
        "a_host_handle_built_without_imports_opens_no_session": (
            "lean-lazy",
            "worker_host_lazy",
            "import Demo.Good\n",
            False,
        ),
        "shims_only_host_handle_skips_user_shared_build": (
            "lean-shims-only",
            "worker_host_handle",
            "import Demo.Good\nimport Demo.Broken\n",
            True,
        ),
    }
    transitive = host_builder_case == (
        "shims_only_host_handle_imports_transitive_lake_package_oleans"
    )
    good = host_builder_case == "shims_only_session_opens_without_user_dylib"
    broken_shared = host_builder_case == (
        "load_shims_only_succeeds_when_user_shared_facet_does_not_build"
    )
    assert host_builder_case is None or (
        broken
        and (
            transitive or good or broken_shared or host_builder_case in builder_profiles
        )
    )
    builder = builder_profiles.get(host_builder_case)
    private_package = sdk / "target/metadata-qualification/stream-package"
    assets = private_package if stream else sdk
    project = (
        state
        / (
            "lean-transitive"
            if transitive
            else "lean-broken-shared"
            if broken_shared
            else "lean-good"
            if good
            else builder[0]
            if builder
            else "lean"
        )
        if broken
        else assets
        / (
            "fixtures/interop-worker"
            if stream
            else "fixtures/lean"
            if typed
            else "fixtures/metadata-qualification/lean"
        )
    )
    if legacy:
        project = state / "legacy"
    elif scalar:
        project = state / "lean"
    elif typed and not stream and not optional_whnf:
        project = Path(os.environ["LEAN_RS_TEST_TYPED_PROJECT"])
        assert project in (sdk / "fixtures/lean", state / "lean"), project
        assert project == project.resolve(strict=True) and project.is_dir(), project
        owner = project.stat()
        assert owner.st_uid == os.geteuid() and owner.st_mode & 0o7777 == 0o700, project
    assert Path(rec["project"]) == project
    foreign = Path(rec["foreign_workspace"]) if rec.get("foreign_workspace") else None
    if foreign:
        assert (
            stream and not rec.get("signature_probe") and foreign == state / "foreign"
        )
        assert foreign == foreign.resolve(strict=True)
        owner = foreign.stat()
        assert owner.st_uid == os.geteuid() and owner.st_mode & 0o7777 == 0o700
    release = Path(rec["sysroot"])
    shim_package = private_package if broken or typed or scalar else sdk
    bundle = state / "shims" if optional_whnf else shim_package / "crates/lean-rs-host/shims"
    dep = bundle / "lean-rs-interop-shims"
    host = bundle / "lean-rs-host-shims"
    if legacy:
        legacy_provider = dep / ".lake/build/lib/lean"
        assert not os.path.lexists(legacy_provider), (
            "ordinary Lake import root would shadow the pinned provider"
        )
        rec["legacy_provider_root"] = str(legacy_provider)
        assert not any(
            field in rec
            for field in ("consumer_manifest", "consumer_json", "signature_probe")
        )
    key = rec["generated_output_key"]
    assert key == "lean-4.36.0"
    out = lambda p: p / ".lake/lean-rs-generated" / key / "lib"
    image = lambda p, name: out(p) / native_name(name)
    di = image(dep, "lean__rs__interop__shims_LeanRsInterop")
    hi = image(host, "lean__rs__host__shims_LeanRsHostShims")
    package, module = ("lean_rs_interop_consumer", "LeanRsInteropConsumer")
    stem = package.replace("_", "__") + "_" + module
    if typed and not stream:
        package, module = "lean_rs_fixture", "LeanRsFixture.Effects"
        stem = "lean__rs__fixture_LeanRsFixture"
    if scalar:
        package, module = "lean_rs_fixture", "LeanRsFixture"
        stem = "lean__rs__fixture_LeanRsFixture"
    ci = (
        None
        if broken
        else (
            project / ".lake/build/lib" / native_name(stem)
            if legacy or scalar or (typed and not stream)
            else image(project, stem)
        )
    )
    shims = [out(dep) / "lean", out(host) / "lean"]
    if not broken:
        assert Path(rec["consumer_image"]) == ci
    assert list(map(Path, rec["shim_paths"])) == shims
    user_dep = project / ".lake/packages/dep"
    assert list(map(Path, rec["roots"])) == sorted(
        [
            project,
            dep,
            host,
            *([user_dep] if transitive else []),
            *([foreign] if foreign else []),
        ]
    )
    assert list(map(Path, rec["shim_roots"])) == sorted([dep, host])
    filepins = {}
    contentpins = {}
    directories = []

    def pin(path):
        assert path == path.resolve() and path.is_file(), path
        stat = path.stat()
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        filepins[str(path)] = [stat.st_dev, stat.st_ino, digest]

    def tree(path):
        assert path == path.resolve() and path.is_dir(), path
        directories.append(str(path))
        for child in path.iterdir():
            assert not child.is_symlink(), child
            tree(child) if child.is_dir() else pin(child)

    def pin_content(path):
        assert path == path.resolve() and path.is_file(), path
        with path.open("rb") as stream:
            contentpins[str(path)] = hashlib.file_digest(stream, "sha256").hexdigest()

    for manifest in ["shim"] if broken or legacy else ["consumer", "shim"]:
        path = Path(rec[manifest + "_manifest"])
        assert path.read_text() == rec[manifest + "_json"]
        # Pin the typed capability manifest in the private state. The existing
        # typed host-shim snapshot can be republished under its writer.
        if scalar and manifest == "consumer":
            assert path.parent == project / ".lake/lean-rs-build-cache"
            assert path.name.startswith("LeanRsFixture-") and path.name.endswith(
                ".lean-rs-capability.json"
            )
            pin(path)
        elif typed and manifest == "consumer":
            assert path == state / (
                "worker-stream-capability.json"
                if stream
                else "worker-json-capability.json"
            )
            pin(path)
        elif optional_whnf or (not broken and not typed):
            pin(path)
        v = json.loads(path.read_text())
        p, m, i = (
            (package, module, ci)
            if manifest == "consumer"
            else ("lean_rs_host_shims", "LeanRsHostShims", hi)
        )
        assert (v["package"], v["module"], v["primary_dylib"]) == (p, m, str(i))
        if optional_whnf and manifest == "shim":
            assert [row["symbol"] for row in v["exports"]].count(WHNF) == 1
        if stream and manifest == "consumer":
            assert v["target_name"] == "LeanRsInteropConsumer"
        assert v["dependencies"] == (
            []
            if (scalar or (typed and not stream)) and manifest == "consumer"
            else [
                {
                    "name": "lean_rs_interop_shims",
                    "dylib_path": str(di),
                    "export_symbols_for_dependents": True,
                    "initializer": {
                        "package": "lean_rs_interop_shims",
                        "module": "LeanRsInterop",
                    },
                }
            ]
        )
        assert (
            (scalar and manifest == "consumer" and v["build_toolchain"] is None)
            or v["build_toolchain"]["sysroot"] == str(release)
        ) and v["toolchain_fingerprint"]["lean_version"] == "4.36.0"
        assert (
            v["toolchain_fingerprint"]["header_sha256"]
            == "dbccdab3a0036f41494f22762adce633875e34cf8f6f7cfa2186175b0e964aa4"
        )
    project_symbols = ["initialize_" + stem]
    if legacy:
        project_symbols.append("lean_rs_interop_consumer_worker_json_command")
        pin_content(project / "lean-toolchain")
    elif stream:
        exports = json.loads(rec["consumer_json"])["exports"]
        symbols = [export["symbol"] for export in exports]
        if signature_probe is None:
            assert 0 < len(symbols) <= 33 and len(set(symbols)) == len(symbols)
            project_symbols.extend(symbols)
        else:
            real_symbol = "lean_rs_interop_consumer_worker_json_command"
            if (
                signature_probe
                == "manifest_missing_worker_export_signature_fails_before_dispatch"
            ):
                assert symbols == []
            else:
                assert (
                    signature_probe
                    == "manifest_wrong_worker_export_signature_fails_before_dispatch"
                )
                assert symbols == [real_symbol]
            # Check the actual export, even when its manifest row is absent.
            project_symbols.append(real_symbol)
    elif scalar:
        expected = ["lean_rs_fixture_u64_mul", "lean_rs_fixture_panic_unit"]
        assert [
            e["symbol"] for e in json.loads(rec["consumer_json"])["exports"]
        ] == expected
        project_symbols.extend(expected)
    elif typed:
        project_symbols.extend(
            [
                "initialize_lean__rs__fixture_LeanRsFixture_Effects",
                "lean_rs_interop_consumer_worker_json_command",
            ]
        )
    elif not broken:
        project_symbols.append("lean_rs_interop_consumer_worker_metadata")
    if stream:
        pin_content(project / "lakefile.lean")
        for source in (
            "LeanRsInteropConsumer.lean",
            "LeanRsInteropConsumer/Callback.lean",
        ):
            pin_content(assets / "fixtures/interop-shims" / source)
    images = [
        (di, ["initialize_lean__rs__interop__shims_LeanRsInterop"]),
        (hi, ["initialize_lean__rs__host__shims_LeanRsHostShims"]),
    ]
    if not broken:
        images.append((ci, project_symbols))
    for f, syms in images:
        pin(f)
        if optional_whnf and f == hi:
            syms.append("lean_rs_host_meta_infer_type")
        check_native_image(f, syms, release / "lib/lean")
        if optional_whnf:
            prefix = "_" if platform.system() == "Darwin" else ""
            assert prefix + WHNF not in defined_exports(f), (
                f"WHNF unexpectedly exported by {f}"
            )
    if optional_whnf:
        for name in OPTIONAL_SHIM_SOURCES:
            path = bundle / name
            if path.name != "lake-manifest.json":
                assert path.read_bytes() == optional_shim_source(sdk, name)
            pin_content(path)
    for root in (dep, host, project):
        pin_content(root / "lake-manifest.json")
    packages = json.loads((project / "lake-manifest.json").read_text())["packages"]
    project_deps = []
    if scalar or (typed and not stream):
        assert packages == []
    elif transitive:
        assert packages == [
            {
                "type": "path",
                "scope": "",
                "name": "dep",
                "manifestFile": "lake-manifest.json",
                "inherited": False,
                "dir": "./.lake/packages/dep",
                "configFile": "lakefile.lean",
            }
        ]
        inputs = {
            project / "lakefile.lean": (
                "import Lake\nopen Lake DSL\npackage consumer\n"
                'require dep from "./.lake/packages/dep"\nlean_lib Consumer\n'
            ),
            project / "Consumer.lean": (
                "import Dep.Hello\ndef Consumer.value : Nat := Dep.hello + 1\n"
            ),
            user_dep
            / "lakefile.lean": "import Lake\nopen Lake DSL\npackage dep\nlean_lib Dep\n",
            user_dep / "Dep/Hello.lean": "def Dep.hello : Nat := 41\n",
            **{
                root / "lean-toolchain": "leanprover/lean4:v4.36.0\n"
                for root in (project, user_dep)
            },
        }
        for path, text in inputs.items():
            assert path.read_text() == text
            pin_content(path)
        dep_manifest = user_dep / "lake-manifest.json"
        assert json.loads(dep_manifest.read_text())["packages"] == []
        pin_content(dep_manifest)
        project_deps = [user_dep / ".lake/build/lib/lean"]
        for root in (project, user_dep):
            assert not any(
                f.suffix in {".dylib", ".so", ".dll"}
                for f in (root / ".lake/build/lib").rglob("*")
            )
    elif good or broken_shared:
        assert packages == []
        lakefile = (
            "import Lake\nopen Lake DSL\npackage demo_broken\nlean_lib Good\nlean_lib Broken\n"
            if broken_shared
            else "import Lake\nopen Lake DSL\npackage demo_shims\nlean_lib Good\n"
        )
        for name, value in {
            "lakefile.lean": lakefile,
            "Good.lean": "def goodValue : Nat := 41\n",
            "lean-toolchain": "leanprover/lean4:v4.36.0\n",
        }.items():
            assert (project / name).read_text() == value
            pin_content(project / name)
        if broken_shared:
            source = project / "Broken.lean"
            assert (
                source.read_text()
                == "theorem broken : True := sorry_that_doesnt_exist\n"
            )
            pin_content(source)
        assert not any(
            f.suffix in {".dylib", ".so", ".dll"}
            for f in (project / ".lake/build/lib").rglob("*")
        )
    elif broken:
        assert packages == []
        sources = ["lakefile.lean", "Demo.lean", "lean-toolchain"]
        if builder:
            _, package, umbrella, has_broken = builder
            assert (project / "lakefile.lean").read_text() == (
                f"import Lake\nopen Lake DSL\npackage {package}\nlean_lib Demo\n"
            )
            assert (project / "Demo.lean").read_text() == umbrella
            assert (
                project / "Demo/Good.lean"
            ).read_text() == "def goodValue : Nat := 41\n"
            sources.append("Demo/Good.lean")
        else:
            has_broken = True
        if has_broken:
            assert (
                project / "Demo/Broken.lean"
            ).read_text() == "theorem broken : True := sorry_that_doesnt_exist\n"
            sources.append("Demo/Broken.lean")
        else:
            assert not (project / "Demo/Broken.lean").exists()
        for source in sources:
            pin_content(project / source)
    else:
        assert len(packages) == 1
        newdir = packages[0]["dir"]
        assert (project / newdir).resolve() == dep
        if legacy:
            assert newdir == str(dep), (
                "direct consumer must use the canonical provider workspace"
            )
        else:
            project_deps = [
                project / newdir / ".lake/lean-rs-generated" / key / "lib/lean"
            ]
    project_oleans = (
        project / ".lake/build/lib/lean"
        if legacy or scalar or broken or (typed and not stream)
        else out(project) / "lean"
    )
    # Both built packages must actually select the same source package.
    assert (
        host
        / json.loads((host / "lake-manifest.json").read_text())["packages"][0]["dir"]
    ).resolve() == dep
    for f in [release / "include/lean/lean.h", release / "bin/lean"]:
        pin(f)
    assert (
        filepins[str(release / "include/lean/lean.h")][2]
        == "dbccdab3a0036f41494f22762adce633875e34cf8f6f7cfa2186175b0e964aa4"
    )
    for r in [
        project_oleans,
        *(project_deps if transitive else []),
        *shims,
        release / "lib/lean",
    ]:
        tree(r)
    if foreign:
        foreign_oleans = foreign / ".lake/build/lib/lean"
        inputs = {
            "lakefile.lean": "import Lake\nopen Lake DSL\npackage audited_workspace\nlean_lib B\n",
            "B.lean": "import B.Mod\n",
            "B/Mod.lean": "def B.Mod.externalValue : Nat := 7\n",
            "lean-toolchain": "leanprover/lean4:v4.36.0\n",
        }
        for name, text in inputs.items():
            source = foreign / name
            assert source.read_text() == text
            pin_content(source)
        manifest = foreign / "lake-manifest.json"
        assert json.loads(manifest.read_text())["packages"] == []
        pin_content(manifest)
        tree(foreign_oleans)
        assert str(foreign_oleans / "B/Mod.olean") in filepins
        assert not any(
            (root / "B").exists() or (root / "B.olean").exists()
            for root in [project_oleans, *project_deps, *shims, release / "lib/lean"]
        )
        assert not any(
            (root / "LeanRsInteropConsumer").exists()
            or (root / "LeanRsInteropConsumer.olean").exists()
            for root in [foreign_oleans, *shims, release / "lib/lean"]
        )
    if typed and not stream:
        for module in ("LeanRsFixtureLateExtension.olean", "LeanRsFixture/Meta.olean"):
            required = project_oleans / module
            assert str(required) in filepins, (
                f"missing sealed registration/control artifact: {required}"
            )
        for module in ("Imported", "Internal", "PrivateScope"):
            required = project_oleans / f"Fixture/{module}.olean"
            assert str(required) in filepins, (
                f"missing sealed module-syntax artifact: {required}"
            )
    # Verify original transitive import selection and refuse local shadows of builtin namespaces.
    for prefix in ["Init", "Lean", "Lake", "Std"]:
        for r in [project_oleans, *(project_deps if transitive else []), *shims]:
            assert not (r / prefix).exists() and not (r / (prefix + ".olean")).exists()
    if transitive:
        for relative, selected in (
            ("Consumer.olean", project_oleans),
            ("Dep/Hello.olean", project_deps[0]),
            ("Dep/NonExistent.olean", None),
        ):
            found = [
                root
                for root in [
                    project_oleans,
                    *project_deps,
                    *shims,
                    release / "lib/lean",
                ]
                if (root / relative).exists()
            ]
            assert found == ([selected] if selected else [])
    elif good or broken_shared:
        assert str(project_oleans / "Good.olean") in filepins
        assert not any(
            (r / "Good.olean").exists() for r in [*shims, release / "lib/lean"]
        )
        if broken_shared:
            assert not any(
                os.path.lexists(r / name)
                for r in [project_oleans, *shims, release / "lib/lean"]
                for name in ("Broken", "Broken.olean")
            )
    elif broken:
        if builder:
            assert str(project_oleans / "Demo/Good.olean") in filepins
            assert not any(
                (r / "Demo/Good.olean").exists() for r in [*shims, release / "lib/lean"]
            )
            for stem in ("Demo", f"{builder[1].replace('_', '__')}_Demo"):
                assert not (project / ".lake/build/lib" / native_name(stem)).exists()
        assert not any(
            (r / "Demo/Broken.olean").exists() or (r / "Demo.olean").exists()
            for r in [project_oleans, *shims, release / "lib/lean"]
        )
    elif scalar or (typed and not stream):
        for module in ("LeanRsFixture/Handles", "LeanRsFixture/SourceRanges"):
            assert (project_oleans / (module + ".olean")).is_file()
    else:
        assert (project_oleans / "LeanRsInteropConsumer/Callback.olean").is_file()
    assert (shims[0] / "LeanRsInterop.olean").is_file()
    assert (
        not (project_oleans / "LeanRsInterop").exists()
        and not (project_oleans / "LeanRsInterop.olean").exists()
    )
    rec.update(
        files=filepins,
        content_files=contentpins,
        directories=sorted(directories),
        interop_image=str(di),
        host_image=str(hi),
        import_roots=list(map(str, [project_oleans, *project_deps, *shims])),
    )
    rec["ordered_directory_ids"] = [
        (Path(root).stat().st_dev, Path(root).stat().st_ino)
        for root in rec["import_roots"]
    ]
    (state / "deployment.json").write_text(
        json.dumps(rec, separators=(",", ":"), sort_keys=True)
    )
    print(
        "sealed native images, registration order and checked exports:",
        ci,
        di,
        hi,
        "files",
        len(filepins),
        "directories",
        len(directories),
        flush=True,
    )


if __name__ == "__main__":
    main()
