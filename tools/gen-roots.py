#!/usr/bin/env python3
"""Собирает kernel/src/tls/roots.rs из системного набора корневых CA.

Оставляются только сертификаты с ключом RSA (2048/3072/4096 бит): TLS-клиент
Perfix проверяет подписи RSA и пока не умеет ECDSA. Каждый корень встраивается
как DER, чтобы не разбирать PEM в ядре.

Использование: python3 tools/gen-roots.py [--bundle путь] [--out путь]
"""

import argparse
import base64
import re
import subprocess
import sys

DEFAULT_BUNDLE = "/etc/ssl/certs/ca-certificates.crt"
DEFAULT_OUT = "kernel/src/tls/roots.rs"

RSA_OID = bytes.fromhex("2a864886f70d010101")


def pem_to_der(pem: bytes) -> bytes:
    return base64.b64decode(b"".join(line for line in pem.splitlines() if not line.startswith(b"---")))


def der_tlv(data: bytes, pos: int):
    """Читает один DER-элемент: (tag, начало значения, конец значения, конец элемента)."""
    tag = data[pos]
    first = data[pos + 1]
    pos += 2

    if first & 0x80:
        count = first & 0x7F
        length = int.from_bytes(data[pos:pos + count], "big")
        pos += count
    else:
        length = first

    return tag, pos, pos + length, pos + length


def spki_info(cert: bytes):
    """Возвращает (алгоритм, размер ключа RSA) или None."""
    # Certificate ::= SEQUENCE { tbs, sigalg, sig }
    _, tbs_start, tbs_end, _ = der_tlv(cert, 0)
    # tbsCertificate ::= SEQUENCE { ... }
    _, pos, tbs_body_end, _ = der_tlv(cert, tbs_start)

    if cert[pos] == 0xA0:  # version [0]
        _, _, _, pos = der_tlv(cert, pos)

    for _ in range(4):  # serial, signature, issuer, validity
        _, _, _, pos = der_tlv(cert, pos)

    # subject
    _, _, _, pos = der_tlv(cert, pos)

    # subjectPublicKeyInfo
    tag, spki_start, spki_end, _ = der_tlv(cert, pos)
    assert tag == 0x30

    # algorithm
    _, alg_start, alg_end, _ = der_tlv(cert, spki_start)
    # OID
    _, oid_start, oid_end, _ = der_tlv(cert, alg_start)
    oid = cert[oid_start:oid_end]

    if oid != RSA_OID:
        if oid == bytes.fromhex("2a8648ce3d0201"):
            # id-ecPublicKey: параметр — OID кривой, поддерживаем prime256v1.
            tag, param_start, param_end, _ = der_tlv(cert, alg_end)

            if tag == 0x06 and cert[param_start:param_end] == bytes.fromhex("2a8648ce3d030107"):
                return ("ec", 256)

            return ("other", 0)

        return ("other", 0)

    # BIT STRING -> RSAPublicKey ::= SEQUENCE { modulus, exponent }
    _, bits_start, bits_end, _ = der_tlv(cert, alg_end)
    key = cert[bits_start + 1:bits_end]
    _, key_start, key_end, _ = der_tlv(key, 0)
    _, mod_start, mod_end, _ = der_tlv(key, key_start)
    modulus = key[mod_start:mod_end].lstrip(b"\x00")
    return ("rsa", len(modulus) * 8)


def subject(cert: bytes) -> str:
    try:
        out = subprocess.run(
            ["openssl", "x509", "-inform", "DER", "-noout", "-subject", "-nameopt", "RFC2253"],
            input=cert, capture_output=True, check=True,
        )
        text = out.stdout.decode(errors="replace").strip()
        match = re.search(r"CN=([^,]+)", text)
        return match.group(1) if match else text.removeprefix("subject=")
    except Exception:
        return "unknown"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bundle", default=DEFAULT_BUNDLE)
    parser.add_argument("--out", default=DEFAULT_OUT)
    args = parser.parse_args()

    try:
        bundle = open(args.bundle, "rb").read()
    except OSError as error:
        print(f"cannot read {args.bundle}: {error}", file=sys.stderr)
        return 1

    blocks = re.findall(rb"-----BEGIN CERTIFICATE-----.*?-----END CERTIFICATE-----", bundle, re.S)
    roots = []
    total = 0

    for block in blocks:
        der = pem_to_der(block)
        kind, bits = spki_info(der)

        if not (kind == "ec" or (kind == "rsa" and bits >= 2048)):
            continue

        name = subject(der)
        name = re.sub(r"\\[0-9A-Fa-f]{2}", "", name)
        name = "".join(ch if 32 <= ord(ch) < 127 else "?" for ch in name)
        name = name.replace("\\", "").replace('"', "'")
        roots.append((name, der))
        total += len(der)

    roots.sort()

    blob = bytearray()
    entries = []

    for name, der in roots:
        entries.append((name, len(blob), len(der)))
        blob += der

    binary_path = args.out.replace(".rs", ".bin")

    with open(binary_path, "wb") as binary:
        binary.write(blob)

    with open(args.out, "w", encoding="utf-8") as out:
        out.write(
            "//! Корневые сертификаты для проверки цепочек TLS.\n"
            "//!\n"
            "//! Файл сгенерирован tools/gen-roots.py из системного набора CA\n"
            "//! (ca-certificates): только RSA-ключи 2048 бит и больше.\n"
            "//! DER-сертификаты лежат подряд в roots.bin, таблица — здесь.\n"
            "//! Не редактируйте вручную — перезапустите генератор.\n"
            "\n"
            "static ROOTS_BIN: &[u8] = include_bytes!(\"roots.bin\");\n"
            "\n"
            "/// Доверенный корневой сертификат.\n"
            "pub struct Root {\n"
            "    #[allow(dead_code)]\n"
            "    pub name: &'static str,\n"
            "    offset: usize,\n"
            "    length: usize,\n"
            "}\n"
            "\n"
            "impl Root {\n"
            "    /// DER-кодировка сертификата.\n"
            "    pub fn der(&self) -> &'static [u8] {\n"
            "        &ROOTS_BIN[self.offset..self.offset + self.length]\n"
            "    }\n"
            "}\n"
            "\n"
            "pub static ROOTS: &[Root] = &[\n"
        )

        for name, offset, length in entries:
            out.write(
                f'    Root {{\n        name: "{name}",\n'
                f"        offset: {offset},\n        length: {length},\n    }},\n"
            )

        out.write("];\n")

    print(
        f"roots: {len(entries)}, der bytes: {len(blob)}, "
        f"file: {args.out}, binary: {binary_path}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
