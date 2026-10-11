// SPDX-License-Identifier: AGPL-3.0-only
use nexc_lite::{FileDictionary, LiteDictionary};
use nextime_dict::{Dictionary, DictionarySource, Entry};
use std::{fs, hint::black_box, io, path::PathBuf, sync::Arc, time::Instant};

fn main() {
    if let Err(e) = run() {
        eprintln!("benchmark: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (mut count, mut iterations) = (100_000_usize, 2000_usize);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--entries" => count = args.next().ok_or("missing entries")?.parse()?,
            "--iterations" => iterations = args.next().ok_or("missing iterations")?.parse()?,
            "--bench" => {}
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    if count == 0 || iterations == 0 {
        return Err("positive entries and iterations required".into());
    }
    let directory = std::env::temp_dir().join(format!("nextime-nexc-bench-{}", std::process::id()));
    fs::create_dir(&directory)?;
    let cleanup = Cleanup(directory);
    let path = cleanup.0.join("base.nud");
    let journal = cleanup.0.join("words.nxl");
    println!(
        "NEXC v1; OS={}; arch={}; entries={count}; iterations={iterations}; limit=8; synthetic=true; warmup=100; cache=OS_uncontrolled",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let entries: Vec<_> = (0..count)
        .map(|i| Entry {
            reading: format!("ごい{i:09}"),
            surface: format!("語彙{i:09}"),
            part_of_speech: "noun".into(),
            frequency: 1000 + (i % 10000) as u32,
        })
        .collect();
    let dictionary = Dictionary::from_entries(entries)?;
    let tsv_bytes: usize = dictionary
        .entries()
        .iter()
        .map(|e| {
            e.reading.len()
                + e.surface.len()
                + e.part_of_speech.len()
                + e.frequency.to_string().len()
                + 4
        })
        .sum();
    let start = Instant::now();
    let stats = nexc_max::build_dictionary(&path, &dictionary, 1)?;
    println!(
        "build_ms={:.3}; nud_bytes={}; canonical_tsv_bytes={tsv_bytes}; nud_over_tsv={:.6}; blocks={}",
        start.elapsed().as_secs_f64() * 1000.0,
        stats.bytes,
        stats.bytes as f64 / tsv_bytes as f64,
        stats.blocks
    );
    let start = Instant::now();
    let file = Arc::new(FileDictionary::open(&path)?);
    println!(
        "open_ms={:.3}; index_encoded_bytes={}; index_memory_estimate_bytes={}",
        start.elapsed().as_secs_f64() * 1000.0,
        file.index_encoded_bytes(),
        file.index_memory_bytes()
    );
    // Validate every field once before benchmarking searches.
    let mut i = 0;
    file.for_each_entry(|entry| {
        if entry != dictionary.entries()[i] {
            return Err(io::Error::other("roundtrip mismatch"));
        }
        i += 1;
        Ok(())
    })?;
    assert_eq!(i, dictionary.len());
    let memory = dictionary;
    file.clear_cache()?;
    let disk = Arc::clone(&file);
    let uncached_file = Arc::new(FileDictionary::open_with_cache(&path, 0)?);
    let uncached = Arc::clone(&uncached_file);
    let queries: Vec<_> = (0..iterations.max(100))
        .map(|i| {
            // Fixed multiplicative stride spreads reads across the whole dictionary.
            format!("ごい{:09}", i.wrapping_mul(104729) % count)
        })
        .collect();
    measure("memory/lookup_spread_hit", iterations, |i| {
        black_box(DictionarySource::lookup(&memory, &queries[i], 8).unwrap());
    });
    measure("nud/cache8_lookup_repeated_hit", iterations, |_| {
        black_box(disk.lookup("ごい000000000", 8).unwrap());
    });
    println!("cache8/repeated_stats={:?}", file.cache_stats()?);
    file.clear_cache()?;
    measure("nud/cache8_lookup_spread_hit", iterations, |i| {
        black_box(disk.lookup(&queries[i], 8).unwrap());
    });
    println!("cache8/spread_stats={:?}", file.cache_stats()?);
    measure("nud/cache0_lookup_spread_hit", iterations, |i| {
        black_box(uncached.lookup(&queries[i], 8).unwrap());
    });
    println!("cache0/spread_stats={:?}", uncached_file.cache_stats()?);
    measure("nud/lookup_miss", iterations, |_| {
        black_box(disk.lookup("みとうろく🙂", 8).unwrap());
    });
    let mut lite = LiteDictionary::open(&path, &journal)?;
    let insert_count = iterations.min(100);
    let words: Vec<_> = (0..insert_count + 100)
        .map(|i| Entry {
            reading: format!("しんご{i:09}"),
            surface: format!("新語{i:09}"),
            part_of_speech: "noun".into(),
            frequency: 5000,
        })
        .collect();
    let mut inserted = 0;
    measure("lite/append_sync_data", insert_count, |_| {
        lite.insert(words[inserted].clone()).unwrap();
        inserted += 1;
    });
    println!(
        "overlay_entries={}; journal_bytes={}; base_bytes_after_append={}",
        lite.overlay_len(),
        lite.journal_bytes()?,
        fs::metadata(&path)?.len()
    );
    drop(lite);
    drop(disk);
    drop(uncached);
    drop(file);
    drop(uncached_file);
    drop(memory);
    Ok(())
}

fn measure(label: &str, iterations: usize, mut operation: impl FnMut(usize)) {
    for i in 0..100 {
        operation(i);
    }
    let mut times = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let start = Instant::now();
        operation(i);
        times.push(start.elapsed().as_nanos());
    }
    times.sort_unstable();
    let percentile = |p: usize| times[(iterations * p).div_ceil(100) - 1] as f64 / 1_000_000.0;
    println!(
        "{label}: n={iterations}; p50_ms={:.6}; p95_ms={:.6}; p99_ms={:.6}; max_ms={:.6}",
        percentile(50),
        percentile(95),
        percentile(99),
        *times.last().unwrap() as f64 / 1_000_000.0
    );
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        // This freshly created, process-specific directory contains only these
        // known files. Do not recursively remove a computed filesystem path.
        for name in ["words.nxl", "words.nxl.lock", "base.nud"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}
