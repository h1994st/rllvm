//! Per-module extracted facts persisted between loads.
//!
//! Entries hold neutral facts (see `extract::extract_neutral`): no catalog id,
//! no source status. Both are applied by `ModuleFacts::bind_to_catalog` on
//! every load, so a hit and a fresh extraction produce the same facts.
//!
//! Layout: `<cache root>/query-facts/<generation>/<sha256>.mpz`, where the
//! generation is `f<FACTS_FORMAT>-llvm<version>` and an entry is a zstd frame
//! of named-field MessagePack. The cache never fails a query: every problem
//! reading it is a miss. `<cache root>/query-facts/usage.json` memoizes each
//! generation's entry count and byte total against the generation
//! directory's own mtime, so `disk_bytes` need not walk every entry on every
//! load; see its doc comment for the soundness argument.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use rllvm_core::{config::RLLVMConfig, error::Error};

use crate::extract::{ModuleFacts, llvm_version};

/// Version of what extraction produces. Bump it whenever a change to
/// `extract.rs` or the fact types changes the neutral facts for some input;
/// `the_facts_format_names_what_extraction_produces` fails until you do.
pub const FACTS_FORMAT: u32 = 1;

/// Subdirectory of the cache root that holds every generation.
const DIRECTORY: &str = "query-facts";

/// Entry file extension: MessagePack in a zstd frame.
const ENTRY_EXTENSION: &str = "mpz";

/// zstd level: measured on quiche at ~29x smaller than JSON, +11 ms per load.
const ZSTD_LEVEL: i32 = 3;

/// How old an abandoned `.tmp*` write must be before `clear` removes it: long
/// enough that it cannot belong to a write still in progress.
const ORPHAN_AGE: Duration = Duration::from_secs(60 * 60);

/// Prefix `tempfile::NamedTempFile::new_in` gives every temporary file it
/// creates, so `clear` can recognise one left behind by a process killed
/// mid-write.
const TEMP_PREFIX: &str = ".tmp";

/// Name of the usage memo inside `query-facts/`. A file, not a directory, so
/// the `is_dir` filter in `generations()` already ignores it.
const USAGE_FILE: &str = "usage.json";

/// What an entry claims to be. Checked on read, so a renamed or misplaced
/// file cannot be served under another key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct EntryKey {
    module_sha256: String,
    facts_format: u32,
    llvm_version: String,
}

#[derive(Serialize)]
struct EntryRef<'a> {
    key: EntryKey,
    facts: &'a ModuleFacts,
}

#[derive(Deserialize)]
struct Entry {
    key: EntryKey,
    facts: ModuleFacts,
}

/// One generation directory's usage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GenerationUsage {
    pub name: String,
    pub entries: usize,
    pub bytes: u64,
    pub current: bool,
}

/// A `SystemTime` reduced to what the usage memo needs to compare: seconds
/// and nanoseconds since the epoch. Plain fields, not a duration or
/// `SystemTime` itself, so the memo's JSON stays stable and comparable
/// without a custom (de)serializer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Mtime {
    secs: u64,
    nanos: u32,
}

/// One generation's memoized usage, keyed by generation name in
/// `UsageMemo`. Valid only as long as `modified` still matches the
/// directory's actual mtime.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationRecord {
    modified: Mtime,
    entries: usize,
    bytes: u64,
}

/// `query-facts/usage.json`'s contents: every generation this binary has
/// measured, by directory name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UsageMemo {
    generations: BTreeMap<String, GenerationRecord>,
}

/// `dir`'s own mtime, reduced to [`Mtime`]. `None` when it cannot be
/// statted or predates the epoch (never true in practice) -- never a panic,
/// since a record that cannot be compared is simply treated as stale.
fn directory_mtime(dir: &Path) -> Option<Mtime> {
    let modified = fs::metadata(dir).ok()?.modified().ok()?;
    let elapsed = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(Mtime {
        secs: elapsed.as_secs(),
        nanos: elapsed.subsec_nanos(),
    })
}

/// Entry count and total size of one generation directory's `.mpz` files.
/// Shared by the exact walk (`generations()`) and the memo-aware one
/// (`disk_bytes()`), so the two paths can never drift. Anything that cannot
/// be listed or measured counts as zero.
fn walk_generation(directory: &Path) -> (usize, u64) {
    let Ok(files) = fs::read_dir(directory) else {
        return (0, 0);
    };
    files
        .filter_map(Result::ok)
        .filter(|file| {
            file.path()
                .extension()
                .is_some_and(|ext| ext == ENTRY_EXTENSION)
        })
        .filter_map(|file| file.metadata().ok())
        .fold((0, 0), |(count, total), metadata| {
            (count + 1, total + metadata.len())
        })
}

