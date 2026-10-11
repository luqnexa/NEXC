// SPDX-License-Identifier: AGPL-3.0-only
use nexc_lite::{FileDictionary, LiteDictionary};
use nextime_dict::{Dictionary, DictionarySource, Entry};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nextime-nexc-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn sample(&self) {
        nexc_max::build_dictionary(self.path("base.nud"), &Dictionary::bundled(), 20261011)
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Remove only files in the directory this test exclusively created.
        for item in fs::read_dir(&self.0).unwrap() {
            let item = item.unwrap();
            assert!(item.file_type().unwrap().is_file());
            fs::remove_file(item.path()).unwrap();
        }
        fs::remove_dir(&self.0).unwrap();
    }
}

fn word(reading: &str, surface: &str, frequency: u32) -> Entry {
    Entry {
        reading: reading.into(),
        surface: surface.into(),
        part_of_speech: "noun".into(),
        frequency,
    }
}

#[test]
fn deterministic_lossless_build_and_core_equivalence() {
    let f = Fixture::new();
    f.sample();
    let dictionary = Dictionary::bundled();
    nexc_max::build_dictionary(f.path("second.nud"), &dictionary, 20261011).unwrap();
    assert_eq!(
        fs::read(f.path("base.nud")).unwrap(),
        fs::read(f.path("second.nud")).unwrap()
    );
    let file = FileDictionary::open(f.path("base.nud")).unwrap();
    assert_eq!(file.generation(), 20261011);
    assert_eq!(file.len(), dictionary.len());
    let mut restored = Vec::new();
    file.for_each_entry(|e| {
        restored.push(e);
        Ok(())
    })
    .unwrap();
    assert_eq!(restored, dictionary.entries());
    for input in [
        "はし",
        "にほんごにゅうりょく",
        "せんせいにれんらく",
        "🙂unknown",
        "",
        "カミ",
    ] {
        assert_eq!(
            file.lookup(input, 8).unwrap().as_ref(),
            DictionarySource::lookup(&dictionary, input, 8)
                .unwrap()
                .as_ref()
        );
        assert_eq!(
            file.predict(input, 8).unwrap(),
            dictionary.predict(input, 8).unwrap()
        );
    }
}

#[test]
fn homophones_spanning_blocks_keep_best_frequencies() {
    let f = Fixture::new();
    let entries: Vec<_> = (0..300)
        .map(|i| word("よみ", &format!("表記{i:03}"), i))
        .collect();
    nexc_max::build(f.path("base.nud"), entries.clone(), 1).unwrap();
    let file = FileDictionary::open(f.path("base.nud")).unwrap();
    assert_eq!(file.block_count(), 3);
    let matches = file.lookup("よみ", 8).unwrap();
    assert_eq!(matches.len(), 8);
    assert_eq!(matches[0].frequency, 299);
    assert!(file.lookup("よ", 8).unwrap().is_empty());
    assert!(file.lookup("よみ", 0).unwrap().is_empty());
    assert_eq!(file.lookup("よみ", usize::MAX).unwrap().len(), 300);
}

#[test]
fn empty_and_multilingual_dictionaries() {
    let f = Fixture::new();
    nexc_max::build(f.path("empty.nud"), Vec::new(), 0).unwrap();
    let empty = FileDictionary::open(f.path("empty.nud")).unwrap();
    assert!(empty.is_empty());
    empty.validate_all().unwrap();
    assert!(empty.lookup("a", 8).unwrap().is_empty());
    let entries = vec![
        word("a", "ASCII", 1),
        word("中文", "中文🙂", u32::MAX),
        word("한국", "한국어", 0),
        word("é", "été", 7),
    ];
    nexc_max::build(f.path("base.nud"), entries.clone(), 1).unwrap();
    let file = FileDictionary::open(f.path("base.nud")).unwrap();
    for entry in entries {
        assert_eq!(file.lookup(&entry.reading, 8).unwrap().as_ref(), &[entry]);
    }
}

