//! Общие сведения об USB-устройствах.
//!
//! Разбор device/configuration descriptor'ов вынесен из контроллеров
//! UHCI и EHCI: идентификаторы VID/PID, класс устройства и bulk-эндпоинты
//! нужны обоим драйверам. Здесь же хранится список известных устройств,
//! которым пока не соответствует полноценный драйвер: в частности,
//! USB Wi-Fi адаптер TL-WN727N на Realtek RTL8188EUS.

/// Классы устройств, которые различает USB-стек.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    None,
    Keyboard,
    Mouse,
    /// USB Wi-Fi адаптер. На этом этапе устройство только опознаётся,
    /// 802.11 и WPA2 появятся позже.
    Wifi,
    /// Устройство перечислено, но драйвера для него нет.
    Unsupported,
}

/// Идентификаторы USB-устройства из device descriptor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Identity {
    pub vendor: u16,
    pub product: u16,
}

impl Identity {
    pub const EMPTY: Self = Self {
        vendor: 0,
        product: 0,
    };
}

/// Bulk-эндпоинт из configuration descriptor.
///
/// `address` хранится целиком, вместе с битом направления: младшие четыре
/// бита — номер эндпоинта, бит 0x80 — передача IN.
#[derive(Clone, Copy)]
pub(crate) struct BulkEndpoint {
    pub address: u8,
    pub max_packet: u16,
}

fn get_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Читает VID/PID из device descriptor.
pub(crate) fn identity(descriptor: &[u8]) -> Identity {
    if descriptor.len() < 12 {
        return Identity::EMPTY;
    }

    Identity {
        vendor: get_u16(descriptor, 8),
        product: get_u16(descriptor, 10),
    }
}

/// Известный Wi-Fi адаптер: TL-WN727N (Realtek RTL8188EUS).
pub(crate) fn is_wifi(identity: Identity) -> bool {
    identity.vendor == 0x0bda && identity.product == 0x8179
}

/// Человекочитаемое имя известного устройства.
pub(crate) fn name(identity: Identity) -> Option<&'static [u8]> {
    if is_wifi(identity) {
        Some(b"RTL8188EUS Wi-Fi (TL-WN727N)")
    } else {
        None
    }
}

/// Находит первый bulk IN и первый bulk OUT эндпоинты в конфигурации.
///
/// Vendor-specific устройства, к которым относится RTL8188EUS, не имеют
/// HID-интерфейсов, поэтому HID-разбор им не подходит. Bulk-эндпоинты
/// понадобятся драйверу Wi-Fi на следующем этапе; сейчас они только
/// запоминаются при перечислении.
pub(crate) fn find_bulk(
    configuration: &[u8],
) -> (Option<BulkEndpoint>, Option<BulkEndpoint>) {
    let mut bulk_in = None;
    let mut bulk_out = None;
    let mut offset = 0usize;

    while offset + 2 <= configuration.len() {
        let length = configuration[offset] as usize;
        let descriptor_type = configuration[offset + 1];

        if length < 2 || offset + length > configuration.len() {
            break;
        }

        if descriptor_type == 0x05 && length >= 7 {
            let address = configuration[offset + 2];
            let attributes = configuration[offset + 3];
            let max_packet =
                get_u16(configuration, offset + 4) & 0x07ff;

            if attributes & 0x03 == 0x02 {
                let endpoint = BulkEndpoint {
                    address,
                    max_packet,
                };

                if address & 0x80 != 0 {
                    if bulk_in.is_none() {
                        bulk_in = Some(endpoint);
                    }
                } else if bulk_out.is_none() {
                    bulk_out = Some(endpoint);
                }
            }
        }

        offset += length;
    }

    (bulk_in, bulk_out)
}