/// The facts cache at one location. Cheap to build; touches the disk only
/// when asked to read, write or measure.
#[derive(Clone, Debug)]
pub struct FactsCache {
    directory: PathBuf,
    warn_bytes: u64,
}

impl FactsCache {
    /// The cache under `cache_root` (its `query-facts` subdirectory).
    pub fn new(cache_root: &Path, warn_bytes: u64) -> FactsCache {
        FactsCache {
            directory: cache_root.join(DIRECTORY),
            warn_bytes,
        }
    }

    /// The configured location, whether or not the cache is enabled: the
    /// `cache` command inspects it either way. Never creates the directory --
    /// resolving where the cache lives is not a reason to make it exist.
    pub fn configured(config: &RLLVMConfig) -> Result<FactsCache, Error> {
        let root = rllvm_core::cache::cache_root(config.cache_dir().map(PathBuf::as_path))?;
        Ok(FactsCache::new(&root, config.query_cache_warn_bytes()))
    }

    /// The cache a load should use: `None` when disabled, or when the cache
    /// root cannot be resolved (which is logged, never fatal).
    pub fn from_config(config: &RLLVMConfig) -> Option<FactsCache> {
        if !config.query_cache_enabled() {
            return None;
        }
        FactsCache::configured(config)
            .inspect_err(|error| tracing::debug!(%error, "query facts cache unavailable"))
            .ok()
    }

    /// `query-facts/` itself.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// The generation this binary reads and writes.
    pub fn generation() -> String {
        format!("f{FACTS_FORMAT}-llvm{}", llvm_version())
    }

    pub fn warn_bytes(&self) -> u64 {
        self.warn_bytes
    }

    fn key(module_sha256: &str) -> EntryKey {
        EntryKey {
            module_sha256: module_sha256.to_string(),
            facts_format: FACTS_FORMAT,
            llvm_version: llvm_version(),
        }
    }

    fn entry_path(&self, module_sha256: &str) -> PathBuf {
        self.directory
            .join(FactsCache::generation())
            .join(format!("{module_sha256}.{ENTRY_EXTENSION}"))
    }

    /// The neutral facts stored for `module_sha256`, or `None` on any miss:
    /// absent, unreadable, corrupt, or recorded under another key.
    pub fn read(&self, module_sha256: &str) -> Option<ModuleFacts> {
        let compressed = fs::read(self.entry_path(module_sha256)).ok()?;
        let packed = zstd::decode_all(compressed.as_slice()).ok()?;
        let entry: Entry = rmp_serde::from_slice(&packed).ok()?;
        (entry.key == FactsCache::key(module_sha256)).then_some(entry.facts)
    }

    /// Stores neutral `facts` for `module_sha256`, returning the entry's size.
    /// Written beside its destination and renamed into place, so a reader
    /// never sees a partial entry; concurrent writers of one key write the
    /// same bytes, and either may win.
    pub fn write(&self, module_sha256: &str, facts: &ModuleFacts) -> Result<u64, Error> {
        let path = self.entry_path(module_sha256);
        let directory = path
            .parent()
            .ok_or_else(|| Error::InvalidArguments("cache entry has no directory".into()))?;
        fs::create_dir_all(directory)?;
        let packed = rmp_serde::to_vec_named(&EntryRef {
            key: FactsCache::key(module_sha256),
            facts,
        })
        .map_err(|error| Error::InvalidArguments(format!("cannot encode cache entry: {error}")))?;
        let mut encoder = zstd::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
        encoder.include_checksum(true)?;
        encoder.write_all(&packed)?;
        let compressed = encoder.finish()?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        temporary.write_all(&compressed)?;
        temporary.persist(&path).map_err(|error| error.error)?;
        Ok(compressed.len() as u64)
    }

