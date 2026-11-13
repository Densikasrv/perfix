//! Корневые сертификаты для проверки цепочек TLS.
//!
//! Файл сгенерирован tools/gen-roots.py из системного набора CA
//! (ca-certificates): только RSA-ключи 2048 бит и больше.
//! DER-сертификаты лежат подряд в roots.bin, таблица — здесь.
//! Не редактируйте вручную — перезапустите генератор.

static ROOTS_BIN: &[u8] = include_bytes!("roots.bin");

/// Доверенный корневой сертификат.
pub struct Root {
    #[allow(dead_code)]
    pub name: &'static str,
    offset: usize,
    length: usize,
}

impl Root {
    /// DER-кодировка сертификата.
    pub fn der(&self) -> &'static [u8] {
        &ROOTS_BIN[self.offset..self.offset + self.length]
    }

    /// Длина PEM-представления этого сертификата.
    fn pem_len(&self) -> usize {
        pem_length(self.length)
    }

    /// Записывает сертификат в PEM без завершающего NUL.
    fn write_pem(&self, out: &mut [u8]) -> Option<usize> {
        let mut length = 0usize;
        push(out, &mut length, b"-----BEGIN CERTIFICATE-----\n")?;

        let der = self.der();
        let mut offset = 0usize;
        let mut encoded = [0u8; 64];

        // 48 байт DER дают ровно 64 символа base64 — одну строку PEM.
        while offset < der.len() {
            let count = (der.len() - offset).min(48);
            let encoded_len = encode_base64(&der[offset..offset + count], &mut encoded)?;
            push(out, &mut length, &encoded[..encoded_len])?;
            push(out, &mut length, b"\n")?;
            offset += count;
        }

        push(out, &mut length, b"-----END CERTIFICATE-----\n")?;
        Some(length)
    }
}

/// Длина PEM-бандла со всеми корневыми сертификатами.
pub fn bundle_size() -> usize {
    ROOTS.iter().map(Root::pem_len).sum()
}

/// Записывает все корневые сертификаты в PEM-бандл (формат
/// `/etc/ssl/certs/ca-certificates.crt`).
pub fn write_bundle(out: &mut [u8]) -> Option<usize> {
    let mut length = 0usize;

    for root in ROOTS {
        let written = root.write_pem(&mut out[length..])?;
        length += written;
    }

    Some(length)
}

fn pem_length(der_len: usize) -> usize {
    let base64_len = (der_len + 2) / 3 * 4;
    28 + base64_len + (base64_len + 63) / 64 + 26
}

