#!/usr/bin/env bash
# scripts/prerelease.sh — run every pre-release gate locally.
#
# This script is the local mirror of `.github/workflows/release.yml`'s
# `verify` job: it pins the workspace to a single Lean toolchain,
# builds the Lake packages, exports `LEAN_SYSROOT`, then runs the same
# Rust gates CI runs (fmt, clippy, actionlint, nextest, doc tests,
# loader regressions, rustdoc, the docs.rs simulation, the public-API
# baseline diff, and the ABI fuzz smoke on Linux).
#
# Two modes:
#
#   * Host mode (default): runs against whichever Rust + elan + Lean
#     toolchains are already on `PATH`. Fast for iterative work.
#   * Docker mode (`--docker`): builds the
#     `scripts/Dockerfile.prerelease` image (cached) and re-executes
#     this script inside it. The container's environment matches the
#     Ubuntu-24.04 `ubuntu-latest` cell in CI bit-for-bit.
#
# Failures print the failing gate's name and exit non-zero. The
# `lean-toolchain` files are pinned/restored under a trap, so an
# interrupted run leaves no on-disk drift.
#
# Usage:
#   scripts/prerelease.sh                  # all gates, host mode
#   scripts/prerelease.sh --docker         # all gates, in container
#   scripts/prerelease.sh --docker --docker-platform linux/amd64
#                                          # CI-exact (slow on Apple Silicon)
#   scripts/prerelease.sh --quick          # skip ABI fuzz + public-API diff
#   scripts/prerelease.sh --no-fuzz        # skip ABI fuzz
#   scripts/prerelease.sh --no-publicapi   # skip public-API diff
#   scripts/prerelease.sh --lean-version 4.29.1  # override pinned version
#   scripts/prerelease.sh --help

set -euo pipefail

# -- defaults ---------------------------------------------------------------

# The default mirrors `.github/workflows/release.yml::env.LEAN_VERSION_HEAD`.
# Bumping the supported window requires editing both in lockstep — see
# `docs/bump-toolchain.md`.
DEFAULT_LEAN_VERSION="4.32.0-rc1"

LEAN_VERSION="$DEFAULT_LEAN_VERSION"
RUN_FUZZ=1
RUN_PUBLIC_API=1
USE_DOCKER=0
DOCKER_PLATFORM=""

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCKERFILE="$REPO_ROOT/scripts/Dockerfile.prerelease"
DOCKER_IMAGE_TAG="lean-rs-prerelease"

# Default docker platform: match the host architecture so the
# container runs natively. Apple Silicon → linux/arm64, Intel/AMD →
# linux/amd64. CI runs on linux/amd64 (ubuntu-latest); pass
# `--docker-platform linux/amd64` explicitly on Apple Silicon for a
# CI-exact reproduction (slow because of QEMU emulation, and certain
# timing- or syscall-sensitive tests behave differently under QEMU).
case "$(uname -m)" in
arm64 | aarch64) HOST_DOCKER_PLATFORM="linux/arm64" ;;
x86_64 | amd64) HOST_DOCKER_PLATFORM="linux/amd64" ;;
*) HOST_DOCKER_PLATFORM="" ;;
esac

# -- logging ----------------------------------------------------------------

# ANSI styling, suppressed when stdout is not a TTY (CI logs, redirects).
if [[ -t 1 ]]; then
	BOLD=$'\033[1m'
	GREEN=$'\033[32m'
	RED=$'\033[31m'
	YELLOW=$'\033[33m'
	RESET=$'\033[0m'
else
	BOLD=""
	GREEN=""
	RED=""
	YELLOW=""
	RESET=""
fi