    /// Total size of every entry in every generation.
    ///
    /// Memo-aware: a generation whose directory mtime still matches
    /// `query-facts/usage.json`'s record is taken from the memo rather than
    /// walked. Sound because every entry is published by renaming a temp
    /// file into the generation directory, and `clear` removes files from
    /// it; both change the directory's mtime, and nothing else touches an
    /// entry once written. Any generation that is new, changed, or has no
    /// record is walked with [`walk_generation`] and the memo updated; a
    /// generation the memo has that no longer exists on disk is dropped
    /// from it. Changes are saved (best-effort: a write failure is logged
    /// and otherwise ignored, since the memo is an optimization, never a
    /// source of truth). A missing `query-facts/` is 0 and writes no memo.
    pub fn disk_bytes(&self) -> u64 {
        let Ok(directories) = fs::read_dir(&self.directory) else {
            return 0;
        };
        let mut memo = self.read_usage_memo();
        let mut seen = BTreeSet::new();
        let mut changed = false;
        let mut total = 0u64;
        for entry in directories.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let modified = directory_mtime(&path);
            let fresh = modified.and_then(|modified| {
                memo.generations
                    .get(&name)
                    .filter(|record| record.modified == modified)
                    .map(|record| record.bytes)
            });
            let bytes = match fresh {
                Some(bytes) => bytes,
                None => {
                    let walked = walk_generation(&path);
                    match modified {
                        Some(modified) => {
                            memo.generations.insert(
                                name.clone(),
                                GenerationRecord {
                                    modified,
                                    entries: walked.0,
                                    bytes: walked.1,
                                },
                            );
                            changed = true;
                        }
                        None => {
                            // No reliable mtime to record against: drop any
                            // stale record so the next load walks again too,
                            // rather than trusting a comparison it cannot
                            // make.
                            if memo.generations.remove(&name).is_some() {
                                changed = true;
                            }
                        }
                    }
                    walked.1
                }
            };
            total += bytes;
            seen.insert(name);
        }
        let before = memo.generations.len();
        memo.generations.retain(|name, _| seen.contains(name));
        changed |= memo.generations.len() != before;
        if changed {
            self.write_usage_memo(&memo);
        }
        total
    }

    /// Every generation directory with its entry count and size, sorted by
    /// name. Always an exact walk: callers that report or mutate the cache
    /// need the true state, not the memo.
    fn generations(&self) -> Vec<GenerationUsage> {
        let current = FactsCache::generation();
        let Ok(directories) = fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        let mut generations: Vec<GenerationUsage> = directories
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|directory| {
                let name = directory.file_name().to_string_lossy().into_owned();
                let (entries, bytes) = walk_generation(&directory.path());
                GenerationUsage {
                    current: name == current,
                    name,
                    entries,
                    bytes,
                }
            })
            .collect();
        generations.sort_by(|a, b| a.name.cmp(&b.name));
        generations
    }

    /// Folds this load's writes into the current generation's memo record,
    /// so the next `disk_bytes()` need not walk it. Called only when the
    /// load wrote at least one entry (`entries == 0` is a no-op).
    ///
    /// Accepted gap: a concurrent writer's entry landing between this
    /// load's last `write` and this call's read of the generation
    /// directory's mtime is not counted until that generation changes again
    /// or `rllvm-query cache` re-walks it exactly. The number only feeds a
    /// warning threshold.
    pub fn note_writes(&self, entries: usize, bytes: u64) {
        if entries == 0 {
            return;
        }
        let generation = FactsCache::generation();
        let Some(modified) = directory_mtime(&self.directory.join(&generation)) else {
            return;
        };
        let mut memo = self.read_usage_memo();
        let mut record = memo.generations.remove(&generation).unwrap_or_default();
        record.entries += entries;
        record.bytes += bytes;
        record.modified = modified;
        memo.generations.insert(generation, record);
        self.write_usage_memo(&memo);
    }

    /// A fresh memo built from an exact walk's results, for callers
    /// (`usage`, `clear`) that already have one and want the memo to match
    /// it precisely rather than merge with whatever it held before.
    fn memo_from_generations(&self, generations: &[GenerationUsage]) -> UsageMemo {
        let mut memo = UsageMemo::default();
        for generation in generations {
            if let Some(modified) = directory_mtime(&self.directory.join(&generation.name)) {
                memo.generations.insert(
                    generation.name.clone(),
                    GenerationRecord {
                        modified,
                        entries: generation.entries,
                        bytes: generation.bytes,
                    },
                );
            }
        }
        memo
    }

    fn usage_memo_path(&self) -> PathBuf {
        self.directory.join(USAGE_FILE)
    }

    /// The memo, or empty when missing, unreadable, or undecodable -- never
    /// an error.
    fn read_usage_memo(&self) -> UsageMemo {
        fs::read(self.usage_memo_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Atomically overwrites the memo: written beside it and renamed into
    /// place, same pattern as `write`. Failure -- including `query-facts/`
    /// not existing yet, which `disk_bytes` and `note_writes` never create
    /// just to record a memo -- is logged and otherwise ignored.
    fn write_usage_memo(&self, memo: &UsageMemo) {
        if let Err(error) = self.try_write_usage_memo(memo) {
            tracing::debug!(%error, "facts cache usage memo not written");
        }
    }

    fn try_write_usage_memo(&self, memo: &UsageMemo) -> Result<(), Error> {
        let encoded = serde_json::to_vec_pretty(memo).map_err(|error| {
            Error::InvalidArguments(format!("cannot encode cache usage memo: {error}"))
        })?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(&encoded)?;
        temporary
            .persist(self.usage_memo_path())
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub fn usage(&self, enabled: bool) -> CacheUsage {
        let generations = self.generations();
        let total_bytes = generations.iter().map(|g| g.bytes).sum();
        self.write_usage_memo(&self.memo_from_generations(&generations));
        CacheUsage {
            directory: self.directory.clone(),
            enabled,
            current: FactsCache::generation(),
            generations,
            total_bytes,
            warn_bytes: self.warn_bytes,
            over_threshold: total_bytes > self.warn_bytes,
        }
    }

    /// Removes entries: every generation's, or only those this binary no
    /// longer reads. Deletes `.mpz` files inside `query-facts/` generation
    /// directories, plus any `.tmp*` file old enough (`ORPHAN_AGE`) to be an
    /// abandoned write rather than one still in progress, then each
    /// directory left empty; anything else there is not the cache's to
    /// delete.
    pub fn clear(&self, stale_only: bool) -> Cleared {
        let mut cleared = Cleared::default();
        for generation in self.generations() {
            if stale_only && generation.current {
                continue;
            }
            let directory = self.directory.join(&generation.name);
            let Ok(files) = fs::read_dir(&directory) else {
                continue;
            };
            for file in files.filter_map(Result::ok) {
                let path = file.path();
                if path.extension().is_some_and(|ext| ext == ENTRY_EXTENSION) {
                    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
                    if fs::remove_file(&path).is_ok() {
                        cleared.entries += 1;
                        cleared.bytes += size;
                    }
                    continue;
                }
                if !file.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                    continue;
                }
                // An orphaned write: a process killed between creating its
                // temp file and renaming it into place. Only removed once
                // old enough that it cannot belong to a write still in
                // progress; an unreadable mtime is left alone rather than
                // guessed at.
                let Ok(metadata) = file.metadata() else {
                    continue;
                };
                let Ok(modified) = metadata.modified() else {
                    continue;
                };
                let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
                    continue;
                };
                if age <= ORPHAN_AGE {
                    continue;
                }
                if fs::remove_file(&path).is_ok() {
                    cleared.orphans += 1;
                    cleared.bytes += metadata.len();
                }
            }
            // Fails, harmlessly, when something that is not an entry remains.
            let _ = fs::remove_dir(&directory);
        }
        self.write_usage_memo(&self.memo_from_generations(&self.generations()));
        cleared
    }
}

