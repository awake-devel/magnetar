#!/usr/bin/env bash
# Cross-compile magnetar-server for Termux with cargo-ndk (API 24, Termux's own level).
#
#   scripts/build-termux.sh [--target arm64-v8a|x86_64]... [--out DIR]
#
# Default: both ABIs into dist/ as magnetar-server-aarch64-linux-android and
# magnetar-server-x86_64-linux-android. Fails if a binary needs anything beyond bionic
# libc/libm/libdl, since Termux would not find e.g. libc++_shared.so for it.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# /usr/bin/cargo (Arch) has no Android std: the rustup toolchain must come first.
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-$HOME/Android/Sdk/ndk/29.0.14206865}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}

out=$root/dist
abis=()
while [ $# -gt 0 ]; do
  case "$1" in
    --target) abis+=("$2"); shift ;;
    --out) out=$2; shift ;;
    -h | --help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done
[ ${#abis[@]} -gt 0 ] || abis=(arm64-v8a x86_64)

[ -d "$ANDROID_NDK_HOME" ] || { echo "NDK not found: $ANDROID_NDK_HOME" >&2; exit 1; }
command -v cargo-ndk >/dev/null || { echo "cargo-ndk not on PATH (expected in ~/.local/bin)" >&2; exit 1; }
readelf=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-readelf

triple_of() {
  case "$1" in
    arm64-v8a) echo aarch64-linux-android ;;
    x86_64) echo x86_64-linux-android ;;
    *) echo "unsupported ABI $1 (arm64-v8a or x86_64)" >&2; return 1 ;;
  esac
}

args=()
for abi in "${abis[@]}"; do
  triple_of "$abi" >/dev/null
  args+=(-t "$abi")
done

cd "$root"
echo "==> cargo ndk ${args[*]} --platform 24 build --release -p magnetar-server"
cargo ndk "${args[@]}" --platform 24 build --release -p magnetar-server

target_dir=${CARGO_TARGET_DIR:-$root/target}
mkdir -p "$out"
status=0
for abi in "${abis[@]}"; do
  triple=$(triple_of "$abi")
  src=$target_dir/$triple/release/magnetar-server
  dst=$out/magnetar-server-$triple
  cp -f "$src" "$dst"
  chmod 755 "$dst"
  needed=$("$readelf" -d "$dst" | sed -n 's/.*(NEEDED).*\[\(.*\)\]/\1/p' | sort | tr '\n' ' ')
  interp=$("$readelf" -lW "$dst" | sed -n 's/.*Requesting program interpreter: \(.*\)\]/\1/p')
  echo
  echo "$dst"
  echo "  size     $(du -h "$dst" | cut -f1) ($(stat -c %s "$dst") bytes)"
  echo "  sha256   $(sha256sum "$dst" | cut -d' ' -f1)"
  echo "  interp   $interp"
  echo "  NEEDED   $needed"
  for lib in $needed; do
    case "$lib" in
      libc.so | libm.so | libdl.so) ;;
      *) echo "  ERROR: $lib is not a bionic system library; Termux cannot load it" >&2; status=1 ;;
    esac
  done
done
exit "$status"
