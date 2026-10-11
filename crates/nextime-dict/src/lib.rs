// SPDX-License-Identifier: AGPL-3.0-only
//! In-memory dictionary, entry validation, and the shared dictionary interface.

use std::{borrow::Cow, error::Error, fmt};

/// Common interface for memory-resident and compressed disk dictionaries.
pub trait DictionarySource: fmt::Debug + Send + Sync {
    /// Optional bounded prefix completion; unavailable sources return no suggestions.
    fn predict(&self, _: &str, _: usize) -> Result<Vec<Entry>, LookupError> {
        Ok(Vec::new())
    }
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn max_reading_chars(&self) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LookupError(pub String);

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for LookupError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub reading: String,
    pub surface: String,
    pub part_of_speech: String,
    pub frequency: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct DictionaryError {
    pub line: usize,
    pub reason: &'static str,
}

impl fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dictionary line {}: {}", self.line, self.reason)
    }
}

impl Error for DictionaryError {}

#[derive(Debug)]
pub struct Dictionary {
    entries: Vec<Entry>,
    max_reading_chars: usize,
}

impl Dictionary {
    /// Four tab-separated fields: reading, surface, POS, unsigned frequency.
    /// Blank lines and lines starting with # are ignored. Duplicate keys keep
    /// the highest frequency. The complete dictionary resides in RAM.
    pub fn from_tsv(tsv: &str) -> Result<Self, DictionaryError> {
        let mut entries = Vec::new();
        for (index, line) in tsv.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let fields: Vec<_> = line.split('\t').collect();
            let fail = |reason| DictionaryError {
                line: index + 1,
                reason,
            };
            if fields.len() != 4 {
                return Err(fail("expected exactly four tab-separated fields"));
            }
            let frequency = fields[3]
                .parse()
                .map_err(|_| fail("invalid u32 frequency"))?;
            let entry = Entry {
                reading: fields[0].into(),
                surface: fields[1].into(),
                part_of_speech: fields[2].into(),
                frequency,
            };
            validate_entry(&entry).map_err(fail)?;
            entries.push(entry);
        }
        Self::from_entries(entries)
    }

    pub fn from_entries(mut entries: Vec<Entry>) -> Result<Self, DictionaryError> {
        for (index, entry) in entries.iter().enumerate() {
            validate_entry(entry).map_err(|reason| DictionaryError {
                line: index + 1,
                reason,
            })?;
        }
        entries.sort_unstable_by(|a, b| {
            (&a.reading, &a.surface, &a.part_of_speech)
                .cmp(&(&b.reading, &b.surface, &b.part_of_speech))
                .then_with(|| b.frequency.cmp(&a.frequency))
        });
        entries.dedup_by(|a, b| {
            a.reading == b.reading && a.surface == b.surface && a.part_of_speech == b.part_of_speech
        });
        entries.sort_unstable_by(|a, b| {
            a.reading
                .cmp(&b.reading)
                .then_with(|| b.frequency.cmp(&a.frequency))
                .then_with(|| a.surface.cmp(&b.surface))
                .then_with(|| a.part_of_speech.cmp(&b.part_of_speech))
        });
        let max_reading_chars = entries
            .iter()
            .map(|e| e.reading.chars().count())
            .max()
            .unwrap_or(0);
        Ok(Self {
            entries,
            max_reading_chars,
        })
    }

    /// O(log N + matches), without copying entries.
    pub fn lookup(&self, reading: &str) -> &[Entry] {
        let start = self
            .entries
            .partition_point(|e| e.reading.as_str() < reading);
        let count = self.entries[start..].partition_point(|e| e.reading == reading);
        &self.entries[start..start + count]
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn max_reading_chars(&self) -> usize {
        self.max_reading_chars
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn bundled() -> Self {
        Self::from_tsv(concat!(
            include_str!("../../../dictionaries/sample-ja.tsv"),
            "\n",
            include_str!("../../../dictionaries/extended-ja.tsv"),
            "\n",
            include_str!("../../../dictionaries/english.tsv"),
            "\n",
            include_str!("../../../dictionaries/phrases-ja.tsv"),
            "\n",
            include_str!("../../../dictionaries/proper-nouns.tsv")
        ))
        .expect("bundled dictionary must be valid")
    }
}

pub fn validate_entry(entry: &Entry) -> Result<(), &'static str> {
    if entry.reading.is_empty() || entry.surface.is_empty() || entry.part_of_speech.is_empty() {
        return Err("reading, surface and POS must be nonempty");
    }
    if entry.reading.chars().count() > 128
        || entry.surface.chars().count() > 256
        || entry.part_of_speech.len() > 64
    {
        return Err("entry exceeds Phase 1 field length limits");
    }
    if [&entry.reading, &entry.surface, &entry.part_of_speech]
        .iter()
        .any(|s| s.chars().any(char::is_control))
    {
        return Err("control characters are forbidden");
    }
    Ok(())
}

