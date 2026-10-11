// SPDX-License-Identifier: AGPL-3.0-only
use crate::{
    checksum,
    codec::{BLOCK_ENTRIES, Decoder, MAX_BLOCK_BYTES, put_string, put_uint},
    invalid,
};
use std::io;

pub const HEADER_BYTES: usize = 64;
pub const MAX_INDEX_BYTES: usize = 128 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"NUD\x01NEXC";

#[derive(Clone, Debug)]
pub struct Header {
    pub generation: u64,
    pub entry_count: u64,
    pub block_count: u32,
    pub max_reading_chars: u32,
    pub index_offset: u64,
    pub index_len: u64,
    pub index_crc: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut bytes = [0; HEADER_BYTES];
        bytes[..8].copy_from_slice(MAGIC);
        bytes[8..12].copy_from_slice(&1_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&(BLOCK_ENTRIES as u32).to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generation.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.entry_count.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.block_count.to_le_bytes());
        bytes[36..40].copy_from_slice(&self.max_reading_chars.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.index_offset.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.index_len.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.index_crc.to_le_bytes());
        let crc = checksum(&bytes[..60]);
        bytes[60..64].copy_from_slice(&crc.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8; HEADER_BYTES], file_len: u64) -> io::Result<Self> {
        let u32_at = |offset| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed header"))
        };
        let u64_at = |offset| {
            u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("fixed header"))
        };
        if &bytes[..8] != MAGIC || u32_at(8) != 1 || u32_at(12) != BLOCK_ENTRIES as u32 {
            return Err(invalid("unsupported NUD magic/version/block size"));
        }
        if checksum(&bytes[..60]) != u32_at(60) {
            return Err(invalid("header checksum mismatch"));
        }
        let header = Self {
            generation: u64_at(16),
            entry_count: u64_at(24),
            block_count: u32_at(32),
            max_reading_chars: u32_at(36),
            index_offset: u64_at(40),
            index_len: u64_at(48),
            index_crc: u32_at(56),
        };
        if header.index_offset < HEADER_BYTES as u64
            || header.index_len > MAX_INDEX_BYTES as u64
            || header.index_offset.checked_add(header.index_len) != Some(file_len)
            || header.block_count as u64 > header.index_len / 11
            || header.max_reading_chars > 128
            || (header.entry_count == 0) != (header.block_count == 0)
            || (header.entry_count == 0) != (header.max_reading_chars == 0)
        {
            return Err(invalid("invalid header sizes or counts"));
        }
        Ok(header)
    }
}

#[derive(Clone, Debug)]
pub struct BlockIndex {
    pub first: String,
    pub last: String,
    pub offset: u64,
    pub length: u32,
    pub entries: u32,
    pub crc: u32,
}

pub fn encode_index(index: &[BlockIndex]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in index {
        put_string(&mut bytes, &item.first);
        put_string(&mut bytes, &item.last);
        put_uint(&mut bytes, item.offset);
        put_uint(&mut bytes, u64::from(item.length));
        put_uint(&mut bytes, u64::from(item.entries));
        bytes.extend_from_slice(&item.crc.to_le_bytes());
    }
    bytes
}

pub fn decode_index(bytes: &[u8], header: &Header) -> io::Result<Vec<BlockIndex>> {
    if checksum(bytes) != header.index_crc {
        return Err(invalid("index checksum mismatch"));
    }
    let mut decoder = Decoder::new(bytes);
    let mut index: Vec<BlockIndex> = Vec::new();
    let mut offset = HEADER_BYTES as u64;
    let mut entries = 0_u64;
    for _ in 0..header.block_count {
        let first = decoder.string(512)?;
        let last = decoder.string(512)?;
        let item_offset = decoder.uint()?;
        let length = decoder.size(MAX_BLOCK_BYTES)? as u32;
        let count = decoder.size(BLOCK_ENTRIES)? as u32;
        let crc = u32::from_le_bytes(decoder.bytes(4)?.try_into().expect("four bytes"));
        if first.is_empty()
            || last.is_empty()
            || first > last
            || length == 0
            || count == 0
            || item_offset != offset
            || index.last().is_some_and(|previous| previous.last > first)
            || first.chars().count() > header.max_reading_chars as usize
            || last.chars().count() > header.max_reading_chars as usize
            || first.chars().chain(last.chars()).any(char::is_control)
        {
            return Err(invalid("invalid block index"));
        }
        offset = offset
            .checked_add(u64::from(length))
            .ok_or_else(|| invalid("block offset overflow"))?;
        if offset > header.index_offset {
            return Err(invalid("block overlaps index"));
        }
        entries += u64::from(count);
        index.push(BlockIndex {
            first,
            last,
            offset: item_offset,
            length,
            entries: count,
            crc,
        });
    }
    decoder.finish()?;
    if offset != header.index_offset || entries != header.entry_count {
        return Err(invalid("index totals mismatch"));
    }
    Ok(index)
}
