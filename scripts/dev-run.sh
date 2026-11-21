#!/bin/sh
# The cargo runner on macOS (.cargo/config.toml).
#
# The Keychain ties "Always Allow" on the API token to the partition of the
# code that asked. For a build the linker signed ad hoc, or one signed with a
# certificate Apple did not issue, the partition is the code hash, so every
# rebuild asks for the login password again. For an Apple-issued certificate it
# is the team, which all builds share. README describes how to get one; without
# it codesign fails and the binary runs as built.
codesign --force --sign "Apple Development" "$1" 2>/dev/null
exec "$@"
