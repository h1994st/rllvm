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
    time::{Duration, SystemTime, UNIX_EPOCH},
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

/// Git's racy-timestamp margin: a usage record is trusted only once it was
/// recorded at least this long after the mtime it recorded. Many
/// filesystems' mtimes are coarser than the gap between two commands running
/// back to back -- 1 second on HFS+, and tick-granular on some ext4/xfs
/// configurations -- so a write landing within the same tick as a prior
/// recording would leave the directory's mtime unchanged even though its
/// contents did change. A record made comfortably after its own mtime cannot
/// have missed a same-tick write that way; one made right away might have,
/// and is re-walked instead of trusted. Like git's rule, this assumes the
/// clock `recorded` is read from is never ahead of the clock that stamps an
/// mtime by more than this margin -- not guaranteed on a network filesystem
/// with its own clock, but true of every local filesystem this cache targets.
const RACY_WINDOW: Duration = Duration::from_secs(2);

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Mtime {
    secs: u64,
    nanos: u32,
}

impl Mtime {
    fn as_duration(self) -> Duration {
        Duration::new(self.secs, self.nanos)
    }
}

/// One generation's memoized usage, keyed by generation name in
/// `UsageMemo`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct GenerationRecord {
    /// The generation directory's mtime when this record was made.
    modified: Mtime,
    /// Wall-clock time when this record was made (not the directory's
    /// mtime): the racy-timestamp check in [`GenerationRecord::trusted_for`]
    /// needs to know how long ago that was, not just what it was.
    recorded: Mtime,
    entries: usize,
    bytes: u64,
}

impl GenerationRecord {
    /// Whether this record can stand in for a walk of a directory whose
    /// current mtime is `modified`. Requires an exact mtime match *and*
    /// that the record was made at least [`RACY_WINDOW`] after that mtime --
    /// see its doc comment for why the match alone is not enough.
    fn trusted_for(&self, modified: Mtime) -> bool {
        self.modified == modified
            && self.recorded.as_duration() >= self.modified.as_duration() + RACY_WINDOW
    }
}

/// `query-facts/usage.json`'s contents: every generation this binary has
/// measured, by directory name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct UsageMemo {
    generations: BTreeMap<String, GenerationRecord>,
}

/// `time` reduced to [`Mtime`]: seconds and nanoseconds since the epoch.
/// `None` when `time` predates the epoch (never true in practice) -- never a
/// panic, since a timestamp that cannot be compared is simply treated as
/// unavailable.
fn mtime_of(time: SystemTime) -> Option<Mtime> {
    let elapsed = time.duration_since(UNIX_EPOCH).ok()?;
    Some(Mtime {
        secs: elapsed.as_secs(),
        nanos: elapsed.subsec_nanos(),
    })
}

/// `dir`'s own mtime, reduced to [`Mtime`]. `None` when it cannot be
/// statted.
fn directory_mtime(dir: &Path) -> Option<Mtime> {
    mtime_of(fs::metadata(dir).ok()?.modified().ok()?)
}

/// Entry count and total size of one generation directory's `.mpz` files.
/// Shared by every exact measurement (`generations()`, `exact_usage()`) and
/// the memo-aware one (`disk_bytes()`), so the walk logic can never drift
/// between them. Anything that cannot be listed or measured counts as zero.
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