impl DictionarySource for Dictionary {
    fn predict(&self, prefix: &str, limit: usize) -> Result<Vec<Entry>, LookupError> {
        if prefix.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let start = self
            .entries
            .partition_point(|e| e.reading.as_str() < prefix);
        let mut result = Vec::new();
        // Bound work for very broad prefixes. A global frequency index is future work.
        for entry in self.entries[start..]
            .iter()
            .take_while(|e| e.reading.starts_with(prefix))
            .take(2048)
        {
            result.push(entry.clone());
            if result.len() > limit.saturating_mul(2) {
                sort_predictions(&mut result, limit);
            }
        }
        sort_predictions(&mut result, limit);
        Ok(result)
    }
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError> {
        let entries = self.lookup(reading);
        Ok(Cow::Borrowed(&entries[..entries.len().min(limit)]))
    }
    fn len(&self) -> usize {
        self.len()
    }
    fn max_reading_chars(&self) -> usize {
        self.max_reading_chars()
    }
}

impl<T: DictionarySource + ?Sized> DictionarySource for std::sync::Arc<T> {
    fn predict(&self, prefix: &str, limit: usize) -> Result<Vec<Entry>, LookupError> {
        self.as_ref().predict(prefix, limit)
    }
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError> {
        self.as_ref().lookup(reading, limit)
    }
    fn len(&self) -> usize {
        self.as_ref().len()
    }
    fn max_reading_chars(&self) -> usize {
        self.as_ref().max_reading_chars()
    }
}

pub fn sort_matches(entries: &mut Vec<Entry>, limit: usize) {
    entries.sort_unstable_by(|a, b| {
        b.frequency
            .cmp(&a.frequency)
            .then_with(|| a.surface.cmp(&b.surface))
            .then_with(|| a.part_of_speech.cmp(&b.part_of_speech))
    });
    entries.truncate(limit);
}

/// Visible prediction slots must not be consumed by alternative POS IDs.
pub fn sort_predictions(entries: &mut Vec<Entry>, limit: usize) {
    sort_matches(entries, usize::MAX);
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert(e.surface.clone()));
    entries.truncate(limit);
}

