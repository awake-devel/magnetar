#!/usr/bin/env bash
# Build a static (musl) magnetar-server for Linux.
#
#   scripts/build-linux.sh <x86_64|aarch64> [--docker | --native] [--out DIR]
#
# Writes DIR/magnetar-server-<arch>-linux (default DIR: dist/). The build is native: the
# target arch must be the host arch (no QEMU, no cross toolchain). CI builds aarch64 on an
# arm64 runner.
#
#   --docker  (default when docker works) build inside rust:<ver>-alpine, where musl is the
#             native libc. Needs nothing but docker on the host.
#   --native  build with the host's cargo for <arch>-unknown-linux-musl. Needs a toolchain
#             that has that target (rustup, which adds it) and a musl C compiler for
#             ring/aws-lc: Alpine: apk add rustup musl-dev gcc (Alpine's own `rust` package
#             only has <arch>-alpine-linux-musl); Debian/Ubuntu: apt install musl-tools.
#
# Env: RUST_IMAGE (default rust:1.99-alpine), CARGO_BUILD_JOBS.
# Fails unless the result is statically linked (no ELF interpreter, no NEEDED libraries).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
RUST_IMAGE=${RUST_IMAGE:-rust:1.99-alpine}

usage() { sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

arch=
mode=
out=$root/dist
while [ $# -gt 0 ]; do
  case "$1" in
    x86_64 | aarch64) arch=$1 ;;
    amd64) arch=x86_64 ;;
    arm64) arch=aarch64 ;;
    --docker) mode=docker ;;
    --native) mode=native ;;
    --out) out=${2:?--out needs a directory}; shift ;;
    -h | --help) usage ;;
    *) echo "unknown argument: $1" >&2; usage 2 ;;
  esac
  shift
done
[ -n "$arch" ] || usage 2

host=$(uname -m)
case "$host" in arm64) host=aarch64 ;; amd64) host=x86_64 ;; esac
[ "$host" = "$arch" ] || die "host is $host, target is $arch: build on a $arch machine (CI uses a native arm64 runner; no emulation is assumed)"

triple=$arch-unknown-linux-musl
case "$arch" in x86_64) platform=linux/amd64 ;; aarch64) platform=linux/arm64 ;; esac
if [ -z "$mode" ]; then
  if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then mode=docker; else mode=native; fi
fi

# Separate target dir: the musl objects must not mix with the host's glibc ones.
target_dir=$root/target/linux-musl
mkdir -p "$target_dir" "$out"
jobs=${CARGO_BUILD_JOBS:-}

case "$mode" in
  docker)
    echo "==> $RUST_IMAGE ($platform): cargo build --release --locked -p magnetar-server --target $triple"
    # Runs as root (apk needs it); the trap hands target/linux-musl back to the caller.
    docker run --rm --platform "$platform" \
      -v "$root:/src" -w /src \
      -e CARGO_TARGET_DIR=/src/target/linux-musl \
      -e CARGO_HOME=/src/target/linux-musl/cargo-home \
      -e CARGO_BUILD_JOBS="$jobs" \
      -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
      "$RUST_IMAGE" sh -euc '
        trap "chown -R \$HOST_UID:\$HOST_GID /src/target/linux-musl" EXIT
        [ -n "$CARGO_BUILD_JOBS" ] || unset CARGO_BUILD_JOBS
        apk add --no-cache musl-dev >/dev/null
        rustc -vV | sed -n "s/^host: //p"
        cargo build --release --locked -p magnetar-server --target '"$triple"
    ;;
  native)
    command -v cargo >/dev/null || die "cargo not found (install rustup, or use --docker)"
    if command -v rustup >/dev/null 2>&1; then
      rustup target list --installed | grep -qx "$triple" || rustup target add "$triple"
    fi
    # Without rustup the target must already be there: a distro rustc (Alpine's `rust` is
    # <arch>-alpine-linux-musl only) would otherwise fail deep in the build on `core`.
    libdir=$(rustc --print target-libdir --target "$triple" 2>/dev/null) || libdir=
    ls "$libdir"/libcore-*.rlib >/dev/null 2>&1 ||
      die "rustc $(rustc -vV 2>/dev/null | sed -n 's/^host: //p') has no standard library for $triple: install rustup (Alpine: apk add rustup && rustup-init), or use --docker"
    # On a glibc host the musl C compiler is musl-gcc (musl-tools); on Alpine plain cc is musl.
    cc_var=CC_${triple//-/_}
    if [ -z "${!cc_var:-}" ] && ! ldd --version 2>&1 | grep -qi musl; then
      command -v musl-gcc >/dev/null || die "no musl C compiler: apt install musl-tools (or set $cc_var), or use --docker"
      export "$cc_var=musl-gcc"
    fi
    [ -n "$jobs" ] || unset CARGO_BUILD_JOBS
    echo "==> cargo build --release --locked -p magnetar-server --target $triple"
    (cd "$root" && CARGO_TARGET_DIR=$target_dir cargo build --release --locked -p magnetar-server --target "$triple")
    ;;
esac

src=$target_dir/$triple/release/magnetar-server
dst=$out/magnetar-server-$arch-linux
[ -f "$src" ] || die "build produced no $src"
cp -f "$src" "$dst"
chmod 755 "$dst"

# Static check without depending on `file`: no PT_INTERP and no DT_NEEDED.
elf_info() {
  if command -v readelf >/dev/null 2>&1; then
    readelf -lW "$dst" | grep -q 'Requesting program interpreter' && echo dynamic-interp
    readelf -dW "$dst" 2>/dev/null | grep '(NEEDED)' | sed 's/.*\[\(.*\)\]/needs \1/'
  elif command -v file >/dev/null 2>&1; then
    file -b "$dst" | grep -q 'statically linked\|static-pie linked' || echo "not static: $(file -b "$dst")"
  else
    echo "cannot check: neither readelf nor file is installed" >&2
  fi
}
problems=$(elf_info || true)

echo
echo "$dst"
command -v file >/dev/null 2>&1 && echo "  file     $(file -b "$dst")"
echo "  size     $(du -h "$dst" | cut -f1) ($(wc -c <"$dst" | tr -d ' ') bytes)"
echo "  sha256   $(sha256sum "$dst" | cut -d' ' -f1)"
echo "  version  $("$dst" --version)"
if [ -n "$problems" ]; then
  printf '  ERROR: %s\n' "$problems" >&2
  exit 1
fi
echo "  static   yes"
