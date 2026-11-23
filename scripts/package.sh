#!/bin/sh
# Builds the release binary, assembles target/package/Shrew.app and wraps it in
# a .dmg (DESIGN.md §9). Needs the Xcode Command Line Tools.
#
#   SHREW_SIGN_IDENTITY    Developer ID Application identity to sign with, as
#                          listed by `security find-identity -v -p codesigning`.
#   SHREW_NOTARY_PROFILE   keychain profile made by `xcrun notarytool
#                          store-credentials`; needs SHREW_SIGN_IDENTITY.
#
# A step whose variable is not set is skipped, and the result is then named
# Shrew-NOT-FOR-RELEASE.dmg: Gatekeeper blocks it on other Macs. Only a run
# that signed and notarized produces target/package/Shrew.dmg.
set -eu

cd "$(dirname "$0")/.."

identity=${SHREW_SIGN_IDENTITY:-}
profile=${SHREW_NOTARY_PROFILE:-}

if [ -n "$profile" ] && [ -z "$identity" ]; then
    echo "SHREW_NOTARY_PROFILE needs SHREW_SIGN_IDENTITY: Apple rejects an unsigned build." >&2
    exit 1
fi
case "$identity" in
"Developer ID Application:"*) ;;
"")
    ;;
*)
    if [ -n "$profile" ]; then
        echo "Apple notarizes only code signed with a Developer ID Application identity." >&2
        exit 1
    fi
    ;;
esac

out=target/package
app=$out/Shrew.app
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -n 1)

# Gives the build the profile of the installed app (app_core::APP_ID)
SHREW_PACKAGED=1 cargo build --release --locked -p app

rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/app "$app/Contents/MacOS/Shrew"
cp assets/icons/Shrew.icns "$app/Contents/Resources/Shrew.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Shrew</string>
    <key>CFBundleDisplayName</key><string>Shrew</string>
    <key>CFBundleExecutable</key><string>Shrew</string>
    <key>CFBundleIdentifier</key><string>dev.shrew.app</string>
    <key>CFBundleIconFile</key><string>Shrew</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>15.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist"

if [ -n "$identity" ]; then
    codesign --force --timestamp --options runtime --sign "$identity" "$app"
    codesign --verify --deep --strict "$app"
fi

mkdir "$out/dmg-root"
cp -R "$app" "$out/dmg-root/"
ln -s /Applications "$out/dmg-root/Applications"
if [ -n "$identity" ] && [ -n "$profile" ]; then
    dmg=$out/Shrew.dmg
else
    dmg=$out/Shrew-NOT-FOR-RELEASE.dmg
fi
hdiutil create -volname Shrew -srcfolder "$out/dmg-root" -ov -format UDZO "$dmg"
rm -r "$out/dmg-root"

if [ -n "$identity" ]; then
    codesign --force --timestamp --sign "$identity" "$dmg"
fi
if [ -n "$profile" ]; then
    xcrun notarytool submit "$dmg" --keychain-profile "$profile" --wait
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
fi

echo
if [ -n "$identity" ] && [ -n "$profile" ]; then
    echo "Release build: $dmg"
else
    echo "*** NOT A RELEASE BUILD: $dmg ***"
    [ -n "$identity" ] || echo "*** Not signed (SHREW_SIGN_IDENTITY is not set). ***"
    [ -n "$profile" ] || echo "*** Not notarized or stapled (SHREW_NOTARY_PROFILE is not set). ***"
    echo "*** Gatekeeper blocks it on any other Mac. ***"
fi
