// SPDX-License-Identifier: AGPL-3.0-only
use crate::FileDictionary;
use nexc_core::{
    checksum,
    codec::{MAX_BLOCK_BYTES, decode_block, encode_block},
    invalid,
};
use nextime_dict::{DictionarySource, Entry, LookupError, sort_matches, validate_entry};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"NXL1\0\0\0\0";
const MAX_RECORD_BYTES: usize = 2048;
type Key = (String, String, String);

#[derive(Debug)]
pub struct LiteDictionary {
    base: FileDictionary,
    words: BTreeMap<Key, Entry>,
    journal: File,
    lock: Lock,
    added: usize,
    max_reading_chars: usize,
}

impl LiteDictionary {
    /// Exclusive journal ownership; a stale .lock after a crash requires operator
    /// removal after confirming no process is using this journal.
    pub fn open(base: impl AsRef<Path>, journal: impl AsRef<Path>) -> io::Result<Self> {
        let base = FileDictionary::open(base)?;
        let journal_path = journal.as_ref();
        let mut lock_path = journal_path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let lock_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)?;
        let lock = Lock {
            path: lock_path,
            file: Some(lock_file),
        };
        let mut journal = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(journal_path)?;
        if journal.metadata()?.len() == 0 {
            journal.write_all(MAGIC)?;
            journal.sync_data()?;
        }
        let mut magic = [0; 8];
        journal.seek(SeekFrom::Start(0))?;
        journal.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("invalid Lite journal header"));
        }
        let mut result = Self {
            max_reading_chars: base.max_reading_chars(),
            base,
            words: BTreeMap::new(),
            journal,
            lock,
            added: 0,
        };
        result.replay()?;
        Ok(result)
    }

    pub fn base(&self) -> &FileDictionary {
        &self.base
    }
    pub fn overlay_len(&self) -> usize {
        self.words.len()
    }
    pub fn journal_bytes(&self) -> io::Result<u64> {
        Ok(self.journal.metadata()?.len())
    }
    pub fn lock_path(&self) -> &Path {
        &self.lock.path
    }

    fn replay(&mut self) -> io::Result<()> {
        loop {
            let offset = self.journal.stream_position()?;
            let remaining = self.journal.metadata()?.len() - offset;
            if remaining == 0 {
                break;
            }
            if remaining < 8 {
                self.journal.set_len(offset)?;
                self.journal.sync_data()?;
                break;
            }
            let mut header = [0; 8];
            self.journal.read_exact(&mut header)?;
            let len = u32::from_le_bytes(header[..4].try_into().expect("record length")) as usize;
            let crc = u32::from_le_bytes(header[4..].try_into().expect("record CRC"));
            if len == 0 || len > MAX_RECORD_BYTES || len > MAX_BLOCK_BYTES {
                return Err(invalid("invalid journal record length"));
            }
            if remaining - 8 < len as u64 {
                self.journal.set_len(offset)?;
                self.journal.sync_data()?;
                break;
            }
            let mut bytes = vec![0; len];
            self.journal.read_exact(&mut bytes)?;
            if checksum(&bytes) != crc {
                return Err(invalid("journal checksum mismatch"));
            }
            let mut entries = decode_block(&bytes)?;
            if entries.len() != 1 {
                return Err(invalid("journal record must contain one word"));
            }
            let entry = entries.pop().expect("one entry");
            let is_new = self.is_new(&entry)?;
            self.apply(entry, is_new);
        }
        self.journal.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn is_new(&self, entry: &Entry) -> io::Result<bool> {
        if self.words.contains_key(&key(entry)) {
            return Ok(false);
        }
        Ok(!self.base.contains_entry_key(entry)?)
    }

    fn apply(&mut self, entry: Entry, is_new: bool) {
        self.max_reading_chars = self.max_reading_chars.max(entry.reading.chars().count());
        self.added += usize::from(is_new);
        self.words.insert(key(&entry), entry);
    }

    /// Durable append; the base NUD is never recompressed. Same-key entries
    /// explicitly override the base or previous journal value, including frequency.
    pub fn insert(&mut self, entry: Entry) -> io::Result<()> {
        validate_entry(&entry).map_err(invalid)?;
        let is_new = self.is_new(&entry)?;
        let payload = encode_block(std::slice::from_ref(&entry))?;
        let offset = self.journal.seek(SeekFrom::End(0))?;
        let operation = (|| {
            self.journal
                .write_all(&(payload.len() as u32).to_le_bytes())?;
            self.journal.write_all(&checksum(&payload).to_le_bytes())?;
            self.journal.write_all(&payload)?;
            self.journal.sync_data()
        })();
        if let Err(error) = operation {
            // Preserve the original error unless rollback itself fails.
            self.journal.set_len(offset)?;
            self.journal.sync_data()?;
            return Err(error);
        }
        self.apply(entry, is_new);
        Ok(())
    }
}

fn key(entry: &Entry) -> Key {
    (
        entry.reading.clone(),
        entry.surface.clone(),
        entry.part_of_speech.clone(),
    )
}

impl DictionarySource for LiteDictionary {
    fn predict(&self, prefix: &str, limit: usize) -> Result<Vec<Entry>, LookupError> {
        if prefix.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let start = (prefix.to_owned(), String::new(), String::new());
        let mut entries = self
            .base
            .predict(prefix, limit.saturating_add(self.words.len().min(2048)))?;
        entries.retain(|e| !self.words.contains_key(&key(e)));
        for (_, entry) in self
            .words
            .range(start..)
            .take_while(|(key, _)| key.0.starts_with(prefix))
            .take(2048)
        {
            entries.push(entry.clone());
            nextime_dict::sort_predictions(&mut entries, limit);
        }
        nextime_dict::sort_predictions(&mut entries, limit);
        Ok(entries)
    }
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError> {
        if limit == 0 {
            return Ok(Cow::Owned(Vec::new()));
        }
        let start = (reading.to_owned(), String::new(), String::new());
        let overlay: Vec<_> = self
            .words
            .range(start..)
            .take_while(|(key, _)| key.0 == reading)
            .map(|(_, entry)| entry)
            .collect();
        // Ask for extra base matches so overridden entries cannot hide lower ranks.
        let mut entries = self
            .base
            .lookup(reading, limit.saturating_add(overlay.len()))?
            .into_owned();
        entries.retain(|e| !self.words.contains_key(&key(e)));
        entries.extend(overlay.into_iter().cloned());
        sort_matches(&mut entries, limit);
        Ok(Cow::Owned(entries))
    }
    fn len(&self) -> usize {
        self.base.len() + self.added
    }
    fn max_reading_chars(&self) -> usize {
        self.max_reading_chars
    }
}

#[derive(Debug)]
struct Lock {
    path: PathBuf,
    file: Option<File>,
}
impl Drop for Lock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