log_step() { printf '\n%s==>%s %s%s%s\n' "$BOLD" "$RESET" "$BOLD" "$*" "$RESET"; }
log_ok() { printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
log_warn() { printf '%s!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
log_err() { printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2; }

# -- arg parsing ------------------------------------------------------------

usage() {
	sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
	case "$1" in
	--docker)
		USE_DOCKER=1
		shift
		;;
	--no-fuzz)
		RUN_FUZZ=0
		shift
		;;
	--no-publicapi)
		RUN_PUBLIC_API=0
		shift
		;;
	--quick)
		RUN_FUZZ=0
		RUN_PUBLIC_API=0
		shift
		;;
	--lean-version)
		if [[ $# -lt 2 ]]; then
			log_err "--lean-version requires a value"
			exit 2
		fi
		LEAN_VERSION="$2"
		shift 2
		;;
	--lean-version=*)
		LEAN_VERSION="${1#*=}"
		shift
		;;
	--docker-platform)
		if [[ $# -lt 2 ]]; then
			log_err "--docker-platform requires a value"
			exit 2
		fi
		DOCKER_PLATFORM="$2"
		shift 2
		;;
	--docker-platform=*)
		DOCKER_PLATFORM="${1#*=}"
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		log_err "unknown argument: $1"
		usage >&2
		exit 2
		;;
	esac
done

# -- docker entrypoint ------------------------------------------------------

# When `--docker` is set, build the image (cached on the Dockerfile +
# build args) and re-exec this script inside a container that mounts
# the repo. The recursive call drops `--docker`; everything else flows
# through verbatim.
if [[ "$USE_DOCKER" == 1 ]]; then
	if ! command -v docker >/dev/null 2>&1; then
		log_err "docker not found on PATH"
		exit 2
	fi
	if [[ ! -f "$DOCKERFILE" ]]; then
		log_err "Dockerfile not found at $DOCKERFILE"
		exit 2
	fi

	platform="${DOCKER_PLATFORM:-$HOST_DOCKER_PLATFORM}"
	if [[ -z "$platform" ]]; then
		log_err "could not determine docker platform (uname -m: $(uname -m)); pass --docker-platform"
		exit 2
	fi
	# Volume names include the platform so a switch between amd64
	# and arm64 cleanly starts a fresh build cache instead of
	# tripping on mixed-arch artifacts.
	platform_tag="${platform//\//-}"

	log_step "Building Docker image (Lean ${LEAN_VERSION}, ${platform})"
	docker build \
		--platform "$platform" \
		--file "$DOCKERFILE" \
		--build-arg "LEAN_VERSION=${LEAN_VERSION}" \
		--tag "${DOCKER_IMAGE_TAG}:${LEAN_VERSION}-${platform_tag}" \
		"$REPO_ROOT/scripts"

	# The host repo is bind-mounted read-only at /repo-source; on
	# entry the container rsyncs into a writable /workspace volume,
	# excluding target/ and every .lake/ (build artifacts whose
	# embedded paths and architecture would otherwise leak from the
	# host into the container — host macOS .dylibs masquerading as
	# Linux .so files, /Users/... paths baked into compiled test
	# fixtures, etc.). cargo caches stay on dedicated volumes so
	# incremental compilation survives across invocations.
	args=("--lean-version" "$LEAN_VERSION")
	[[ "$RUN_FUZZ" == 0 ]] && args+=("--no-fuzz")
	[[ "$RUN_PUBLIC_API" == 0 ]] && args+=("--no-publicapi")

	log_step "Running pre-release gates inside container (${platform})"
	# shellcheck disable=SC2016 # the $@ inside the bash -c body is
	# expanded inside the container, not by this shell.
	exec docker run --rm \
		--init \
		--platform "$platform" \
		--workdir /workspace \
		--mount "type=bind,source=${REPO_ROOT},target=/repo-source,readonly" \
		--mount "type=volume,source=lean-rs-prerelease-workspace-${platform_tag},target=/workspace" \
		--mount "type=volume,source=lean-rs-prerelease-cargo-registry-${platform_tag},target=/root/.cargo/registry" \
		--mount "type=volume,source=lean-rs-prerelease-cargo-git-${platform_tag},target=/root/.cargo/git" \
		"${DOCKER_IMAGE_TAG}:${LEAN_VERSION}-${platform_tag}" \
		bash -c '
			set -euo pipefail
			rsync -a --delete \
				--exclude=target \
				--exclude=.lake \
				/repo-source/ /workspace/
			exec bash /workspace/scripts/prerelease.sh "$@"
		' -- "${args[@]}"
fi

# -- host preflight ---------------------------------------------------------

require_cmd() {
	if ! command -v "$1" >/dev/null 2>&1; then
		log_err "required command not found: $1${2:+ ($2)}"
		exit 2
	fi
}

require_cmd cargo "install via https://rustup.rs"
require_cmd rustup
require_cmd cargo-nextest "cargo install cargo-nextest --locked"
require_cmd lake "install via elan + leanprover/lean4"
require_cmd lean
require_cmd python3
require_cmd git

OS_KIND="$(uname -s)"
case "$OS_KIND" in
Linux | Darwin) ;;
*)
	log_err "unsupported OS: $OS_KIND (Linux and Darwin only)"
	exit 2
	;;
