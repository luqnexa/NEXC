// SPDX-License-Identifier: AGPL-3.0-only
//! Independently implemented NUD v1 block codec and bounds-checked format.
pub mod codec;
pub mod format;

use std::io;

pub fn invalid(reason: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.into())
}

/// CRC-32/ISO-HDLC detects accidental corruption; it is not authentication.
pub fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc = CRC_TABLE[((crc ^ u32::from(byte)) & 255) as usize] ^ (crc >> 8);
    }
    !crc
}

const CRC_TABLE: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    #[test]
    fn checksum_known_vector() {
        assert_eq!(super::checksum(b"123456789"), 0xcbf4_3926);
    }
}
