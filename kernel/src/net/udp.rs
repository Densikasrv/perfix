//! UDP: минимальная отправка и приём датаграмм.
//!
//! Контрольная сумма UDP для IPv4 необязательна (ноль означает
//! «не проверять»), поэтому она не считается. Этого достаточно для
//! DNS и других простых протоколов в user-mode сети QEMU.

use super::{get_u16, put_u16, send_ipv4};

const IP_PROTO_UDP: u8 = 17;

/// Отправляет UDP-датаграмму.
pub fn send(destination: [u8; 4], source_port: u16, destination_port: u16, payload: &[u8]) -> bool {
    if payload.len() > 1472 {
        return false;
    }

    let mut packet = [0u8; 1480];
    put_u16(&mut packet, 0, source_port);
    put_u16(&mut packet, 2, destination_port);
    put_u16(&mut packet, 4, (8 + payload.len()) as u16);
    put_u16(&mut packet, 6, 0);
    packet[8..8 + payload.len()].copy_from_slice(payload);

    send_ipv4(destination, IP_PROTO_UDP, &packet[..8 + payload.len()])
}

/// Разбирает входящую UDP-датаграмму и передаёт её обработчику порта.
pub fn handle(payload: &[u8], source_ip: [u8; 4]) {
    if payload.len() < 8 {
        return;
    }

    let source_port = get_u16(payload, 0);
    let destination_port = get_u16(payload, 2);
    let length = get_u16(payload, 4) as usize;

    if length < 8 || length > payload.len() {
        return;
    }

    let data = &payload[8..length];

    super::dns::handle(source_ip, source_port, destination_port, data);
}
