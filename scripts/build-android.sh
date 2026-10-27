#!/usr/bin/env bash
# Build the Magnetar Android app (docs/APP.md §10): plain Gradle, no Rust, no NDK.
#
#   scripts/build-android.sh                                  # unit tests + debug and release APKs
#   GRADLE_TASKS="assembleDebug testDebugUnitTest" scripts/build-android.sh
#
# Outputs:
#   android/app/build/outputs/apk/debug/app-debug.apk
#   android/app/build/outputs/apk/release/app-release.apk  (signed with android/keystore/release.jks)
#
# The release key is generated on first run into android/keystore/ (gitignored). Keep that
# directory: Android refuses to update an installed app signed with a different key.
#
# Env overrides: ANDROID_HOME, GRADLE_USER_HOME, GRADLE_TASKS.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT/android"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"

command -v keytool >/dev/null || { echo "missing keytool (JDK)" >&2; exit 1; }
[[ -d "$ANDROID_HOME" ]] || { echo "Android SDK not found at $ANDROID_HOME" >&2; exit 1; }

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

echo "==> Gradle ${GRADLE_TASKS:-testDebugUnitTest assembleDebug assembleRelease}"
cd "$ANDROID_DIR"
# shellcheck disable=SC2086
./gradlew --no-daemon ${GRADLE_TASKS:-testDebugUnitTest assembleDebug assembleRelease}

echo "==> APKs"
ls -l app/build/outputs/apk/*/*.apk
