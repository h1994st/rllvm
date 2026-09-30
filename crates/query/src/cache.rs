//! Per-module extracted facts persisted between loads.
//!
//! Entries hold neutral facts (see `extract::extract_neutral`): no catalog id,
//! no source status. Both are applied by `ModuleFacts::bind_to_catalog` on
//! every load, so a hit and a fresh extraction produce the same facts.
//!
//! Layout: `<cache root>/query-facts/<generation>/<sha256>.mpz`, where the
//! generation is `f<FACTS_FORMAT>-llvm<version>` and an entry is a zstd frame
//! of named-field MessagePack. The cache never fails a query: every problem
//! reading it is a miss.

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    time::Duration,
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

    /// Total size of every entry in every generation. One directory walk;
    /// anything that cannot be listed or measured counts as zero.
    pub fn disk_bytes(&self) -> u64 {
        self.generations()
            .iter()
            .map(|generation| generation.bytes)
            .sum()
    }

    /// Every generation directory with its entry count and size, sorted by
    /// name.
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
                let (entries, bytes) = fs::read_dir(directory.path())
                    .map(|files| {
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
                    })
                    .unwrap_or((0, 0));
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

    pub fn usage(&self, enabled: bool) -> CacheUsage {
        let generations = self.generations();
        let total_bytes = generations.iter().map(|g| g.bytes).sum();
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
        "total:       {} of {} warning threshold\n",
        crate::render::human_bytes(usage.total_bytes),
        crate::render::human_bytes(usage.warn_bytes).replace(".0 MB", " MB")
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