esac

# -- toolchain pinning ------------------------------------------------------

# The Lake packages embed the toolchain version in their lean-toolchain
# files; the host Rust build picks LEAN_SYSROOT off the elan layout. We
# pin every relevant file to the target version, then restore on exit
# so an interrupted run does not leave the tree in a half-pinned state.
TOOLCHAIN_FILES=(
	"$REPO_ROOT/lean-toolchain"
	"$REPO_ROOT/crates/lean-rs/shims/lean-rs-interop-shims/lean-toolchain"
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-interop-shims/lean-toolchain"
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-host-shims/lean-toolchain"
	"$REPO_ROOT/fixtures/lean/lean-toolchain"
	"$REPO_ROOT/fixtures/interop-shims/lean-toolchain"
	"$REPO_ROOT/templates/shipped-lean-crate/lean/lean-toolchain"
)

declare -a BACKED_UP=()

# Invoked via `trap restore_toolchains EXIT`; the trap dispatch hides
# the call from shellcheck.
# shellcheck disable=SC2329
restore_toolchains() {
	local path
	for path in "${BACKED_UP[@]}"; do
		if [[ -f "${path}.bak" ]]; then
			mv "${path}.bak" "$path"
		fi
	done
}
trap restore_toolchains EXIT

for path in "${TOOLCHAIN_FILES[@]}"; do
	if [[ -f "$path" ]]; then
		cp "$path" "${path}.bak"
		BACKED_UP+=("$path")
	fi
	printf 'leanprover/lean4:v%s\n' "$LEAN_VERSION" >"$path"
done

# -- LEAN_SYSROOT + LD_LIBRARY_PATH ----------------------------------------

ELAN_HOME="${ELAN_HOME:-$HOME/.elan}"
SYSROOT="$ELAN_HOME/toolchains/leanprover--lean4---v${LEAN_VERSION}"
if [[ ! -d "$SYSROOT" ]]; then
	log_err "Lean ${LEAN_VERSION} toolchain not installed at $SYSROOT"
	log_err "install it with: elan toolchain install leanprover/lean4:v${LEAN_VERSION}"
	exit 2
fi
export LEAN_SYSROOT="$SYSROOT"

LAKE_LIB_DIRS=(
	"$REPO_ROOT/crates/lean-rs/shims/lean-rs-interop-shims/.lake/build/lib"
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-interop-shims/.lake/build/lib"
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-host-shims/.lake/build/lib"
	"$REPO_ROOT/fixtures/lean/.lake/build/lib"
	"$REPO_ROOT/fixtures/interop-shims/.lake/build/lib"
)

# Linux dlopen needs to find the Lake-built shim dylibs; macOS uses
# DYLD_LIBRARY_PATH but the workspace's build script encodes rpaths,
# so the Linux export is the only one we need.
if [[ "$OS_KIND" == "Linux" ]]; then
	joined="$(
		IFS=:
		echo "${LAKE_LIB_DIRS[*]}"
	)"
	export LD_LIBRARY_PATH="${joined}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

# -- gate runner ------------------------------------------------------------

declare -a PASSED=()
declare -a FAILED=()
declare -a SKIPPED=()

# `run_gate NAME COMMAND ARGS...` runs the gate and records its
# pass/fail status. Output is streamed live; the run terminates only
# if all gates have been attempted.
run_gate() {
	local name="$1"
	shift
	log_step "$name"
	local start
	start=$SECONDS
	if "$@"; then
		local elapsed=$((SECONDS - start))
		log_ok "$name (${elapsed}s)"
		PASSED+=("$name")
	else
		local rc=$?
		local elapsed=$((SECONDS - start))
		log_err "$name FAILED in ${elapsed}s (exit $rc)"
		FAILED+=("$name")
	fi
}

# -- gates ------------------------------------------------------------------

