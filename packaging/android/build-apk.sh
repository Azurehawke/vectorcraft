#!/usr/bin/env bash
# Build VectorCraft for Android (arm64) as a signed APK: target/android-apk/vectorcraft.apk.
#
# Needs: the Android SDK (ANDROID_HOME or ANDROID_SDK_ROOT, with build-tools and
# platforms;android-34), the NDK (ANDROID_NDK_HOME), `rustup target add aarch64-linux-android`
# and `cargo install cargo-ndk`. No Gradle: aapt2 links the manifest and icon, the library is
# added to the archive, then zipalign and apksigner finish it.
#
# Signing: ANDROID_KEYSTORE (a path) with ANDROID_KEYSTORE_PASSWORD and ANDROID_KEY_ALIAS when
# set; otherwise the debug key next to this script (password "android"). Android only installs
# an update signed with the same key as the installed app.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
HERE="$ROOT/packaging/android"
OUT="$ROOT/target/android-apk"
SDK=${ANDROID_HOME:-${ANDROID_SDK_ROOT:?set ANDROID_HOME to the Android SDK}}
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to the Android NDK}"

API=34       # targetSdk: 35 would force edge-to-edge drawing under the system bars.
MIN_API=30   # Android 11: All files access (MANAGE_EXTERNAL_STORAGE).
BUILD_TOOLS=$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)
PLATFORM="$SDK/platforms/android-$API/android.jar"
if [ ! -f "$PLATFORM" ]; then
    yes | "$SDK/cmdline-tools/latest/bin/sdkmanager" "platforms;android-$API" >/dev/null
fi
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
VERSION_CODE=${VERSION_CODE:-1}

rm -rf "$OUT"
mkdir -p "$OUT/jniLibs" "$OUT/res/mipmap-xxxhdpi" "$OUT/apk"

echo "== libvectorcraft_android.so"
(cd "$ROOT" && cargo ndk -t arm64-v8a --platform "$MIN_API" -o "$OUT/jniLibs" build -p vectorcraft-android --release)
test -f "$OUT/jniLibs/arm64-v8a/libvectorcraft_android.so"

echo "== resources and manifest"
# The launcher icon: an adaptive icon (teal background, document-and-nib foreground) drawn
# from packaging/android/icon.svg, and a whole-icon PNG for launchers that want one.
cp -r "$HERE/res/." "$OUT/res/"
"$BUILD_TOOLS/aapt2" compile --dir "$OUT/res" -o "$OUT/res.zip"
"$BUILD_TOOLS/aapt2" link -o "$OUT/unaligned.apk" -I "$PLATFORM" \
    --manifest "$HERE/AndroidManifest.xml" \
    --min-sdk-version "$MIN_API" --target-sdk-version "$API" \
    --version-code "$VERSION_CODE" --version-name "$VERSION" \
    "$OUT/res.zip"

echo "== activity class"
# VectorCraftActivity: NativeActivity plus the updater's download and install (java/).
mkdir -p "$OUT/classes" "$OUT/dex"
javac --release 11 -classpath "$PLATFORM" -d "$OUT/classes" $(find "$HERE/java" -name '*.java')
"$BUILD_TOOLS/d8" --release --min-api "$MIN_API" --lib "$PLATFORM" --output "$OUT/dex" $(find "$OUT/classes" -name '*.class')
cp "$OUT/dex/classes.dex" "$OUT/apk/"

echo "== native library"
# Only VectorCraft's library: cargo-ndk also copies dependencies' own dylib builds (ocrs, boa_engine),
# which libvectorcraft_android.so doesn't load.
mkdir -p "$OUT/apk/lib/arm64-v8a"
cp "$OUT/jniLibs/arm64-v8a/libvectorcraft_android.so" "$OUT/apk/lib/arm64-v8a/"
(cd "$OUT/apk" && zip -qr "$OUT/unaligned.apk" lib classes.dex)

echo "== align and sign"
"$BUILD_TOOLS/zipalign" -p -f 4 "$OUT/unaligned.apk" "$OUT/aligned.apk"
KS=${ANDROID_KEYSTORE:-$HERE/debug.keystore}
KS_PASS=${ANDROID_KEYSTORE_PASSWORD:-android}
ALIAS=${ANDROID_KEY_ALIAS:-androiddebugkey}
"$BUILD_TOOLS/apksigner" sign --ks "$KS" --ks-key-alias "$ALIAS" --ks-pass "pass:$KS_PASS" --key-pass "pass:$KS_PASS" \
    --out "$OUT/vectorcraft.apk" "$OUT/aligned.apk"
"$BUILD_TOOLS/apksigner" verify "$OUT/vectorcraft.apk"
ls -la "$OUT/vectorcraft.apk"