/// Removes `path` if it is an orphaned `.tmp*` write: one old enough
/// (`ORPHAN_AGE`) that it cannot belong to a write still in progress.
/// Returns its size when removed. Shared by the per-generation and
/// top-level sweeps in `clear`, so the age check lives in one place. An
/// unreadable mtime is left alone rather than guessed at.
fn remove_if_orphaned_temp_file(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let age = SystemTime::now().duration_since(modified).ok()?;
    if age <= ORPHAN_AGE {
        return None;
    }
    fs::remove_file(path).ok()?;
    Some(metadata.len())
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
    /// `query-facts/usage.json`'s record, and whose record was made safely
    /// outside [`RACY_WINDOW`] of that mtime, is taken from the memo rather
    /// than walked. Sound because every entry is published by renaming a
    /// temp file into the generation directory, and `clear` removes files
    /// from it; both change the directory's mtime, and nothing else touches
    /// an entry once written -- subject to the racy-timestamp caveat on
    /// [`RACY_WINDOW`]. Any generation that is new, changed, racy, or has no
    /// record is walked with [`walk_generation`] and the memo updated; a
    /// generation the memo has that no longer exists on disk is dropped
    /// from it. Changes are saved (best-effort: a write failure is logged
    /// and otherwise ignored, since the memo is an optimization, never a
    /// source of truth). A missing `query-facts/` is 0 and writes no memo.
    pub fn disk_bytes(&self) -> u64 {
        let directories = self.generation_directories();
        let mut memo = self.read_usage_memo();
        let now = mtime_of(SystemTime::now());
        let mut seen = BTreeSet::new();
        let mut changed = false;
        let mut total = 0u64;
        for (name, path) in directories {
            let modified = directory_mtime(&path);
            let fresh = modified.and_then(|modified| {
                memo.generations
                    .get(&name)
                    .filter(|record| record.trusted_for(modified))
                    .map(|record| record.bytes)
            });
            let bytes = match fresh {
                Some(bytes) => bytes,
                None => {
                    let (entries, bytes) = walk_generation(&path);
                    match (modified, now) {
                        (Some(modified), Some(recorded)) => {
                            memo.generations.insert(
                                name.clone(),
                                GenerationRecord {
                                    modified,
                                    recorded,
                                    entries,
                                    bytes,
                                },
                            );
                            changed = true;
                        }
                        _ => {
                            // No reliable timestamp to record against: drop
                            // any stale record so the next load walks again
                            // too, rather than trusting a comparison it
                            // cannot make.
                            if memo.generations.remove(&name).is_some() {
                                changed = true;
                            }
                        }
                    }
                    bytes
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

    /// The top-level entries of `query-facts/` that are generation
    /// directories, as (name, path) pairs. The one listing shared by every
    /// caller that needs it, so it exists in a single place. Empty when
    /// `query-facts/` does not exist or cannot be listed.
    fn generation_directories(&self) -> Vec<(String, PathBuf)> {
        let Ok(directories) = fs::read_dir(&self.directory) else {
            return Vec::new();
        };
        directories
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.path(),
                )
            })
            .collect()
    }

    /// Every generation directory with its entry count and size, sorted by
    /// name. Always an exact walk: callers that report or mutate the cache
    /// need the true state, not the memo. Delegates to [`Self::exact_usage`]
    /// so the two never drift; the memo it also builds is simply unused
    /// here.
    fn generations(&self) -> Vec<GenerationUsage> {
        self.exact_usage().0
    }

    /// An exact measurement of every generation, for the callers
    /// (`generations`, `usage`, `clear`) that need the true state rather
    /// than one the memo may have served, or must rewrite the memo to match
    /// it exactly rather than merge with whatever it held before.
    ///
    /// Each generation's mtime is read *before* that generation is walked,
    /// so a write landing mid-walk shows up as a change the *next*
    /// measurement will catch, rather than being paired with a mtime that
    /// had already moved past what was counted. `recorded` is one "now"
    /// taken *before* any generation in this call is walked, matching
    /// `disk_bytes`: the racy-timestamp rule in
    /// [`GenerationRecord::trusted_for`] only holds if a record's
    /// `recorded` can never be later than the walk it describes.
    fn exact_usage(&self) -> (Vec<GenerationUsage>, UsageMemo) {
        let current = FactsCache::generation();
        let recorded = mtime_of(SystemTime::now());
        let mut generations = Vec::new();
        let mut memo = UsageMemo::default();
        for (name, path) in self.generation_directories() {
            let modified = directory_mtime(&path);
            let (entries, bytes) = walk_generation(&path);
            generations.push(GenerationUsage {
                current: name == current,
                name: name.clone(),
                entries,
                bytes,
            });
            if let (Some(modified), Some(recorded)) = (modified, recorded) {
                memo.generations.insert(
                    name,
                    GenerationRecord {
                        modified,
                        recorded,
                        entries,
                        bytes,
                    },
                );
            }
        }
        generations.sort_by(|a, b| a.name.cmp(&b.name));
        (generations, memo)
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
    /// place, same pattern as `write`. Every failure -- including an
    /// encoding failure, and `query-facts/` not existing yet, which
    /// `disk_bytes` never creates just to record a memo -- is logged and
    /// otherwise ignored; the memo is an optimization, never a source of
    /// truth.
    fn write_usage_memo(&self, memo: &UsageMemo) {
        let encoded = match serde_json::to_vec_pretty(memo) {
            Ok(encoded) => encoded,
            Err(error) => {
                tracing::debug!(%error, "facts cache usage memo not encoded");
                return;
            }
        };
        if let Err(error) = self.try_write_usage_memo(&encoded) {
            tracing::debug!(%error, "facts cache usage memo not written");
        }
    }

    fn try_write_usage_memo(&self, encoded: &[u8]) -> Result<(), Error> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        temporary.write_all(encoded)?;
        temporary
            .persist(self.usage_memo_path())
            .map_err(|error| error.error)?;
        Ok(())
    }

    pub fn usage(&self, enabled: bool) -> CacheUsage {
        let (generations, memo) = self.exact_usage();
        let total_bytes = generations.iter().map(|g| g.bytes).sum();
        self.write_usage_memo(&memo);
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
    /// abandoned write rather than one still in progress -- in every
    /// generation directory, and at the top level of `query-facts/` itself,
    /// where a usage-memo write killed mid-rename can leave one beside
    /// `usage.json` -- then each generation directory left empty; anything
    /// else there is not the cache's to delete, and `usage.json` is never an
    /// orphan no matter its age. The top-level sweep belongs to no
    /// generation, so `stale_only` (`--stale`) narrows only the
    /// per-generation deletions above and never skips it.
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
                if let Some(size) = remove_if_orphaned_temp_file(&path) {
                    cleared.orphans += 1;
                    cleared.bytes += size;
                }
            }
            // Fails, harmlessly, when something that is not an entry remains.
            let _ = fs::remove_dir(&directory);
        }
        if let Ok(files) = fs::read_dir(&self.directory) {
            for file in files.filter_map(Result::ok) {
                // Generation directories are handled above; this sweeps only
                // what sits directly in `query-facts/`.
                if file.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let path = file.path();
                if !file.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
                    continue;
                }
                if let Some(size) = remove_if_orphaned_temp_file(&path) {
                    cleared.orphans += 1;
                    cleared.bytes += size;
                }
            }
        }
        let (_, memo) = self.exact_usage();
        self.write_usage_memo(&memo);
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

    /// Plants a memo containing exactly one record for the current
    /// generation, with `modified` read fresh from the directory (so it
    /// matches) and `recorded` offset from it by `recorded_offset` -- never
    /// from real elapsed wall-clock time, so these tests cannot depend on
    /// how long they take to run.
    fn plant_memo_record(cache: &FactsCache, bytes: u64, recorded_offset: Duration) {
        let generation_dir = cache.directory().join(FactsCache::generation());
        let modified = directory_mtime(&generation_dir).expect("generation dir exists");
        let recorded = modified.as_duration() + recorded_offset;
        let memo = serde_json::json!({
            "generations": {
                FactsCache::generation(): {
                    "modified": {"secs": modified.secs, "nanos": modified.nanos},
                    "recorded": {"secs": recorded.as_secs(), "nanos": recorded.subsec_nanos()},
                    "entries": 1,
                    "bytes": bytes,
                }
            }
        });
        std::fs::write(
            cache.directory().join(USAGE_FILE),
            serde_json::to_vec(&memo).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_record_recorded_well_after_its_mtime_is_served_from_the_memo() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();

        // Comfortably outside RACY_WINDOW (2s): trusted without a walk.
        plant_memo_record(&cache, 999_999, Duration::from_secs(10));

        assert_eq!(
            cache.disk_bytes(),
            999_999,
            "a record made well after its mtime, with a matching mtime, must be trusted"
        );
    }

    #[test]
    fn a_record_recorded_within_the_racy_window_is_rewalked() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        let written = cache.write(SHA, &sample()).unwrap();

        // Inside RACY_WINDOW: a change within the same mtime tick as the
        // recording would be invisible, so this record must not be
        // trusted even though its mtime matches.
        plant_memo_record(&cache, 999_999, Duration::from_secs(1));

        assert_eq!(
            cache.disk_bytes(),
            written,
            "a racy record must be rewalked rather than trusted"
        );

        let memo: serde_json::Value =
            serde_json::from_slice(&std::fs::read(cache.directory().join(USAGE_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            memo["generations"][FactsCache::generation().as_str()]["bytes"],
            written,
            "the rewalk must replace the sentinel with the true size"
        );
    }

    #[test]
    fn a_changed_generation_is_rewalked() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        cache.write(SHA, &sample()).unwrap();

        // Push the generation directory's mtime 10s into the past -- far
        // more than any real filesystem's mtime granularity (1s on HFS+) --
        // so the write below is guaranteed to land in a different tick on
        // any filesystem, rather than relying on how fast this test happens
        // to run.
        let generation_dir = cache.directory().join(FactsCache::generation());
        std::fs::File::open(&generation_dir)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(10))
            .unwrap();
        // A record matching that (backdated) mtime and recorded roughly
        // now -- trusted because it is safely outside the racy window of
        // the backdated mtime, not because the test asserts a `recorded`
        // real code would never produce.
        plant_memo_record(&cache, 1, Duration::from_secs(10));

        let other = SHA.replace('1', "4");
        cache.write(&other, &sample()).unwrap(); // moves the mtime to now

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

    #[test]
    fn clear_removes_an_old_orphaned_temp_file_left_beside_the_usage_memo() {
        let root = tempfile::tempdir().unwrap();
        let cache = FactsCache::new(root.path(), u64::MAX);
        std::fs::create_dir_all(cache.directory()).unwrap();
        std::fs::write(cache.directory().join(USAGE_FILE), b"{}").unwrap();

        let orphan = cache.directory().join(".tmpUSAGE01");
        std::fs::write(&orphan, [0u8; 9]).unwrap();
        std::fs::File::open(&orphan)
            .unwrap()
            .set_modified(stale_mtime())
            .unwrap();
        let fresh = cache.directory().join(".tmpUSAGEFRESH");
        std::fs::write(&fresh, [0u8; 2]).unwrap();

        let cleared = cache.clear(false);

        assert_eq!(cleared.orphans, 1);
        assert_eq!(cleared.bytes, 9);
        assert!(!orphan.exists());
        assert!(
            fresh.exists(),
            "a fresh top-level temp file might still be being written"
        );
        assert!(
            cache.directory().join(USAGE_FILE).exists(),
            "usage.json is never an orphan, no matter its age"
        );
    }
}