log_step "Building Lake packages"
for dir in \
	"$REPO_ROOT/crates/lean-rs/shims/lean-rs-interop-shims" \
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-interop-shims" \
	"$REPO_ROOT/crates/lean-rs-host/shims/lean-rs-host-shims" \
	"$REPO_ROOT/fixtures/lean" \
	"$REPO_ROOT/fixtures/interop-shims"; do
	log_step "lake build ($(basename "$dir"))"
	(cd "$dir" && lake build)
done

# The shipped-crate template (templates/shipped-lean-crate) is built lazily by
# the worker loader-regression tests (cargo build -> build.rs -> lake), so the
# loop above does not build it. Its .lake oleans and the cargo target that
# embeds the toolchain digest must be wiped when we re-pin, or a previous
# toolchain's artifacts linger and the worker bootstrap fails reading them with
# an "incompatible header" Lean exception. The toolchain sweep
# (scripts/test-all-toolchains.sh) wipes the same paths for the same reason.
log_step "Wiping stale shipped-crate template artifacts"
rm -rf "$REPO_ROOT/templates/shipped-lean-crate/lean/.lake" \
	"$REPO_ROOT/templates/shipped-lean-crate/target"

run_gate "cargo fmt --check" \
	cargo fmt --all --check

run_gate "cargo clippy --all-targets -- -D warnings" \
	cargo clippy --workspace --all-targets -- -D warnings

if command -v actionlint >/dev/null 2>&1; then
	run_gate "actionlint workflows" \
		actionlint \
		"$REPO_ROOT/.github/workflows/ci.yml" \
		"$REPO_ROOT/.github/workflows/release.yml" \
		"$REPO_ROOT/.github/workflows/sanitizer.yml" \
		"$REPO_ROOT/.github/workflows/compile-fail.yml"
else
	log_warn "actionlint not installed; skipping (install: go install github.com/rhysd/actionlint/cmd/actionlint@latest)"
	SKIPPED+=("actionlint workflows")
fi

# nextest on Linux preloads libgcc_s.so.1 — Lean's `libleanshared.so`
# statically links its own libstdc++ + `_Unwind_*` symbols, which shadow
# glibc's libgcc_s when the dylib is dlopened. Rust panic unwinding
# then can't find `catch_unwind` landing pads and aborts. Preloading the
# system libgcc_s forces it to win the symbol resolution race. The
# workflow hardcodes the x86_64 path; we resolve via the dynamic loader
# instead so the script also works on aarch64 hosts.
#
# Inside a Docker container we serialise the suite. The nextest config
# defaults to 4 concurrent test processes × ~1.5 GiB per Lean import,
# and the leak-loop integration tests double up further. CI runners
# get away with parallel runs because the host kernel is generous with
# overcommit; Docker Desktop's Linux VM enforces stricter cgroup
# memory accounting on a smaller pool (~8 GiB) and OOM-kills the leak
# loops mid-import. test-threads=1 keeps the gate authoritative at the
# cost of wall time (~5 min vs ~1.5 min). The pre-release pass is not
# run often enough for the parallelism to matter.
run_nextest() {
	local env_args=(env RUST_BACKTRACE=0)
	if [[ "$OS_KIND" == "Linux" ]]; then
		env_args+=("LD_PRELOAD=libgcc_s.so.1")
	fi
	if [[ -f /.dockerenv ]]; then
		env_args+=("NEXTEST_TEST_THREADS=1")
		# The default 8-iteration `session_create_drop_loop_small`
		# accumulates enough per-import Lean residual to OOM-kill in
		# a Docker cgroup; reduce to 4, matching the companion
		# `pool_overflow_eviction_loop_small` default. The dedicated
		# sanitizer CI job runs the longer `_loop_long` variants
		# anyway — those are the real leak gate.
		env_args+=("LEAN_RS_LEAK_LOOP_ITERS=4")
	fi
	"${env_args[@]}" cargo nextest run --workspace --profile ci
}

run_gate "cargo nextest run --workspace --profile ci" \
	run_nextest

run_gate "cargo test --doc --workspace" \
	cargo test --doc --workspace

run_rustdoc() {
	RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
}
run_gate "cargo doc --no-deps --workspace (-D warnings)" \
	run_rustdoc

run_rustdoc_docsrs() {
	DOCS_RS=1 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace
}
run_gate "cargo doc --no-deps --workspace (DOCS_RS=1)" \
	run_rustdoc_docsrs

