#!/bin/sh
# The cargo runner on macOS (.cargo/config.toml).
#
# The linker signs a build ad hoc, and the Keychain ties "Always Allow" on the
# API token to the code hash of an ad hoc binary, so every rebuild asks for the
# login password again. A certificate gives all builds one identity.
# scripts/create-dev-cert.sh creates it; without it the binary runs as built.
keychain="$HOME/Library/Keychains/shrew-dev.keychain-db"
if [ -e "$keychain" ]; then
    security unlock-keychain -p shrew "$keychain" &&
        codesign --force --sign "Shrew Dev" --keychain "$keychain" "$1" 2>/dev/null
fi
exec "$@"
