#!/bin/bash
# The box's TLS: a CA of its own, and the relay's certificate under it, in /keys (the volume
# mic-testbox-keys). Clients trust the CA (`--connect-tls-root /keys/ca.pem`), as they would a
# real one. Made once: a replayed pod keeps its keys, and `down --wipe` removes them.
set -euo pipefail
cd /keys
if [[ -e relay.pem ]]; then
    exit 0
fi
curve=(-newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes)
openssl req -x509 "${curve[@]}" -days 3650 -subj /CN=mic-testbox-ca \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign \
    -keyout ca.key -out ca.pem 2>/dev/null
openssl req "${curve[@]}" -subj "/CN=${RELAY_HOST}" -keyout relay.key -out relay.csr 2>/dev/null
openssl x509 -req -in relay.csr -CA ca.pem -CAkey ca.key -days 3650 -out relay.pem \
    -extfile <(printf 'subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n' "${RELAY_HOST}") \
    2>/dev/null
rm relay.csr ca.key
chmod 0444 ca.pem relay.pem
echo "keys: a CA and a certificate for ${RELAY_HOST}"
