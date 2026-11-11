#!/usr/bin/env bash
# Build the signed release APK and publish it to the APK store.
#
#   scripts/publish-android.sh              # build + publish
#   scripts/publish-android.sh --build-only # signed APK, no upload
#
# Every Android change ships this way: the store listing is how the app actually
# reaches the phone (Play and CI artifacts alone are not a delivery).
#
# Signing key (never committed):
#   ~/.config/wheezper/android-release.properties   (storeFile/storePassword/...)
#   ~/.config/wheezper/android-release.jks
# Store tooling:
#   $WHEEZPER_STORE_PUBLISH, default ../server-control-center/hello-rocket/
#   google-play-temu/scripts/publish.sh
# Store API (for the post-publish check):
#   $WHEEZPER_STORE_URL, default https://wheezper-dl.pumpkinforge.de/store
#
# Bump versionCode AND versionName in android/app/build.gradle.kts first: the
# store (and Play) key on versionCode, and an unchanged one is not a release.
set -euo pipefail
cd "$(dirname "$0")/.."

BUILD_ONLY=0
[[ "${1:-}" == "--build-only" ]] && BUILD_ONLY=1

PROPS="${WHEEZPER_ANDROID_KEYSTORE_PROPERTIES:-$HOME/.config/wheezper/android-release.properties}"
if [[ ! -f "$PROPS" ]]; then
    echo "wheezper: no signing key at $PROPS" >&2
    echo "wheezper: see android/README.md, 'Updating the app' — do not ship a debug-signed APK" >&2
    exit 1
fi

# shellcheck disable=SC1090
set -a; . "$PROPS"; set +a
export WHEEZPER_ANDROID_KEYSTORE="${storeFile:?storeFile missing from $PROPS}"
export WHEEZPER_ANDROID_KEYSTORE_PASSWORD="${storePassword:?storePassword missing from $PROPS}"
export WHEEZPER_ANDROID_KEY_ALIAS="${keyAlias:?keyAlias missing from $PROPS}"
export WHEEZPER_ANDROID_KEY_PASSWORD="${keyPassword:-$storePassword}"

cd android
./gradlew --no-daemon assembleRelease
cd ..

APK="android/app/build/outputs/apk/release/app-release.apk"
TOOLS="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Android/sdk-real}}/build-tools"
AAPT=$(ls -d "$TOOLS"/*/aapt 2>/dev/null | sort -V | tail -1 || true)
if [[ -n "${AAPT:-}" ]]; then
    BADGING=$("$AAPT" dump badging "$APK" 2>/dev/null || true)
    VERSION=$(sed -nE "s/^package:.*versionCode='([0-9]+)'.*versionName='([^']*)'.*/\1 (\2)/p" <<<"$BADGING")
    [[ -n "$VERSION" ]] && echo "wheezper: built $APK — versionCode $VERSION"
    if grep -qi "application-debuggable" <<<"$BADGING"; then
        echo "wheezper: refusing to publish a debuggable build" >&2
        exit 1
    fi
fi
sha256sum "$APK"

if [[ "$BUILD_ONLY" == 1 ]]; then
    echo "wheezper: signed APK ready: $(pwd)/$APK"
    exit 0
fi

STORE_PUBLISH="${WHEEZPER_STORE_PUBLISH:-../server-control-center/hello-rocket/google-play-temu/scripts/publish.sh}"
if [[ ! -x "$STORE_PUBLISH" ]]; then
    echo "wheezper: store publish script not found: $STORE_PUBLISH" >&2
    echo "wheezper: set WHEEZPER_STORE_PUBLISH=/path/to/publish.sh (the APK above is built and signed)" >&2
    exit 1
fi
"$STORE_PUBLISH" "$(pwd)/$APK"

# The listing is the receipt: it must show this versionCode.
STORE_URL="${WHEEZPER_STORE_URL:-https://wheezper-dl.pumpkinforge.de/store}"
if command -v curl >/dev/null && [[ -n "${VERSION:-}" ]]; then
    code="${VERSION%% *}"
    if curl -fsS --max-time 20 "$STORE_URL/api/apps" \
        | grep -q "\"package\":\"com.wheezper.app\".*\"versionCode\":$code"; then
        echo "wheezper: store listing shows com.wheezper.app versionCode $code"
    else
        echo "wheezper: WARNING could not confirm versionCode $code in $STORE_URL/api/apps" >&2
    fi
fi