# Local runs almost always have uncommitted changes — the script's
# whole point is to verify before tagging. CI's clean checkout doesn't
# need this flag; the workflow omits it for that reason.
run_gate "Package tarball docs.rs simulation" \
	python3 "$REPO_ROOT/scripts/check_package_docsrs.py" --allow-dirty

if [[ "$RUN_PUBLIC_API" == 1 ]]; then
	if ! command -v cargo-public-api >/dev/null 2>&1; then
		log_warn "cargo-public-api not installed; skipping (install: cargo install cargo-public-api --locked)"
		SKIPPED+=("public-API baseline diff")
	elif ! rustup toolchain list | grep -q '^nightly'; then
		log_warn "rustup nightly toolchain not installed; skipping public-API diff (install: rustup toolchain install nightly)"
		SKIPPED+=("public-API baseline diff")
	else
		run_public_api_diff() {
			local fail=0
			for crate in lean-rs-sys lean-toolchain lean-rs-interop-shims lean-rs lean-rs-host \
				lean-rs-worker-protocol lean-rs-worker-parent lean-rs-worker-child; do
				local baseline="$REPO_ROOT/docs/api-review/${crate}-public.txt"
				if [[ ! -f "$baseline" ]]; then
					log_err "Missing baseline: $baseline"
					fail=1
					continue
				fi
				local actual
				actual="$(mktemp)"
				cargo public-api --simplified -p "$crate" >"$actual"
				if ! diff -u "$baseline" "$actual"; then
					log_err "Public-API drift for $crate. Regenerate $baseline before tagging."
					fail=1
				fi
				rm -f "$actual"
			done
			return "$fail"
		}
		run_gate "Public-API baseline diff" \
			run_public_api_diff
	fi
else
	SKIPPED+=("public-API baseline diff (--no-publicapi)")
fi

if [[ "$RUN_FUZZ" == 1 ]]; then
	if [[ "$OS_KIND" != "Linux" ]]; then
		log_warn "ABI fuzz smoke runs only on Linux (release.yml gates it the same way)"
		SKIPPED+=("ABI fuzz smoke (non-Linux)")
	elif ! rustup toolchain list | grep -q '^nightly'; then
		log_warn "rustup nightly toolchain not installed; skipping ABI fuzz smoke"
		SKIPPED+=("ABI fuzz smoke")
	elif ! command -v cargo-fuzz >/dev/null 2>&1; then
		log_warn "cargo-fuzz not installed; skipping ABI fuzz smoke (install: cargo install cargo-fuzz --locked)"
		SKIPPED+=("ABI fuzz smoke")
	else
		# ASAN_OPTIONS=detect_leaks=0 matches release.yml — Lean's
		# shared runtime intentionally retains process-global
		# allocations that LSan flags at exit.
		run_fuzz() {
			(
				cd "$REPO_ROOT/crates/lean-rs/fuzz" &&
					ASAN_OPTIONS=detect_leaks=0 cargo +nightly fuzz run abi_decode -- \
						-runs=200000 -max_total_time=120
			)
		}
		run_gate "ABI fuzz smoke (200k runs / 120s cap)" \
			run_fuzz
	fi
else
	SKIPPED+=("ABI fuzz smoke (--no-fuzz)")
fi

# -- summary ----------------------------------------------------------------

printf '\n%s====== Pre-release summary ======%s\n' "$BOLD" "$RESET"
printf 'Lean version: %s\n' "$LEAN_VERSION"
printf 'OS:           %s\n' "$OS_KIND"
printf 'Mode:         host\n'
printf '\npassed (%d):\n' "${#PASSED[@]}"
for name in "${PASSED[@]}"; do printf '  %s✓%s %s\n' "$GREEN" "$RESET" "$name"; done

if ((${#SKIPPED[@]} > 0)); then
	printf '\nskipped (%d):\n' "${#SKIPPED[@]}"
	for name in "${SKIPPED[@]}"; do printf '  %s-%s %s\n' "$YELLOW" "$RESET" "$name"; done
fi

if ((${#FAILED[@]} > 0)); then
	printf '\n%sfailed (%d):%s\n' "$RED" "${#FAILED[@]}" "$RESET"
	for name in "${FAILED[@]}"; do printf '  %s✗%s %s\n' "$RED" "$RESET" "$name"; done
	exit 1
fi

printf '\n%sAll gates passed.%s\n' "$GREEN" "$RESET"
