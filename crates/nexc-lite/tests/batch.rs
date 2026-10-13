// SPDX-License-Identifier: AGPL-3.0-only
use nexc_lite::{FileDictionary, LiteDictionary};
use nextime_dict::{DictionarySource, Entry};
use std::{fs, path::PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nexc-batch-test-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
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
fn batch_lookup_matches_scalar_and_decodes_each_block_once() {
    let f = Fixture::new();
    nexc_max::build(
        f.path("base.nud"),
        vec![word("a", "A", 10), word("b", "B", 20), word("c", "C", 30)],
        1,
    )
    .unwrap();
    let file = FileDictionary::open_with_cache(f.path("base.nud"), 0).unwrap();
    let queries = ["a", "b", "missing", "a", "c"];
    let scalar: Vec<_> = queries
        .iter()
        .map(|q| file.lookup(q, 32).unwrap().into_owned())
        .collect();
    file.clear_cache().unwrap();
    assert_eq!(file.lookup_batch(&queries, 32).unwrap(), scalar);
    assert_eq!(file.cache_stats().unwrap().misses, 1);
    assert!(file.lookup_batch(&["a"; 129], 1).is_err());
    assert!(
        file.lookup_batch(&queries, 0)
            .unwrap()
            .iter()
            .all(Vec::is_empty)
    );
    let mut overlay = LiteDictionary::open(f.path("base.nud"), f.path("words.nxl")).unwrap();
    overlay.insert(word("a", "A", 1)).unwrap();
    overlay.insert(word("b", "Other", 100)).unwrap();
    let scalar: Vec<_> = queries
        .iter()
        .map(|q| overlay.lookup(q, 2).unwrap().into_owned())
        .collect();
    assert_eq!(overlay.lookup_batch(&queries, 2).unwrap(), scalar);
}
