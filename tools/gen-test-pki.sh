#!/usr/bin/env bash
# Готовит тестовый удостоверяющий центр для локальной проверки TLS.
#
# Создаёт корневой и серверный сертификаты (RSA-2048/SHA-256) с SAN
# IP:10.0.2.2 и DNS:perfix.test и кладёт DER корня в
# kernel/src/tls/dev-root.der. Этот корень принимается только при сборке
# с флагом KERNEL_FEATURES=dev-root:
#
#     bash tools/gen-test-pki.sh
#     KERNEL_FEATURES=dev-root bash tools/build-iso.sh
#     openssl s_server -accept 4433 -tls1_3 -www \
#         -cert /tmp/perfix-tls-pki/server.pem \
#         -key /tmp/perfix-tls-pki/server.key
#
# В обычной сборке (без флага) тестовый корень не встраивается, поэтому
# подделать сертификат им нельзя.
set -euo pipefail

cd "$(dirname "$0")/.."

DIR="${1:-/tmp/perfix-tls-pki}"
mkdir -p "$DIR"

if ! command -v openssl >/dev/null 2>&1; then
    echo "error: не найден openssl" >&2
    exit 1
fi

openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 3650 \
    -keyout "$DIR/root.key" -out "$DIR/root.pem" \
    -subj "/CN=Perfix Test Root CA" \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    >/dev/null 2>&1

openssl req -newkey rsa:2048 -sha256 -nodes \
    -keyout "$DIR/server.key" -out "$DIR/server.csr" \
    -subj "/CN=10.0.2.2" >/dev/null 2>&1

cat > "$DIR/server.ext" <<'EOF'
subjectAltName=IP:10.0.2.2,DNS:perfix.test
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
EOF

openssl x509 -req -in "$DIR/server.csr" -CA "$DIR/root.pem" -CAkey "$DIR/root.key" \
    -CAcreateserial -out "$DIR/server.pem" -days 825 -sha256 \
    -extfile "$DIR/server.ext" >/dev/null 2>&1

# Сертификат с чужим именем — для проверки отказа в доверии.
openssl req -newkey rsa:2048 -sha256 -nodes \
    -keyout "$DIR/wrong.key" -out "$DIR/wrong.csr" \
    -subj "/CN=wrong.example" >/dev/null 2>&1

cat > "$DIR/wrong.ext" <<'EOF'
subjectAltName=DNS:wrong.example
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
EOF

openssl x509 -req -in "$DIR/wrong.csr" -CA "$DIR/root.pem" -CAkey "$DIR/root.key" \
    -CAcreateserial -out "$DIR/wrong.pem" -days 825 -sha256 \
    -extfile "$DIR/wrong.ext" >/dev/null 2>&1

openssl x509 -in "$DIR/root.pem" -outform DER -out "$DIR/dev-root.der"
cp "$DIR/dev-root.der" kernel/src/tls/dev-root.der

echo "pki: $DIR"
echo "server: $DIR/server.pem (SAN IP:10.0.2.2, DNS:perfix.test)"
echo "wrong:  $DIR/wrong.pem  (SAN DNS:wrong.example)"
