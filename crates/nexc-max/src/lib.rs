// SPDX-License-Identifier: AGPL-3.0-only
//! Offline deterministic NUD builder. Sorting still requires all entries in RAM.
use nexc_core::{
    checksum,
    codec::{BLOCK_ENTRIES, encode_block},
    format::{BlockIndex, HEADER_BYTES, Header, MAX_INDEX_BYTES, encode_index},
    invalid,
};
use nextime_dict::{Dictionary, Entry};
use std::{
    fs::{self, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct BuildStats {
    pub entries: usize,
    pub blocks: usize,
    pub bytes: u64,
    pub index_bytes: usize,
}

pub fn build(
    path: impl AsRef<Path>,
    entries: Vec<Entry>,
    generation: u64,
) -> io::Result<BuildStats> {
    let dictionary = Dictionary::from_entries(entries).map_err(|e| invalid(e.to_string()))?;
    build_dictionary(path, &dictionary, generation)
}

/// Publish a new immutable file without replacing an existing destination.
/// A same-directory temporary file is synced before a no-replace hard link.
pub fn build_dictionary(
    path: impl AsRef<Path>,
    dictionary: &Dictionary,
    generation: u64,
) -> io::Result<BuildStats> {
    let path = path.as_ref();
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(
        ".building-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(temporary);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let mut cleanup = Temporary {
        path: temporary,
        file: Some(file),
    };
    let file = cleanup.file.as_mut().expect("open temporary file");
    file.write_all(&[0; HEADER_BYTES])?;
    let mut index = Vec::new();
    for block in dictionary.entries().chunks(BLOCK_ENTRIES) {
        let payload = encode_block(block)?;
        let offset = file.stream_position()?;
        file.write_all(&payload)?;
        index.push(BlockIndex {
            first: block[0].reading.clone(),
            last: block[block.len() - 1].reading.clone(),
            offset,
            length: payload.len() as u32,
            entries: block.len() as u32,
            crc: checksum(&payload),
        });
    }
    let index_offset = file.stream_position()?;
    let index_bytes = encode_index(&index);
    if index_bytes.len() > MAX_INDEX_BYTES {
        return Err(invalid("index exceeds NUD v1 limit"));
    }
    file.write_all(&index_bytes)?;
    let header = Header {
        generation,
        entry_count: dictionary.len() as u64,
        block_count: u32::try_from(index.len()).map_err(|_| invalid("too many blocks"))?,
        max_reading_chars: dictionary.max_reading_chars() as u32,
        index_offset,
        index_len: index_bytes.len() as u64,
        index_crc: checksum(&index_bytes),
    };
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header.encode())?;
    file.sync_all()?;
    drop(cleanup.file.take());
    fs::hard_link(&cleanup.path, path)?;
    let stats = BuildStats {
        entries: dictionary.len(),
        blocks: index.len(),
        bytes: index_offset + index_bytes.len() as u64,
        index_bytes: index_bytes.len(),
    };
    // Dropping cleanup removes only the temporary link, never the published file.
    drop(cleanup);
    Ok(stats)
}

struct Temporary {
    path: PathBuf,
    file: Option<std::fs::File>,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
