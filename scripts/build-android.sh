#!/usr/bin/env bash
# Build the Magnetar Android app: native lib (cargo-ndk), Kotlin bindings (uniffi), APKs (Gradle).
#
#   scripts/build-android.sh            # debug + release APKs
#   SKIP_NATIVE=1 scripts/build-android.sh   # reuse jniLibs/ and the generated bindings
#
# Outputs:
#   android/app/build/outputs/apk/debug/app-debug.apk
#   android/app/build/outputs/apk/release/app-release.apk  (signed with android/keystore/release.jks)
#
# The release key is generated on first run into android/keystore/ (gitignored). Keep that
# directory: Android refuses to update an installed app signed with a different key.
#
# Env overrides: ANDROID_HOME, ANDROID_NDK_HOME, GRADLE_USER_HOME, CARGO_BUILD_JOBS, GRADLE_TASKS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT/android"
APP_MAIN="$ANDROID_DIR/app/src/main"

# Arch's /usr/bin/cargo has no Android std; rustup's toolchain does (android.md §0).
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$ANDROID_HOME/ndk/29.0.14206865}"

for tool in cargo cargo-ndk keytool; do
    command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done
[[ -d "$ANDROID_NDK_HOME" ]] || { echo "NDK not found at $ANDROID_NDK_HOME" >&2; exit 1; }

cd "$ROOT"

if [[ "${SKIP_NATIVE:-0}" != 1 ]]; then
    echo "==> magnetar-ffi for arm64-v8a + x86_64 (release)"
    cargo ndk -t arm64-v8a -t x86_64 -P 26 -o "$APP_MAIN/jniLibs" \
        build --release -p magnetar-ffi --lib

    # Bindings come from the unstripped host build: stripping removes the uniffi metadata.
    echo "==> Kotlin bindings (package app.magnetar.ffi)"
    cargo build -p magnetar-ffi --lib
    rm -rf "$APP_MAIN/java/app/magnetar/ffi"
    cargo run -q -p uniffi-bindgen -- generate \
        --library "$ROOT/target/debug/libmagnetar_ffi.so" \
        --language kotlin --out-dir "$APP_MAIN/java" --no-format
fi

[[ -f "$APP_MAIN/java/app/magnetar/ffi/magnetar_ffi.kt" ]] || { echo "bindings missing; run without SKIP_NATIVE" >&2; exit 1; }

# Local release key (not a Play key; this app is side-loaded).
KEYSTORE_DIR="$ANDROID_DIR/keystore"
if [[ ! -f "$KEYSTORE_DIR/keystore.properties" ]]; then
    echo "==> generating local release keystore in android/keystore/"
    mkdir -p "$KEYSTORE_DIR"
    chmod 700 "$KEYSTORE_DIR"
    PASS="$(head -c 24 /dev/urandom | base64 | tr -d '/+=')"
    keytool -genkeypair -noprompt -keystore "$KEYSTORE_DIR/release.jks" -storetype PKCS12 \
        -alias magnetar -keyalg RSA -keysize 4096 -validity 10000 \
        -storepass "$PASS" -keypass "$PASS" -dname "CN=Magnetar local release"
    cat >"$KEYSTORE_DIR/keystore.properties" <<EOF
storeFile=release.jks
storePassword=$PASS
keyAlias=magnetar
keyPassword=$PASS
EOF
    chmod 600 "$KEYSTORE_DIR/keystore.properties" "$KEYSTORE_DIR/release.jks"
fi

[[ -f "$ANDROID_DIR/local.properties" ]] || echo "sdk.dir=$ANDROID_HOME" >"$ANDROID_DIR/local.properties"

echo "==> Gradle ${GRADLE_TASKS:-assembleDebug assembleRelease}"
cd "$ANDROID_DIR"
# shellcheck disable=SC2086
./gradlew --no-daemon ${GRADLE_TASKS:-assembleDebug assembleRelease}

echo "==> APKs"
ls -l app/build/outputs/apk/*/*.apk
