// SPDX-License-Identifier: AGPL-3.0-only
//! Bounded block reads plus a durable append-only local word journal.
mod journal;
pub use journal::LiteDictionary;

use nexc_core::{
    checksum,
    codec::decode_block,
    format::{BlockIndex, HEADER_BYTES, Header, decode_index},
    invalid,
};
use nextime_dict::{DictionarySource, Entry, LookupError, sort_matches, sort_predictions};
use std::{
    borrow::Cow,
    collections::VecDeque,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub struct FileDictionary {
    state: Mutex<FileState>,
    header: Header,
    index: Vec<BlockIndex>,
    entries: usize,
    index_encoded_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

#[derive(Debug)]
struct FileState {
    file: File,
    cache: VecDeque<(u64, Arc<Vec<Entry>>)>,
    cache_blocks: usize,
    stats: CacheStats,
}

impl FileDictionary {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_cache(path, 8)
    }
    pub fn open_with_cache(path: impl AsRef<Path>, cache_blocks: usize) -> io::Result<Self> {
        if cache_blocks > 32 {
            return Err(invalid("cache block count must be 0..32"));
        }
        let mut file = File::open(path)?;
        let mut bytes = [0; HEADER_BYTES];
        file.read_exact(&mut bytes)?;
        let header = Header::decode(&bytes, file.metadata()?.len())?;
        let entries = usize::try_from(header.entry_count)
            .map_err(|_| invalid("entry count exceeds platform size"))?;
        file.seek(SeekFrom::Start(header.index_offset))?;
        let mut bytes = vec![0; header.index_len as usize];
        file.read_exact(&mut bytes)?;
        let index = decode_index(&bytes, &header)?;
        Ok(Self {
            state: Mutex::new(FileState {
                file,
                cache: VecDeque::new(),
                cache_blocks,
                stats: CacheStats::default(),
            }),
            index_encoded_bytes: bytes.len(),
            header,
            index,
            entries,
        })
    }
    pub fn generation(&self) -> u64 {
        self.header.generation
    }
    pub fn block_count(&self) -> usize {
        self.index.len()
    }
    pub fn index_encoded_bytes(&self) -> usize {
        self.index_encoded_bytes
    }
    /// Estimate of index-owned storage, not process RSS or peak allocations.
    pub fn index_memory_bytes(&self) -> usize {
        self.index.capacity() * std::mem::size_of::<BlockIndex>()
            + self
                .index
                .iter()
                .map(|i| i.first.capacity() + i.last.capacity())
                .sum::<usize>()
    }

    pub fn cache_stats(&self) -> io::Result<CacheStats> {
        Ok(self
            .state
            .lock()
            .map_err(|_| invalid("dictionary mutex poisoned"))?
            .stats)
    }
    pub fn clear_cache(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("dictionary mutex poisoned"))?;
        state.cache.clear();
        state.stats = CacheStats::default();
        Ok(())
    }

    fn read_block(&self, item: &BlockIndex) -> io::Result<Arc<Vec<Entry>>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| invalid("dictionary mutex poisoned"))?;
        if let Some(position) = state
            .cache
            .iter()
            .position(|(offset, _)| *offset == item.offset)
        {
            let cached = state.cache.remove(position).expect("cache position exists");
            let entries = Arc::clone(&cached.1);
            state.cache.push_back(cached);
            state.stats.hits = state.stats.hits.saturating_add(1);
            return Ok(entries);
        }
        state.stats.misses = state.stats.misses.saturating_add(1);
        let mut bytes = vec![0; item.length as usize];
        state.file.seek(SeekFrom::Start(item.offset))?;
        state.file.read_exact(&mut bytes)?;
        if checksum(&bytes) != item.crc {
            return Err(invalid("block checksum mismatch"));
        }
        let entries = decode_block(&bytes)?;
        if entries.len() != item.entries as usize
            || entries[0].reading != item.first
            || entries[entries.len() - 1].reading != item.last
            || entries
                .iter()
                .any(|e| e.reading.chars().count() > self.header.max_reading_chars as usize)
        {
            return Err(invalid("block metadata mismatch"));
        }
        let entries = Arc::new(entries);
        if state.cache_blocks > 0 {
            if state.cache.len() >= state.cache_blocks {
                state.cache.pop_front();
            }
            state.cache.push_back((item.offset, Arc::clone(&entries)));
        }
        Ok(entries)
    }

    /// Validate and stream all entries with at most one decoded block in RAM.
    /// The callback is invoked before later blocks have been validated.
    pub fn for_each_entry(
        &self,
        mut callback: impl FnMut(Entry) -> io::Result<()>,
    ) -> io::Result<()> {
        for block in &self.index {
            for entry in self.read_block(block)?.iter() {
                callback(entry.clone())?;
            }
        }
        Ok(())
    }

    pub fn validate_all(&self) -> io::Result<()> {
        self.for_each_entry(|_| Ok(()))
    }

    /// Check an identity without collecting an unbounded homophone result list.
    pub fn contains_entry_key(&self, entry: &Entry) -> io::Result<bool> {
        let start = self.index.partition_point(|b| b.last < entry.reading);
        for item in self.index[start..]
            .iter()
            .take_while(|b| b.first <= entry.reading)
        {
            if self.read_block(item)?.iter().any(|e| {
                e.reading == entry.reading
                    && e.surface == entry.surface
                    && e.part_of_speech == entry.part_of_speech
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl DictionarySource for FileDictionary {
    fn lookup_batch(
        &self,
        readings: &[&str],
        limit: usize,
    ) -> Result<Vec<Vec<Entry>>, LookupError> {
        if readings.len() > 128 {
            return Err(LookupError("batch exceeds 128 readings".into()));
        }
        let mut result = vec![Vec::new(); readings.len()];
        if limit == 0 {
            return Ok(result);
        }
        let mut groups = std::collections::BTreeMap::<usize, Vec<usize>>::new();
        for (query, reading) in readings.iter().enumerate() {
            let start = self
                .index
                .partition_point(|block| block.last.as_str() < *reading);
            for (offset, _) in self.index[start..]
                .iter()
                .take_while(|block| block.first.as_str() <= *reading)
                .enumerate()
            {
                groups.entry(start + offset).or_default().push(query);
            }
        }
        for (block, queries) in groups {
            let entries = self
                .read_block(&self.index[block])
                .map_err(|e| LookupError(e.to_string()))?;
            for query in queries {
                result[query].extend(
                    entries
                        .iter()
                        .filter(|entry| entry.reading == readings[query])
                        .cloned(),
                );
                sort_matches(&mut result[query], limit);
            }
        }
        Ok(result)
    }
    fn predict(&self, prefix: &str, limit: usize) -> Result<Vec<Entry>, LookupError> {
        if prefix.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let start = self.index.partition_point(|b| b.last.as_str() < prefix);
        let mut result = Vec::new();
        for block in self.index[start..].iter().take(4) {
            if block.first.as_str() > prefix && !block.first.starts_with(prefix) {
                break;
            }
            let entries = self
                .read_block(block)
                .map_err(|e| LookupError(e.to_string()))?;
            result.extend(
                entries
                    .iter()
                    .filter(|e| e.reading.starts_with(prefix))
                    .cloned(),
            );
            sort_predictions(&mut result, limit);
        }
        Ok(result)
    }
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError> {
        let operation = || -> io::Result<Vec<Entry>> {
            if limit == 0 {
                return Ok(Vec::new());
            }
            let start = self
                .index
                .partition_point(|block| block.last.as_str() < reading);
            let mut matches = Vec::new();
            for item in self.index[start..]
                .iter()
                .take_while(|block| block.first.as_str() <= reading)
            {
                for entry in self.read_block(item)?.iter() {
                    if entry.reading == reading {
                        matches.push(entry.clone());
                    }
                }
                // All homophone blocks are visited, but only the best K matches retained.
                sort_matches(&mut matches, limit);
            }
            Ok(matches)
        };
        operation()
            .map(Cow::Owned)
            .map_err(|e| LookupError(e.to_string()))
    }
    fn len(&self) -> usize {
        self.entries
    }
    fn max_reading_chars(&self) -> usize {
        self.header.max_reading_chars as usize
    }
}