#[derive(Debug)]
pub struct WithCommonPredictions<T: DictionarySource> {
    source: T,
    common: Dictionary,
    names: Dictionary,
}
impl<T: DictionarySource> WithCommonPredictions<T> {
    pub fn new(source: T) -> Self {
        Self {
            source,
            common: Dictionary::bundled(),
            names: Dictionary::from_tsv(include_str!("../../../dictionaries/proper-nouns.tsv"))
                .expect("proper noun overlay must be valid"),
        }
    }
}
impl<T: DictionarySource> DictionarySource for WithCommonPredictions<T> {
    fn lookup(&self, reading: &str, limit: usize) -> Result<Cow<'_, [Entry]>, LookupError> {
        let source = self.source.lookup(reading, limit)?;
        let names = self.names.lookup(reading);
        if !names.is_empty() {
            let mut entries = source.into_owned();
            entries.extend_from_slice(names);
            sort_matches(&mut entries, usize::MAX);
            let mut seen = std::collections::HashSet::new();
            entries.retain(|e| seen.insert((e.surface.clone(), e.part_of_speech.clone())));
            entries.truncate(limit);
            return Ok(Cow::Owned(entries));
        }
        // New common English vocabulary must work with an older immutable NUD,
        // including mixed-script conversion, not only prediction.
        if source.is_empty() {
            let english: Vec<_> = self
                .common
                .lookup(reading)
                .iter()
                .filter(|entry| matches!(entry.part_of_speech.as_str(), "english" | "slang-en"))
                .take(limit)
                .cloned()
                .collect();
            if !english.is_empty() {
                return Ok(Cow::Owned(english));
            }
        }
        Ok(source)
    }
    fn predict(&self, prefix: &str, limit: usize) -> Result<Vec<Entry>, LookupError> {
        let mut entries = self.source.predict(prefix, limit)?;
        entries.extend(DictionarySource::predict(&self.common, prefix, limit)?);
        sort_predictions(&mut entries, limit);
        Ok(entries)
    }
    fn len(&self) -> usize {
        self.source.len()
    }
    fn max_reading_chars(&self) -> usize {
        self.source
            .max_reading_chars()
            .max(self.names.max_reading_chars())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_lookup_sorted_and_deduplicated() {
        let dict = Dictionary::from_tsv(
            "はし\t橋\tnoun\t2\nはし\t箸\tnoun\t3\nはし\t橋\tnoun\t9\nは\t葉\tnoun\t1",
        )
        .unwrap();
        assert_eq!(dict.len(), 3);
        assert_eq!(dict.lookup("はし")[0].surface, "橋");
        assert_eq!(dict.lookup("はし")[0].frequency, 9);
        assert_eq!(dict.lookup("は").len(), 1);
        assert!(dict.lookup("はしら").is_empty());
        assert!(dict.lookup("").is_empty());
    }

    #[test]
    fn rejects_malformed_data() {
        for input in [
            "a\tb",
            "a\tb\tnoun\t-1",
            "a\tb\tnoun\t4294967296",
            "\tb\tnoun\t1",
            "a\tb\tnoun\t1\textra",
            "a\tb\0\tnoun\t1",
        ] {
            assert!(Dictionary::from_tsv(input).is_err(), "{input:?}");
        }
        assert_eq!(
            Dictionary::from_tsv("# comment\nwrong").unwrap_err().line,
            2
        );
    }

    #[test]
    fn empty_and_bundled() {
        assert!(Dictionary::from_tsv("").unwrap().is_empty());
        assert!(
            Dictionary::bundled()
                .lookup("にほんご")
                .iter()
                .any(|e| e.surface == "日本語")
        );
    }
    #[test]
    fn predictions_keep_distinct_surfaces_and_long_phrases() {
        let mut entries = Vec::new();
        for word in 0..30 {
            for pos in 0..12 {
                entries.push(Entry {
                    reading: format!("よ{word:02}"),
                    surface: format!("候補{word:02}"),
                    part_of_speech: format!("pos{pos}"),
                    frequency: 10000 - word,
                });
            }
        }
        let dictionary = Dictionary::from_entries(entries).unwrap();
        let predictions = DictionarySource::predict(&dictionary, "よ", 24).unwrap();
        assert_eq!(predictions.len(), 24);
        assert_eq!(predictions[0].surface, "候補00");
        assert_eq!(predictions[23].surface, "候補23");
        let common = WithCommonPredictions::new(dictionary);
        assert!(
            common
                .predict("よろ", 24)
                .unwrap()
                .iter()
                .any(|e| e.surface == "よろしくお願いいたします")
        );
        assert!(
            DictionarySource::predict(&Dictionary::bundled(), "おせ", 24)
                .unwrap()
                .iter()
                .any(|e| e.surface == "お世話になっております")
        );
    }
}