/// What `rllvm-query cache` reports.
#[derive(Clone, Debug, Serialize)]
pub struct CacheUsage {
    pub directory: PathBuf,
    /// Whether loads use the cache (`query_cache`, `RLLVM_QUERY_CACHE`).
    pub enabled: bool,
    /// The generation this binary reads and writes.
    pub current: String,
    pub generations: Vec<GenerationUsage>,
    pub total_bytes: u64,
    pub warn_bytes: u64,
    pub over_threshold: bool,
}

/// What a `clear` removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cleared {
    pub entries: usize,
    /// Orphaned `.tmp*` writes removed alongside the entries; their sizes
    /// are folded into `bytes` too.
    pub orphans: usize,
    pub bytes: u64,
}

/// The text form of `rllvm-query cache`.
pub fn render_usage(usage: &CacheUsage) -> String {
    let mut out = format!("directory:   {}\n", usage.directory.display());
    if !usage.enabled {
        out.push_str("enabled:     no (query_cache = false or RLLVM_QUERY_CACHE=0)\n");
    }
    let current = usage.generations.iter().find(|g| g.current);
    let (entries, bytes) = current.map_or((0, 0), |g| (g.entries, g.bytes));
    out.push_str(&format!(
        "current:     {:<16} {entries:>5} entries   {}\n",
        usage.current,
        crate::render::human_bytes(bytes)
    ));
    for stale in usage.generations.iter().filter(|g| !g.current) {
        out.push_str(&format!(
            "stale:       {:<16} {:>5} entries   {}\n",
            stale.name,
            stale.entries,
            crate::render::human_bytes(stale.bytes)
        ));
    }
    out.push_str(&format!(
        "total:       {} of {} MB warning threshold\n",
        crate::render::human_bytes(usage.total_bytes),
        usage.warn_bytes / crate::render::MIB
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FunctionFact, FunctionId, Linkage};

    fn sample() -> ModuleFacts {
        ModuleFacts {
            functions: vec![FunctionFact {
                id: FunctionId {
                    module_id: String::new(),
                    symbol: "f".into(),
                },
                is_definition: true,
                linkage: Linkage::External,
                language: None,
                signature: "i32 ()".into(),
                location: None,
                mapped_lines: Default::default(),
            }],
            producers: vec!["clang".into()],
            ..Default::default()
        }
    }

    const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000001";

    #[test]
    fn a_written_entry_reads_back_under_its_key_only() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        assert!(cache.read(SHA).is_none(), "an empty cache has nothing");
        let size = cache.write(SHA, &sample()).unwrap();
        assert!(size > 0);
        let read = cache.read(SHA).expect("hit");
        assert_eq!(read.functions[0].id.symbol, "f");
        assert_eq!(read.producers, ["clang"]);
        assert!(cache.read(&SHA.replace('1', "2")).is_none(), "other key");
    }

    #[test]
    fn an_entry_stored_under_another_key_is_a_miss() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();
        let other = SHA.replace('1', "3");
        std::fs::rename(cache.entry_path(SHA), cache.entry_path(&other)).unwrap();
        assert!(
            cache.read(&other).is_none(),
            "a renamed entry must not be served"
        );
    }

    #[test]
    fn a_corrupt_entry_is_a_miss_and_a_rewrite_replaces_it() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();
        std::fs::write(cache.entry_path(SHA), b"not zstd").unwrap();
        assert!(cache.read(SHA).is_none());
        cache.write(SHA, &sample()).unwrap();
        assert!(cache.read(SHA).is_some());
    }

    #[test]
    fn a_cache_root_that_is_a_file_fails_writes_and_reads_nothing() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"x").unwrap();
        let cache = FactsCache::new(&file, u64::MAX);
        assert!(cache.write(SHA, &sample()).is_err());
        assert!(cache.read(SHA).is_none());
        assert_eq!(cache.disk_bytes(), 0);
    }

    #[test]
    fn disk_bytes_counts_entries_in_every_generation_and_nothing_else() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let written = cache.write(SHA, &sample()).unwrap();
        let stale = cache.directory().join("f0-llvm1.0.0");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join(format!("{SHA}.mpz")), [0u8; 10]).unwrap();
        std::fs::write(stale.join("README"), [0u8; 99]).unwrap();
        assert_eq!(cache.disk_bytes(), written + 10);
    }

    fn with_stale_generation(root: &Path) -> FactsCache {
        let cache = FactsCache::new(root, 1);
        cache.write(SHA, &sample()).unwrap();
        let stale = cache.directory().join("f0-llvm1.0.0");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join(format!("{SHA}.mpz")), [0u8; 10]).unwrap();
        std::fs::write(stale.join("keep.txt"), b"not ours").unwrap();
        cache
    }

    #[test]
    fn usage_separates_the_current_generation_from_stale_ones() {
        let root = tempfile::tempdir().unwrap();
        let cache = with_stale_generation(root.path());
        let usage = cache.usage(true);
        assert_eq!(usage.current, FactsCache::generation());
        assert_eq!(usage.generations.len(), 2);
        assert_eq!(usage.generations.iter().filter(|g| g.current).count(), 1);
        assert_eq!(usage.total_bytes, cache.disk_bytes());
        assert!(usage.over_threshold, "warn_bytes is 1");
    }

    #[test]
    fn the_warning_threshold_always_prints_in_whole_megabytes() {
        let usage = CacheUsage {
            directory: PathBuf::from("/cache"),
            enabled: true,
            current: FactsCache::generation(),
            generations: Vec::new(),
            total_bytes: 0,
            // A GiB: human_bytes alone would print this as "1.0 GB", but the
            // threshold is always whole MB, matching query_cache_warn_mb.
            warn_bytes: 1024 * 1024 * 1024,
            over_threshold: false,
        };
        let text = render_usage(&usage);
        assert!(text.contains("of 1024 MB warning threshold"), "got: {text}");
    }

    #[test]
    fn clearing_stale_generations_keeps_the_current_one_and_foreign_files() {
        let root = tempfile::tempdir().unwrap();
        let cache = with_stale_generation(root.path());
        let cleared = cache.clear(true);
        assert_eq!((cleared.entries, cleared.bytes), (1, 10));
        assert!(cache.read(SHA).is_some(), "the current entry survives");
        assert!(cache.directory().join("f0-llvm1.0.0/keep.txt").exists());
    }

    #[test]
    fn clearing_everything_removes_every_entry_and_empty_generation() {
        let root = tempfile::tempdir().unwrap();
        let cache = with_stale_generation(root.path());
        let before = cache.disk_bytes();
        let cleared = cache.clear(false);
        assert_eq!(cleared.entries, 2);
        assert_eq!(cleared.bytes, before);
        assert_eq!(cache.disk_bytes(), 0);
        assert!(!cache.directory().join(FactsCache::generation()).exists());
        assert!(
            cache.directory().join("f0-llvm1.0.0/keep.txt").exists(),
            "never ours to delete"
        );
    }

    /// An hour old plus a margin: comfortably past `ORPHAN_AGE`.
    fn stale_mtime() -> std::time::SystemTime {
        std::time::SystemTime::now() - Duration::from_secs(60 * 61)
    }

    #[test]
    fn clear_removes_an_orphaned_temp_file_older_than_an_hour() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let generation = cache.directory().join(FactsCache::generation());
        std::fs::create_dir_all(&generation).unwrap();
        let orphan = generation.join(".tmpABCDEF");
        std::fs::write(&orphan, [0u8; 7]).unwrap();
        std::fs::File::open(&orphan)
            .unwrap()
            .set_modified(stale_mtime())
            .unwrap();

        let cleared = cache.clear(false);

        assert_eq!(cleared.orphans, 1);
        assert_eq!(cleared.bytes, 7);
        assert!(!orphan.exists());
    }

    #[test]
    fn clear_leaves_a_fresh_temp_file_alone() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let generation = cache.directory().join(FactsCache::generation());
        std::fs::create_dir_all(&generation).unwrap();
        let fresh = generation.join(".tmpFRESH01");
        std::fs::write(&fresh, [0u8; 3]).unwrap();

        let cleared = cache.clear(false);

        assert_eq!(cleared.orphans, 0);
        assert!(
            fresh.exists(),
            "a temp file that might still be written must survive"
        );
    }

    #[test]
    fn clearing_a_generation_with_only_an_old_orphan_removes_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let generation = cache.directory().join(FactsCache::generation());
        std::fs::create_dir_all(&generation).unwrap();
        let orphan = generation.join(".tmpONLYONE");
        std::fs::write(&orphan, [0u8; 5]).unwrap();
        std::fs::File::open(&orphan)
            .unwrap()
            .set_modified(stale_mtime())
            .unwrap();

        cache.clear(false);

        assert!(!generation.exists());
    }

    #[test]
    fn an_unchanged_generation_is_served_from_the_memo() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();
        let exact = cache.disk_bytes();
        assert!(exact > 0);

        let memo_path = cache.directory().join(USAGE_FILE);
        let mut memo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&memo_path).unwrap()).unwrap();
        let generation = FactsCache::generation();
        memo["generations"][generation.as_str()]["bytes"] = serde_json::json!(999_999);
        std::fs::write(&memo_path, serde_json::to_vec(&memo).unwrap()).unwrap();

        assert_eq!(
            cache.disk_bytes(),
            999_999,
            "an unchanged generation must be served from the memo, not walked"
        );
    }

    #[test]
    fn a_changed_generation_is_rewalked() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();
        cache.disk_bytes(); // records the memo

        let other = SHA.replace('1', "4");
        cache.write(&other, &sample()).unwrap(); // changes the generation dir's mtime

        let exact: u64 = cache.generations().iter().map(|g| g.bytes).sum();
        assert_eq!(cache.disk_bytes(), exact);
    }

    #[test]
    fn a_missing_or_corrupt_memo_gives_the_exact_walked_size_and_is_rewritten_valid() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let written = cache.write(SHA, &sample()).unwrap();

        // No memo has ever been written yet: the first call must still be
        // exact.
        assert_eq!(cache.disk_bytes(), written);

        std::fs::write(cache.directory().join(USAGE_FILE), b"not json").unwrap();
        assert_eq!(
            cache.disk_bytes(),
            written,
            "a corrupt memo is treated as empty"
        );

        let memo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .expect("the corrupt memo must be rewritten as valid JSON");
        assert_eq!(
            memo["generations"][FactsCache::generation().as_str()]["bytes"],
            written
        );
    }

    #[test]
    fn a_removed_generation_drops_out_of_the_memo_and_the_total() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();
        let stale = cache.directory().join("f0-llvm1.0.0");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join(format!("{SHA}.mpz")), [0u8; 10]).unwrap();
        let with_both = cache.disk_bytes();
        assert!(with_both > 10);

        std::fs::remove_dir_all(&stale).unwrap();
        assert_eq!(cache.disk_bytes(), with_both - 10);

        let memo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .unwrap();
        assert!(
            memo["generations"]["f0-llvm1.0.0"].is_null(),
            "a removed generation must not linger in the memo: {memo}"
        );
    }

    #[test]
    fn note_writes_updates_the_record_so_disk_bytes_needs_no_walk() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let size = cache.write(SHA, &sample()).unwrap();
        cache.note_writes(1, size);

        let generation_dir = cache.directory().join(FactsCache::generation());
        let modified = std::fs::metadata(&generation_dir)
            .unwrap()
            .modified()
            .unwrap();
        let expected = modified
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap();

        let memo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .unwrap();
        let record = &memo["generations"][FactsCache::generation().as_str()];
        assert_eq!(record["modified"]["secs"], expected.as_secs());
        assert_eq!(record["modified"]["nanos"], expected.subsec_nanos());
        assert_eq!(record["entries"], 1);
        assert_eq!(record["bytes"], size);

        let exact: u64 = cache.generations().iter().map(|g| g.bytes).sum();
        assert_eq!(cache.disk_bytes(), exact);
    }

    #[test]
    fn clear_and_usage_leave_a_memo_consistent_with_the_exact_walk() {
        let root = tempfile::tempdir().unwrap();
        let cache = with_stale_generation(root.path());
        let current = FactsCache::generation();

        cache.usage(true);
        let after_usage: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .unwrap();
        assert_eq!(after_usage["generations"][current.as_str()]["entries"], 1);
        assert_eq!(after_usage["generations"]["f0-llvm1.0.0"]["entries"], 1);

        cache.clear(false);
        let after_clear: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .unwrap();
        assert!(
            after_clear["generations"][current.as_str()].is_null(),
            "the emptied current generation no longer exists to record: {after_clear}"
        );
        assert_eq!(after_clear["generations"]["f0-llvm1.0.0"]["entries"], 0);
    }

    #[test]
    fn stale_clear_only_removes_orphans_in_stale_generations() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);

        let current = cache.directory().join(FactsCache::generation());
        std::fs::create_dir_all(&current).unwrap();
        let current_orphan = current.join(".tmpCURRENT");
        std::fs::write(&current_orphan, [0u8; 4]).unwrap();
        std::fs::File::open(&current_orphan)
            .unwrap()
            .set_modified(stale_mtime())
            .unwrap();

        let stale = cache.directory().join("f0-llvm1.0.0");
        std::fs::create_dir_all(&stale).unwrap();
        let stale_orphan = stale.join(".tmpSTALE01");
        std::fs::write(&stale_orphan, [0u8; 6]).unwrap();
        std::fs::File::open(&stale_orphan)
            .unwrap()
            .set_modified(stale_mtime())
            .unwrap();

        let cleared = cache.clear(true);

        assert_eq!(cleared.orphans, 1);
        assert_eq!(cleared.bytes, 6);
        assert!(
            current_orphan.exists(),
            "--stale must not touch the current generation"
        );
        assert!(!stale_orphan.exists());
    }
}