#[test]
fn refuses_replacement_and_cleans_temporary_files() {
    let f = Fixture::new();
    f.sample();
    let original = fs::read(f.path("base.nud")).unwrap();
    assert!(nexc_max::build(f.path("base.nud"), vec![word("a", "b", 1)], 2).is_err());
    assert_eq!(fs::read(f.path("base.nud")).unwrap(), original);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    assert!(nexc_max::build(f.path("invalid.nud"), vec![word("", "x", 1)], 1).is_err());
    assert!(!f.path("invalid.nud").exists());
}
#[test]
fn compressed_and_personal_prefix_predictions() {
    let f = Fixture::new();
    f.sample();
    let file = FileDictionary::open_with_cache(f.path("base.nud"), 0).unwrap();
    let result = file.predict("よろし", 24).unwrap();
    assert!(result.iter().any(|e| e.surface == "よろしくお願いします"));
    assert!(file.predict("", 24).unwrap().is_empty());
    assert!(
        file.predict("hel", 24)
            .unwrap()
            .iter()
            .any(|e| e.surface == "hello")
    );
    file.clear_cache().unwrap();
    file.predict("a", 24).unwrap();
    assert!(file.cache_stats().unwrap().misses <= 8);
    let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
    lite.insert(word("ねくすと", "NextIME", 20000)).unwrap();
    assert_eq!(lite.predict("ねく", 24).unwrap()[0].surface, "NextIME");
}

#[test]
fn corruption_and_truncation_are_errors_not_empty_matches() {
    let f = Fixture::new();
    let sample = Dictionary::from_tsv(include_str!("../../../dictionaries/sample-ja.tsv")).unwrap();
    nexc_max::build(f.path("base.nud"), sample.entries().to_vec(), 1).unwrap();
    let original = fs::read(f.path("base.nud")).unwrap();
    for length in [0, 7, 63, 64, original.len() - 1] {
        fs::write(f.path("bad.nud"), &original[..length]).unwrap();
        assert!(FileDictionary::open(f.path("bad.nud")).is_err());
    }
    let mut bytes = original.clone();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(f.path("bad.nud"), bytes).unwrap();
    assert!(FileDictionary::open(f.path("bad.nud")).is_err());
    let mut bytes = original;
    bytes[64] ^= 1;
    fs::write(f.path("bad.nud"), bytes).unwrap();
    let file = FileDictionary::open(f.path("bad.nud")).unwrap();
    assert!(file.validate_all().is_err());
    assert!(file.lookup("あした", 8).is_err());
}

#[test]
fn append_reopen_override_and_base_remains_unchanged() {
    let f = Fixture::new();
    f.sample();
    let original = fs::read(f.path("base.nud")).unwrap();
    {
        let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
        lite.insert(word("ねくすと", "NextIME", 10000)).unwrap();
        lite.insert(word("はし", "橋", 0)).unwrap();
        lite.insert(word("ねくすと", "NextIME", 20000)).unwrap();
        assert_eq!(lite.len(), Dictionary::bundled().len() + 1);
        assert_eq!(lite.overlay_len(), 2);
        assert_eq!(lite.lookup("はし", 1).unwrap()[0].surface, "箸");
        assert_eq!(lite.lookup("ねくすと", 8).unwrap()[0].frequency, 20000);
        assert!(LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).is_err());
    }
    assert!(!f.path("words.nxl.lock").exists());
    let lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
    assert_eq!(lite.len(), Dictionary::bundled().len() + 1);
    assert_eq!(lite.overlay_len(), 2);
    assert_eq!(lite.lookup("ねくすと", 8).unwrap()[0].frequency, 20000);
    assert_eq!(fs::read(f.path("base.nud")).unwrap(), original);
    assert_eq!(lite.lookup("ねくすと", 8).unwrap()[0].surface, "NextIME");
}