fn push(out: &mut [u8], length: &mut usize, data: &[u8]) -> Option<()> {
    if *length + data.len() > out.len() {
        return None;
    }

    out[*length..*length + data.len()].copy_from_slice(data);
    *length += data.len();
    Some(())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(input: &[u8], out: &mut [u8]) -> Option<usize> {
    if input.is_empty() || input.len() > 48 || out.len() < 64 {
        return None;
    }

    let mut length = 0usize;
    let mut index = 0usize;

    while index + 3 <= input.len() {
        let value = ((input[index] as u32) << 16)
            | ((input[index + 1] as u32) << 8)
            | input[index + 2] as u32;
        out[length] = BASE64[((value >> 18) & 0x3f) as usize];
        out[length + 1] = BASE64[((value >> 12) & 0x3f) as usize];
        out[length + 2] = BASE64[((value >> 6) & 0x3f) as usize];
        out[length + 3] = BASE64[(value & 0x3f) as usize];
        length += 4;
        index += 3;
    }

    match input.len() - index {
        1 => {
            let value = (input[index] as u32) << 16;
            out[length] = BASE64[((value >> 18) & 0x3f) as usize];
            out[length + 1] = BASE64[((value >> 12) & 0x3f) as usize];
            out[length + 2] = b'=';
            out[length + 3] = b'=';
            length += 4;
        }
        2 => {
            let value = ((input[index] as u32) << 16) | ((input[index + 1] as u32) << 8);
            out[length] = BASE64[((value >> 18) & 0x3f) as usize];
            out[length + 1] = BASE64[((value >> 12) & 0x3f) as usize];
            out[length + 2] = BASE64[((value >> 6) & 0x3f) as usize];
            out[length + 3] = b'=';
            length += 4;
        }
        _ => {}
    }

    Some(length)
}

pub static ROOTS: &[Root] = &[
    Root {
        name: "ACCVRAIZ1",
        offset: 0,
        length: 2007,
    },
    Root {
        name: "ANF Secure Server Root CA",
        offset: 2007,
        length: 1523,
    },
    Root {
        name: "Actalis Authentication Root CA",
        offset: 3530,
        length: 1471,
    },
    Root {
        name: "Amazon Root CA 1",
        offset: 5001,
        length: 837,
    },
    Root {
        name: "Amazon Root CA 2",
        offset: 5838,
        length: 1349,
    },
    Root {
        name: "Atos TrustedRoot Root CA RSA TLS 2021",
        offset: 7187,
        length: 1384,
    },
    Root {
        name: "Autoridad de Certificacion Firmaprofesional CIF A62634068",
        offset: 8571,
        length: 1560,
    },
    Root {
        name: "BJCA Global Root CA1",
        offset: 10131,
        length: 1400,
    },
    Root {
        name: "Buypass Class 2 Root CA",
        offset: 11531,
        length: 1373,
    },
    Root {
        name: "Buypass Class 3 Root CA",
        offset: 12904,
        length: 1373,
    },
    Root {
        name: "CA Disig Root R2",
        offset: 14277,
        length: 1389,
    },
    Root {
        name: "CFCA EV ROOT",
        offset: 15666,
        length: 1425,
    },
    Root {
        name: "COMODO RSA Certification Authority",
        offset: 17091,
        length: 1500,
    },
    Root {
        name: "Certainly Root R1",
        offset: 18591,
        length: 1355,
    },
    Root {
        name: "Certigna Root CA",
        offset: 19946,
        length: 1631,
    },
    Root {
        name: "Certum Trusted Network CA",
        offset: 21577,
        length: 959,
    },
    Root {
        name: "Certum Trusted Network CA 2",
        offset: 22536,
        length: 1494,
    },
    Root {
        name: "Certum Trusted Root CA",
        offset: 24030,
        length: 1476,
    },
    Root {
        name: "D-TRUST BR Root CA 2 2023",
        offset: 25506,
        length: 1453,
    },
    Root {
        name: "D-TRUST EV Root CA 2 2023",
        offset: 26959,
        length: 1453,
    },
    Root {
        name: "D-TRUST Root Class 3 CA 2 2009",
        offset: 28412,
        length: 1079,
    },
    Root {
        name: "D-TRUST Root Class 3 CA 2 EV 2009",
        offset: 29491,
        length: 1095,
    },
    Root {
        name: "DigiCert Assured ID Root G2",
        offset: 30586,
        length: 922,
    },
    Root {
        name: "DigiCert Global Root G2",
        offset: 31508,
        length: 914,
    },
    Root {
        name: "DigiCert TLS RSA4096 Root G5",
        offset: 32422,
        length: 1386,
    },
    Root {
        name: "DigiCert Trusted Root G4",
        offset: 33808,
        length: 1428,
    },
    Root {
        name: "GDCA TrustAUTH R5 ROOT",
        offset: 35236,
        length: 1420,
    },
    Root {
        name: "GTS Root R1",
        offset: 36656,
        length: 1371,
    },
    Root {
        name: "GlobalSign",
        offset: 38027,
        length: 867,
    },
    Root {
        name: "GlobalSign",
        offset: 38894,
        length: 1415,
    },
    Root {
        name: "GlobalSign Root R46",
        offset: 40309,
        length: 1374,
    },
    Root {
        name: "Go Daddy Root Certificate Authority - G2",
        offset: 41683,
        length: 969,
    },
    Root {
        name: "HARICA TLS RSA Root CA 2021",
        offset: 42652,
        length: 1448,
    },
    Root {
        name: "Hellenic Academic and Research Institutions RootCA 2015",
        offset: 44100,
        length: 1551,
    },
    Root {
        name: "HiPKI Root CA - G1",
        offset: 45651,
        length: 1390,
    },
    Root {
        name: "Hongkong Post Root CA 3",
        offset: 47041,
        length: 1491,
    },
    Root {
        name: "ISRG Root X1",
        offset: 48532,
        length: 1391,
    },
    Root {
        name: "IdenTrust Commercial Root CA 1",
        offset: 49923,
        length: 1380,
    },
    Root {
        name: "IdenTrust Public Sector Root CA 1",
        offset: 51303,
        length: 1386,
    },
    Root {
        name: "Izenpe.com",
        offset: 52689,
        length: 1525,
    },
    Root {
        name: "Microsec e-Szigno Root CA 2009",
        offset: 54214,
        length: 1038,
    },
    Root {
        name: "Microsoft RSA Root Certificate Authority 2017",
        offset: 55252,
        length: 1452,
    },
    Root {
        name: "NAVER Global Root Certification Authority",
        offset: 56704,
        length: 1446,
    },
    Root {
        name: "NetLock Arany (Class Gold) Ftanstvny",
        offset: 58150,
        length: 1049,
    },
    Root {
        name: "OISTE Server Root RSA G1",
        offset: 59199,
        length: 1415,
    },
    Root {
        name: "OISTE WISeKey Global Root GB CA",
        offset: 60614,
        length: 953,
    },
    Root {
        name: "OU=AC RAIZ FNMT-RCM,O=FNMT-RCM,C=ES",
        offset: 61567,
        length: 1415,
    },
    Root {
        name: "OU=Security Communication RootCA2,O=SECOM Trust Systems CO.,LTD.,C=JP",
        offset: 62982,
        length: 891,
    },
    Root {
        name: "OU=certSIGN ROOT CA G2,O=CERTSIGN SA,C=RO",
        offset: 63873,
        length: 1355,
    },
    Root {
        name: "QuoVadis Root CA 1 G3",
        offset: 65228,
        length: 1380,
    },
    Root {
        name: "QuoVadis Root CA 2 G3",
        offset: 66608,
        length: 1380,
    },
    Root {
        name: "QuoVadis Root CA 3 G3",
        offset: 67988,
        length: 1380,
    },
    Root {
        name: "SECOM TLS RSA Root CA 2024",
        offset: 69368,
        length: 1438,
    },
    Root {
        name: "SSL.com EV Root Certification Authority RSA R2",
        offset: 70806,
        length: 1519,
    },
    Root {
        name: "SSL.com Root Certification Authority RSA",
        offset: 72325,
        length: 1505,
    },
    Root {
        name: "SSL.com TLS RSA Root CA 2022",
        offset: 73830,
        length: 1421,
    },
    Root {
        name: "SZAFIR ROOT CA2",
        offset: 75251,
        length: 886,
    },
    Root {
        name: "Sectigo Public Server Authentication Root R46",
        offset: 76137,
        length: 1422,
    },
    Root {
        name: "SecureSign Root CA14",
        offset: 77559,
        length: 1398,
    },
    Root {
        name: "Starfield Root Certificate Authority - G2",
        offset: 78957,
        length: 993,
    },
    Root {
        name: "Starfield Services Root Certificate Authority - G2",
        offset: 79950,
        length: 1011,
    },
    Root {
        name: "SwissSign RSA TLS Root CA 2022 - 1",
        offset: 80961,
        length: 1431,
    },
    Root {
        name: "T-TeleSec GlobalRoot Class 2",
        offset: 82392,
        length: 967,
    },
    Root {
        name: "T-TeleSec GlobalRoot Class 3",
        offset: 83359,
        length: 967,
    },
    Root {
        name: "TUBITAK Kamu SM SSL Kok Sertifikasi - Surum 1",
        offset: 84326,
        length: 1127,
    },
    Root {
        name: "TWCA CYBER Root CA",
        offset: 85453,
        length: 1425,
    },
    Root {
        name: "TWCA Global Root CA",
        offset: 86878,
        length: 1349,
    },
    Root {
        name: "TWCA Root Certification Authority",
        offset: 88227,
        length: 895,
    },
    Root {
        name: "Telekom Security TLS RSA Root 2023",
        offset: 89122,
        length: 1463,
    },
    Root {
        name: "Telia RSA TLS Root CA v3",
        offset: 90585,
        length: 1414,
    },
    Root {
        name: "Telia Root CA v2",
        offset: 91999,
        length: 1400,
    },
    Root {
        name: "TrustAsia Global Root CA G3",
        offset: 93399,
        length: 1449,
    },
    Root {
        name: "TrustAsia TLS RSA Root CA",
        offset: 94848,
        length: 1412,
    },
    Root {
        name: "TunTrust Root CA",
        offset: 96260,
        length: 1463,
    },
    Root {
        name: "UCA Extended Validation Root",
        offset: 97723,
        length: 1374,
    },
    Root {
        name: "UCA Global G2 Root",
        offset: 99097,
        length: 1354,
    },
    Root {
        name: "USERTrust RSA Certification Authority",
        offset: 100451,
        length: 1506,
    },
    Root {
        name: "emSign Root CA - G1",
        offset: 101957,
        length: 920,
    },
    Root {
        name: "vTrus Root CA",
        offset: 102877,
        length: 1370,
    },
];
