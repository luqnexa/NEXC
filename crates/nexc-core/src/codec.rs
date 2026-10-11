// SPDX-License-Identifier: AGPL-3.0-only
use crate::invalid;
use nextime_dict::{Entry, validate_entry};
use std::{collections::BTreeSet, io};

pub const BLOCK_ENTRIES: usize = 128;
pub const MAX_BLOCK_BYTES: usize = 256 * 1024;

pub fn put_uint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

pub fn put_string(out: &mut Vec<u8>, value: &str) {
    put_uint(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

pub struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    pub fn uint(&mut self) -> io::Result<u64> {
        let mut value = 0;
        for i in 0..10 {
            let byte = self.bytes(1)?[0];
            if i == 9 && byte > 1 {
                return Err(invalid("varint overflow"));
            }
            value |= u64::from(byte & 127) << (i * 7);
            if byte & 128 == 0 {
                if i > 0 && byte == 0 {
                    return Err(invalid("noncanonical varint"));
                }
                return Ok(value);
            }
        }
        Err(invalid("unterminated varint"))
    }
    pub fn size(&mut self, max: usize) -> io::Result<usize> {
        let value = self.uint()?;
        if value > max as u64 {
            return Err(invalid("encoded value exceeds limit"));
        }
        Ok(value as usize)
    }
    pub fn bytes(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| invalid("offset overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid("truncated data"))?;
        self.position = end;
        Ok(bytes)
    }
    pub fn string(&mut self, max: usize) -> io::Result<String> {
        let len = self.size(max)?;
        String::from_utf8(self.bytes(len)?.to_vec()).map_err(|_| invalid("invalid UTF-8"))
    }
    pub fn finish(&self) -> io::Result<()> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid("trailing data"))
        }
    }
}

fn put_shared(out: &mut Vec<u8>, previous: &str, current: &str) {
    let prefix = previous
        .bytes()
        .zip(current.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    put_uint(out, prefix as u64);
    put_uint(out, (current.len() - prefix) as u64);
    out.extend_from_slice(&current.as_bytes()[prefix..]);
}

fn shared(decoder: &mut Decoder<'_>, previous: &str, max: usize) -> io::Result<String> {
    let prefix = decoder.size(previous.len())?;
    let suffix = decoder.size(max.saturating_sub(prefix))?;
    let mut bytes = previous.as_bytes()[..prefix].to_vec();
    bytes.extend_from_slice(decoder.bytes(suffix)?);
    String::from_utf8(bytes).map_err(|_| invalid("invalid shared UTF-8 string"))
}

/// Stores shared byte prefixes and a per-block POS string table.
/// Prefixes may end within a UTF-8 character; the complete result is validated.
pub fn encode_block(entries: &[Entry]) -> io::Result<Vec<u8>> {
    if entries.is_empty() || entries.len() > BLOCK_ENTRIES {
        return Err(invalid("invalid block entry count"));
    }
    let pos: Vec<_> = entries
        .iter()
        .map(|e| e.part_of_speech.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut bytes = Vec::new();
    put_uint(&mut bytes, entries.len() as u64);
    put_uint(&mut bytes, pos.len() as u64);
    for item in &pos {
        put_string(&mut bytes, item);
    }
    let (mut reading, mut surface) = ("", "");
    for entry in entries {
        validate_entry(entry).map_err(invalid)?;
        if entry.reading.as_str() < reading {
            return Err(invalid("block readings are not sorted"));
        }
        put_shared(&mut bytes, reading, &entry.reading);
        put_shared(&mut bytes, surface, &entry.surface);
        put_uint(
            &mut bytes,
            pos.binary_search(&entry.part_of_speech.as_str())
                .expect("POS table contains entry") as u64,
        );
        put_uint(&mut bytes, u64::from(entry.frequency));
        reading = &entry.reading;
        surface = &entry.surface;
    }
    if bytes.len() > MAX_BLOCK_BYTES {
        return Err(invalid("block exceeds byte limit"));
    }
    Ok(bytes)
}

pub fn decode_block(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    if bytes.len() > MAX_BLOCK_BYTES {
        return Err(invalid("block exceeds byte limit"));
    }
    let mut decoder = Decoder::new(bytes);
    let count = decoder.size(BLOCK_ENTRIES)?;
    let pos_count = decoder.size(count)?;
    if count == 0 || pos_count == 0 {
        return Err(invalid("empty block or POS table"));
    }
    let mut pos = Vec::with_capacity(pos_count);
    for _ in 0..pos_count {
        pos.push(decoder.string(64)?);
    }
    if pos.iter().any(String::is_empty) || pos.windows(2).any(|p| p[0] >= p[1]) {
        return Err(invalid("invalid POS table"));
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(count);
    for _ in 0..count {
        let previous = entries.last();
        let reading = shared(
            &mut decoder,
            previous.map_or("", |e| e.reading.as_str()),
            512,
        )?;
        let surface = shared(
            &mut decoder,
            previous.map_or("", |e| e.surface.as_str()),
            1024,
        )?;
        let pos_id = decoder.size(pos_count - 1)?;
        let frequency = decoder.uint()?;
        let frequency = u32::try_from(frequency).map_err(|_| invalid("frequency overflow"))?;
        let entry = Entry {
            reading,
            surface,
            part_of_speech: pos[pos_id].clone(),
            frequency,
        };
        validate_entry(&entry).map_err(invalid)?;
        if previous.is_some_and(|e| e.reading > entry.reading) {
            return Err(invalid("unsorted block readings"));
        }
        entries.push(entry);
    }
    decoder.finish()?;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_boundaries_and_bad_encodings() {
        for value in [0, 127, 128, 16383, 16384, u32::MAX as u64, u64::MAX] {
            let mut bytes = Vec::new();
            put_uint(&mut bytes, value);
            let mut decoder = Decoder::new(&bytes);
            assert_eq!(decoder.uint().unwrap(), value);
            decoder.finish().unwrap();
        }
        for bytes in [vec![128], vec![128, 0], vec![255; 10], vec![128; 11]] {
            assert!(Decoder::new(&bytes).uint().is_err());
        }
    }
    #[test]
    fn multilingual_roundtrip_and_truncation() {
        let entries = nextime_dict::Dictionary::from_tsv("あ\t日本\tnoun\t0\nあ\t日程\tverb\t4294967295\n中\t中文🙂\tnoun\t20\n한국\t한국어\tnoun\t100").unwrap();
        let bytes = encode_block(entries.entries()).unwrap();
        assert_eq!(decode_block(&bytes).unwrap(), entries.entries());
        for end in 0..bytes.len() {
            assert!(decode_block(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_block(&trailing).is_err());
    }
    #[test]
    fn malformed_bytes_never_panic() {
        let mut seed = 123_u64;
        for length in 0..256 {
            let bytes: Vec<_> = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (seed >> 32) as u8
                })
                .collect();
            let _ = decode_block(&bytes);
        }
    }

    #[test]
    fn shared_prefix_can_split_a_unicode_character() {
        let dictionary =
            nextime_dict::Dictionary::from_tsv("a\t日\tnoun\t1\na\t旧\tnoun\t1").unwrap();
        assert_eq!(
            dictionary.entries()[0].surface.as_bytes()[..2],
            dictionary.entries()[1].surface.as_bytes()[..2]
        );
        let encoded = encode_block(dictionary.entries()).unwrap();
        assert_eq!(decode_block(&encoded).unwrap(), dictionary.entries());
    }
}
