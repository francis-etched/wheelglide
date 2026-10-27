#!/usr/bin/env bash
# One-time: create a self-signed code-signing identity for wheelglide in its own keychain under .local/signing/
# (git-ignored). install.sh signs with it, so macOS keeps Accessibility / Input Monitoring across rebuilds.
# Idempotent: does nothing if the identity already exists. Remove .local/signing to start over.
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="$PWD/.local/signing"
KC="$DIR/wheelglide-signing.keychain-db"
NAME="WheelGlide Local Signing"

if [[ -f "$DIR/identity.sha1" ]]; then
  echo "identity already exists: $(cat "$DIR/identity.sha1")"
  exit 0
fi

umask 077
mkdir -p "$DIR"
/usr/bin/openssl rand -hex 24 > "$DIR/keychain-password"
PW="$(cat "$DIR/keychain-password")"

cat > "$DIR/certificate.cnf" <<EOF
[req]
distinguished_name=dn
x509_extensions=signing
prompt=no
[dn]
CN=$NAME
[signing]
basicConstraints=critical,CA:false
keyUsage=critical,digitalSignature
extendedKeyUsage=critical,codeSigning
subjectKeyIdentifier=hash
EOF

# LibreSSL (/usr/bin/openssl) writes a PKCS#12 that macOS `security import` reads.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$DIR/certificate.cnf" \
  -keyout "$DIR/private-key.pem" -out "$DIR/certificate.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -name "$NAME" -inkey "$DIR/private-key.pem" -in "$DIR/certificate.pem" \
  -out "$DIR/identity.p12" -passout "pass:$PW"

security create-keychain -p "$PW" "$KC"
security set-keychain-settings "$KC"          # no auto-lock
security unlock-keychain -p "$PW" "$KC"
security import "$DIR/identity.p12" -k "$KC" -P "$PW" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PW" "$KC" >/dev/null

SHA1="$(security find-identity -p codesigning "$KC" | awk -v n="\"$NAME\"" 'index($0, n) { print $2; exit }')"
[[ -n "$SHA1" ]] || { echo "could not find the imported identity" >&2; exit 1; }
echo "$SHA1" > "$DIR/identity.sha1"
rm -f "$DIR/private-key.pem" "$DIR/identity.p12"   # the key now lives only in the keychain
echo "created identity $SHA1 ($NAME) in $KC"
