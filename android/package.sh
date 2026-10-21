#!/usr/bin/env bash
# cargo-apk can't declare a service or ship java code, so this adds them to the apk it built:
# compiles android/src into classes.dex, patches the manifest, adds the launcher icon, and repackages and signs the apk.
#
# usage: android/package.sh <apk> <keystore> <keystore password>
set -euo pipefail

apk="$(realpath "$1")"
keystore="$(realpath "$2")"
password="$3"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

sdk="${ANDROID_HOME:-/opt/android-sdk}"
build_tools="$(ls -d "$sdk"/build-tools/*/ | sort -V | tail -1)"
platform="$sdk/platforms/android-34"
[ -d "$platform" ] || platform="$(ls -d "$sdk"/platforms/android-*/ | sort -V | tail -1)"
android_jar="$platform/android.jar"

# the plain text manifest cargo-apk compiled the apk's from
manifest="$(dirname "$apk")/AndroidManifest.xml"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

javac --release 11 -classpath "$android_jar" -d "$tmp/classes" \
    $(find "$here/src" -name '*.java')
mkdir "$tmp/dex"
"$build_tools/d8" --min-api 26 --lib "$android_jar" --output "$tmp/dex" \
    $(find "$tmp/classes" -name '*.class')

sed -e 's|android:hasCode="false"|android:hasCode="true"|' \
    -e 's|<application |<application android:icon="@mipmap/ic_launcher" |' \
    -e 's|</application>|<service android:name="cash.small.bitcoin.ForegroundService" android:exported="false" android:foregroundServiceType="specialUse"><property android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE" android:value="listens for trade messages over nostr while offers or trades are open"/></service></application>|' \
    "$manifest" > "$tmp/AndroidManifest.xml"
grep -q ForegroundService "$tmp/AndroidManifest.xml"
grep -q ic_launcher "$tmp/AndroidManifest.xml"

# the launcher icon, rendered from docs/static/logo.svg into android/res
"$build_tools/aapt2" compile --dir "$here/res" -o "$tmp/res.zip"

# an apk with the new manifest and the icon, then the native libraries and the dex on top
"$build_tools/aapt2" link --manifest "$tmp/AndroidManifest.xml" -I "$android_jar" "$tmp/res.zip" -o "$tmp/base.apk"
mkdir "$tmp/files"
unzip -q "$apk" 'lib/*' -d "$tmp/files"
cp "$tmp/dex/classes.dex" "$tmp/files/"
(cd "$tmp/files" && zip -q -r -0 "$tmp/base.apk" .)

"$build_tools/zipalign" -p -f 4 "$tmp/base.apk" "$tmp/aligned.apk"
"$build_tools/apksigner" sign --ks "$keystore" --ks-pass "pass:$password" --out "$apk" "$tmp/aligned.apk"
echo "added the foreground service to $apk"
