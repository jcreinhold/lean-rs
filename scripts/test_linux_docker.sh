#!/usr/bin/env bash
# Disposable, link-free Linux tests. No host caches or credentials enter the container.
set -euo pipefail
usage() {
    cat <<'HELP'
Usage: scripts/test_linux_docker.sh [--platform linux/amd64|linux/arm64]
       [--package NAME] [--test NAME | --lib] [--filter TEXT]
Defaults: linux/amd64, lean-rs-worker-parent, --test lifetime_bootstrap.
Packages: lean-rs-worker-parent, lean-rs-worker-protocol, lean-rs-abi.
Optional filters select libtest names; they are not shell or Cargo arguments.
Requires Docker Desktop and GNU timeout (timeout or gtimeout).
HELP
}
fail() { printf '%s\n' "$*" >&2; exit 2; }
platform=linux/amd64
package=lean-rs-worker-parent
target=(--test lifetime_bootstrap)
filter=
while (($#)); do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --platform|--package|--test|--filter)
            (($# >= 2)) || fail "Missing value for $1"
            key=$1; value=$2; shift 2
            case "$key" in
                --platform) platform=$value ;;
                --package) package=$value ;;
                --test) [[ $value =~ ^[a-zA-Z0-9_]+$ ]] || fail 'Invalid test name'; target=(--test "$value") ;;
                --filter) [[ -n $value && $value != -* && ${#value} -le 256 ]] || fail 'Invalid filter'; filter=$value ;;
            esac ;;
        --lib) target=(--lib); shift ;;
        *) fail "Unknown argument: $1" ;;
    esac
done
case "$platform" in linux/amd64|linux/arm64) ;; *) fail 'Unsupported platform' ;; esac
case "$package" in
    lean-rs-worker-parent|lean-rs-worker-protocol|lean-rs-abi) ;;
    *) fail 'Only the documented link-free packages are supported' ;;
esac
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
command -v docker >/dev/null || fail 'Docker is required'
# Select Docker Desktop explicitly, never an inherited remote Docker endpoint.
[[ $(uname -s) == Darwin ]] || fail 'This runner requires Docker Desktop on macOS'
unset DOCKER_HOST DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_CERT_PATH
endpoint=$(docker --context desktop-linux context inspect desktop-linux --format '{{.Endpoints.docker.Host}}') ||
    fail 'Docker Desktop context desktop-linux is unavailable'
case "$endpoint" in unix:///*) ;; *) fail 'Docker Desktop must use a local Unix socket' ;; esac
test_args=(-p "$package" "${target[@]}")
if [[ -n $filter ]]; then test_args+=("$filter"); fi
if command -v timeout >/dev/null; then timer=timeout
elif command -v gtimeout >/dev/null; then timer=gtimeout
else fail 'GNU timeout is required (timeout or gtimeout)'; fi
# 25 GiB reserve plus 5 GiB headroom for the image and Docker bookkeeping.
free_kib=$(df -Pk "$repo" | awk 'END {print $4}')
[[ $free_kib =~ ^[0-9]+$ ]] || fail 'Cannot read free disk space'
((free_kib >= 30 * 1024 * 1024)) || fail 'Need at least 30 GiB free (25 GiB reserve plus headroom)'
name="lean-rs-linux-test-$$-$(date +%s)"
cleanup() { "$timer" 20s docker --context desktop-linux rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf 'Platform: %s; disposable /lab: 3 GiB; deadline: 8 minutes (test: 6 minutes)\n' "$platform"
"$timer" --signal=TERM --kill-after=15s 480s docker --context desktop-linux run \
    --name "$name" --platform "$platform" --rm --init --read-only \
    --cap-drop=ALL --security-opt=no-new-privileges \
    --cpus=2 --memory=4g --memory-swap=4g --pids-limit=192 \
    --mount "type=bind,source=$repo,target=/src,readonly" \
    --tmpfs /lab:rw,exec,nosuid,nodev,size=3g \
    --tmpfs /tmp:rw,noexec,nosuid,nodev,size=16m,mode=1777 \
    --workdir /src \
    --env CARGO_HOME=/lab/cargo --env CARGO_TARGET_DIR=/lab/target --env TMPDIR=/lab/tmp \
    --env HOME=/lab/home --env CARGO_INCREMENTAL=0 \
    --env RUSTUP_TOOLCHAIN=1.100.0 --env RUSTUP_AUTO_INSTALL=0 \
    docker.io/library/rust:1.100.0-bookworm \
    bash -ceu 'mkdir -p /lab/cargo /lab/target /lab/tmp /lab/home; exec timeout --signal=TERM --kill-after=10s 360s cargo test --locked -j1 "$@" -- --test-threads=1' \
    bash "${test_args[@]}"
