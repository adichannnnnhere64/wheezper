#!/usr/bin/env bash
# Build the Android companion app.
#
#   ./scripts/build-android.sh              # debug APK
#   ./scripts/build-android.sh assembleRelease
#
# Needs JDK 17 and an Android SDK (platform 36, build-tools 36). When
# ANDROID_HOME is set and android/local.properties is absent, this writes the
# sdk.dir it needs; local.properties stays gitignored.
set -euo pipefail
cd "$(dirname "$0")/../android"

if [[ ! -f local.properties ]]; then
    if [[ -n "${ANDROID_HOME:-}" ]]; then
        printf 'sdk.dir=%s\n' "$ANDROID_HOME" > local.properties
    elif [[ -n "${ANDROID_SDK_ROOT:-}" ]]; then
        printf 'sdk.dir=%s\n' "$ANDROID_SDK_ROOT" > local.properties
    else
        echo "wheezper: set ANDROID_HOME or write android/local.properties (sdk.dir=...)" >&2
        exit 1
    fi
fi

./gradlew --no-daemon "${1:-assembleDebug}"