#[test]
fn torn_final_record_is_removed_but_committed_words_survive() {
    let f = Fixture::new();
    f.sample();
    {
        let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
        lite.insert(word("しんご", "新語", 5000)).unwrap();
    }
    let committed = fs::read(f.path("words.nxl")).unwrap();
    for tail in [vec![1, 2, 3], vec![20, 0, 0, 0, 1, 2, 3, 4, 0, 0]] {
        let mut bytes = committed.clone();
        bytes.extend(tail);
        fs::write(f.path("words.nxl"), bytes).unwrap();
        let lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
        assert_eq!(lite.lookup("しんご", 8).unwrap()[0].surface, "新語");
        assert_eq!(lite.journal_bytes().unwrap(), committed.len() as u64);
        drop(lite);
        assert_eq!(fs::read(f.path("words.nxl")).unwrap(), committed);
    }
}

#[test]
fn corrupt_committed_journal_is_not_silently_discarded() {
    let f = Fixture::new();
    f.sample();
    {
        let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
        lite.insert(word("しんご", "新語", 5000)).unwrap();
    }
    let mut bytes = fs::read(f.path("words.nxl")).unwrap();
    bytes[16] ^= 1;
    fs::write(f.path("words.nxl"), &bytes).unwrap();
    assert!(LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).is_err());
    assert_eq!(fs::read(f.path("words.nxl")).unwrap(), bytes);
    assert!(!f.path("words.nxl.lock").exists());
}

#[test]
fn updated_base_deduplicates_words_without_losing_override() {
    let f = Fixture::new();
    f.sample();
    let custom = word("しんご", "新語", 5000);
    {
        let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
        lite.insert(custom.clone()).unwrap();
    }
    let mut entries = Dictionary::bundled().entries().to_vec();
    entries.push(word("しんご", "新語", 2000));
    nexc_max::build(f.path("updated.nud"), entries, 2).unwrap();
    let lite = LiteDictionary::open(f.path("updated.nud"), f.path("words.nxl")).unwrap();
    assert_eq!(lite.len(), Dictionary::bundled().len() + 1);
    assert_eq!(lite.lookup("しんご", 8).unwrap()[0], custom);
}

#[test]
fn invalid_registration_never_appends() {
    let f = Fixture::new();
    f.sample();
    let mut lite = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
    let size = lite.journal_bytes().unwrap();
    assert!(lite.insert(word("", "新語", 1)).is_err());
    assert_eq!(lite.journal_bytes().unwrap(), size);
    // Invalid complete record lengths are rejected rather than allocated.
    drop(lite);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(f.path("words.nxl"))
        .unwrap();
    file.write_all(&[255; 8]).unwrap();
    drop(file);
    assert!(LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).is_err());
}

#[test]
fn bounded_cache_hits_eviction_and_disabled_mode() {
    let f = Fixture::new();
    let entries = (0..300)
        .map(|i| word(&format!("よみ{i:03}"), &format!("表記{i:03}"), i))
        .collect();
    nexc_max::build(f.path("base.nud"), entries, 1).unwrap();
    let file = FileDictionary::open_with_cache(f.path("base.nud"), 1).unwrap();
    for reading in ["よみ000", "よみ000", "よみ299", "よみ000"] {
        file.lookup(reading, 8).unwrap();
    }
    assert_eq!(
        file.cache_stats().unwrap(),
        nexc_lite::CacheStats { hits: 1, misses: 3 }
    );
    file.clear_cache().unwrap();
    assert_eq!(
        file.cache_stats().unwrap(),
        nexc_lite::CacheStats::default()
    );
    let uncached = FileDictionary::open_with_cache(f.path("base.nud"), 0).unwrap();
    uncached.lookup("よみ000", 8).unwrap();
    uncached.lookup("よみ000", 8).unwrap();
    assert_eq!(
        uncached.cache_stats().unwrap(),
        nexc_lite::CacheStats { hits: 0, misses: 2 }
    );
    assert!(FileDictionary::open_with_cache(f.path("base.nud"), 33).is_err());
}

#[test]
fn shared_compressed_engine_is_thread_safe() {
    let f = Fixture::new();
    f.sample();
    let engine = FileDictionary::open(f.path("base.nud")).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..50 {
                    assert_eq!(engine.lookup("にほんご", 8).unwrap()[0].surface, "日本語");
                }
            });
        }
    });
}
