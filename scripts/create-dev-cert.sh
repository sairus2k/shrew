#!/bin/sh
# Creates the self-signed certificate that scripts/dev-run.sh signs dev builds with.
#
# It lives in a keychain of its own with a fixed password, so signing never
# brings up a dialog: a key in the login keychain asks for access to it. The
# password guards nothing but the identity of local dev builds.
set -eu

name="Shrew Dev"
keychain="$HOME/Library/Keychains/shrew-dev.keychain-db"
password=shrew

if [ -e "$keychain" ]; then
    echo "$keychain already exists"
    exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

cat >"$dir/openssl.cnf" <<CONF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
CONF

# The system LibreSSL: the PKCS#12 file of OpenSSL 3 is one `security import` cannot read
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -config "$dir/openssl.cnf" -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -name "$name" -passout pass:"$password" \
    -inkey "$dir/key.pem" -in "$dir/cert.pem" -out "$dir/identity.p12"

security create-keychain -p "$password" "$keychain"
# No auto-lock
security set-keychain-settings "$keychain"
security import "$dir/identity.p12" -k "$keychain" -P "$password" -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain" >/dev/null

echo "Created the \"$name\" certificate in $keychain"
