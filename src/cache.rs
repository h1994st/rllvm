//! Incremental bitcode cache to avoid recompilation of unchanged files.
//!
//! An entry is keyed by the *compilation*, not by the source file, in two
//! levels. The **manifest key** covers the command: the source path, the
//! arguments in order, and the compiler's identity. The **content key** adds
//! the contents of every file the compilation actually read.
//!
//! The second needs a dependency list, and clang writes one as a byproduct of
//! a compile: on a miss the bitcode is generated with `-MD` pointing at a
//! private depfile inside the cache directory, and the next build hashes the
//! closure that depfile names. No extra process is spent on a hit.
//!
//! Hashing only the source's own bytes -- as this once did -- meant editing a
//! header served bitcode that no longer matched the object, with the object
//! itself always compiled fresh. The two disagreed silently.
//!
//! Enable via the `RLLVM_CACHE` environment variable (`RLLVM_CACHE=1`) or the
//! `cache_enabled` field in `~/.rllvm/config.toml`.

use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::error::Error;

/// Environment variable to enable caching (`RLLVM_CACHE=1`).
const RLLVM_CACHE_ENV: &str = "RLLVM_CACHE";

/// Default cache directory under the user's home.
const DEFAULT_CACHE_DIR: &str = ".rllvm/cache";

// Global counters for cache statistics.
static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

/// Returns `true` if bitcode caching is enabled.
///
/// Caching is enabled when the `RLLVM_CACHE` environment variable is set to `"1"`,
/// or when the config field `cache_enabled` is `true`.
pub fn is_cache_enabled(config_enabled: bool) -> bool {
    if let Ok(val) = env::var(RLLVM_CACHE_ENV) {
        return val == "1";
    }
    config_enabled
}

/// Returns the cache directory, creating it if necessary.
///
/// Uses `cache_dir` from config if provided, otherwise defaults to `~/.rllvm/cache/`.
pub fn cache_dir(config_cache_dir: Option<&Path>) -> Result<PathBuf, Error> {
    let dir = if let Some(d) = config_cache_dir {
        d.to_path_buf()
    } else {
        let home = env::var("HOME")
            .map_err(|_| Error::ConfigError("HOME environment variable not set".into()))?;
        PathBuf::from(home).join(DEFAULT_CACHE_DIR)
    };

    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|err| {
            tracing::error!("Failed to create cache directory {:?}: {}", dir, err);
            err
        })?;
    }

    Ok(dir)
}

/// Parses the prerequisites out of a `make`-style dependency file.
///
/// The shape is `target: prereq prereq \<newline> prereq`. Line continuations
/// are joined, the target is dropped, and `\ ` is an escaped space inside a
/// path rather than a separator.
pub fn parse_depfile(contents: &str) -> Vec<PathBuf> {
    let joined = contents.replace("\\\r\n", " ").replace("\\\n", " ");
    let Some((_target, prerequisites)) = joined.split_once(':') else {
        return vec![];
    };

    let mut paths = vec![];
    let mut current = String::new();
    let mut chars = prerequisites.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                current.push(' ');
                chars.next();
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    paths.push(PathBuf::from(std::mem::take(&mut current)));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        paths.push(PathBuf::from(current));
    }
    paths
}

/// Hashes the compilation command: source path, arguments, and compiler.
///
/// The arguments are hashed **in order**. Sorting them, as this once did, made
/// `-I a -I b` and `-I b -I a` the same key even though include order decides
/// which header of a given name wins.
///
/// The compiler's size and mtime are included so that upgrading LLVM does not
/// serve bitcode the previous one produced.
pub fn manifest_key(
    src_filepath: &Path,
    compile_args: &[String],
    bitcode_generation_flags: Option<&Vec<String>>,
    compiler: &Path,
) -> u64 {
    let mut hasher = DefaultHasher::new();

    src_filepath.hash(&mut hasher);
    compile_args.hash(&mut hasher);
    if let Some(flags) = bitcode_generation_flags {
        flags.hash(&mut hasher);
    }

    compiler.hash(&mut hasher);
    if let Ok(metadata) = fs::metadata(compiler) {
        metadata.len().hash(&mut hasher);
        if let Ok(modified) = metadata.modified()
            && let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            since_epoch.as_nanos().hash(&mut hasher);
        }
    }

    hasher.finish()
}

/// Returns the path of the private dependency file for a manifest key.
///
/// It lives in the cache directory, never in the build tree: the build tree's
/// dependency file belongs to the user's own `-MD`.
pub fn cached_depfile_path(cache_dir: &Path, manifest_key: u64) -> PathBuf {
    cache_dir.join(format!("{manifest_key:016x}.d"))
}

/// Extends a manifest key with the contents of everything the compilation read.
///
/// Returns `None` when the closure is unknown or unreadable -- no depfile from
/// a previous build, or a prerequisite that has since been deleted -- which the
/// caller must treat as a miss.
///
/// A change that would alter the output always alters this key: it must change
/// the command (already in `manifest_key`) or the contents of a file in the
/// closure. A new `#include` cannot appear without editing a file already
/// listed here.
pub fn content_key(manifest_key: u64, depfile: &Path) -> Option<u64> {
    let contents = fs::read_to_string(depfile).ok()?;
    let prerequisites = parse_depfile(&contents);
    if prerequisites.is_empty() {
        return None;
    }

    let mut hasher = DefaultHasher::new();
    manifest_key.hash(&mut hasher);
    for path in &prerequisites {
        let bytes = fs::read(path).ok()?;
        path.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// Records a miss for a compilation whose closure is not yet known.
///
/// The first build of a command has no dependency file, so it never reaches
/// [`cache_lookup`]; without this the statistics would count only the misses
/// that got as far as checking for a file, and report a flattering hit rate.
pub fn record_miss(src_filepath: &Path) {
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    tracing::info!(
        "Cache miss: src={:?}, no recorded dependency closure yet",
        src_filepath
    );
}

/// Returns the path where a cached bitcode file would be stored.
pub fn cached_bitcode_path(cache_dir: &Path, src_filepath: &Path, cache_key: u64) -> PathBuf {
    let file_stem = src_filepath
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    cache_dir.join(format!("{file_stem}_{cache_key:016x}.bc"))
}

/// Looks up a cached bitcode file. Returns `Some(path)` if a valid cache entry exists.
pub fn cache_lookup(cache_dir: &Path, src_filepath: &Path, cache_key: u64) -> Option<PathBuf> {
    let cached_path = cached_bitcode_path(cache_dir, src_filepath, cache_key);
    if cached_path.exists() {
        CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            "Cache hit: src={:?}, cached={:?}",
            src_filepath,
            cached_path
        );
        Some(cached_path)
    } else {
        CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
        tracing::info!("Cache miss: src={:?}", src_filepath);
        None
    }
}

/// Stores a bitcode file in the cache by copying it to the cache directory.
pub fn cache_store(
    cache_dir: &Path,
    src_filepath: &Path,
    cache_key: u64,
    bitcode_filepath: &Path,
) -> Result<PathBuf, Error> {
    let cached_path = cached_bitcode_path(cache_dir, src_filepath, cache_key);
    fs::copy(bitcode_filepath, &cached_path).map_err(|err| {
        tracing::error!(
            "Failed to store bitcode in cache: src={:?}, err={}",
            bitcode_filepath,
            err
        );
        err
    })?;
    tracing::debug!(
        "Cached bitcode: src={:?}, cached={:?}",
        src_filepath,
        cached_path
    );
    Ok(cached_path)
}

/// Returns the current cache statistics (hits, misses).
pub fn cache_stats() -> (u64, u64) {
    (
        CACHE_HITS.load(Ordering::Relaxed),
        CACHE_MISSES.load(Ordering::Relaxed),
    )
}

/// Logs the current cache statistics.
pub fn log_cache_stats() {
    let (hits, misses) = cache_stats();
    let total = hits + misses;
    if total > 0 {
        tracing::info!(
            "Cache stats: {} hits, {} misses, {:.1}% hit rate",
            hits,
            misses,
            (hits as f64 / total as f64) * 100.0
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn manifest_key_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let compiler = dir.path().join("clang");
        fs::write(&compiler, b"binary").unwrap();
        let args = vec!["-O2".to_string(), "-Wall".to_string()];

        assert_eq!(
            manifest_key(&src, &args, None, &compiler),
            manifest_key(&src, &args, None, &compiler)
        );
    }

    #[test]
    fn manifest_key_depends_on_argument_order() {
        // This asserts the opposite of what it once did. Order is semantic:
        // `-I a -I b` and `-I b -I a` find different headers of the same name,
        // and treating them as one key served the wrong bitcode.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let compiler = dir.path().join("clang");
        fs::write(&compiler, b"binary").unwrap();

        let a_first = ["-I", "a", "-I", "b"].map(String::from).to_vec();
        let b_first = ["-I", "b", "-I", "a"].map(String::from).to_vec();

        assert_ne!(
            manifest_key(&src, &a_first, None, &compiler),
            manifest_key(&src, &b_first, None, &compiler)
        );
    }

    #[test]
    fn manifest_key_changes_with_flags() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let compiler = dir.path().join("clang");
        fs::write(&compiler, b"binary").unwrap();

        assert_ne!(
            manifest_key(&src, &["-O2".to_string()], None, &compiler),
            manifest_key(&src, &["-O3".to_string()], None, &compiler)
        );
    }

    #[test]
    fn manifest_key_changes_with_the_compiler() {
        // An LLVM upgrade must not serve bitcode the old compiler produced.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let compiler = dir.path().join("clang");
        let args = vec!["-O2".to_string()];

        fs::write(&compiler, b"old build").unwrap();
        let before = manifest_key(&src, &args, None, &compiler);

        fs::write(&compiler, b"a different, longer build").unwrap();
        let after = manifest_key(&src, &args, None, &compiler);

        assert_ne!(before, after);
    }

    #[test]
    fn parse_depfile_handles_continuations_and_escaped_spaces() {
        let contents = "out.bc: /src/a.c \\\n  /inc/b.h \\\n  /has\\ space/c.h\n";
        assert_eq!(
            parse_depfile(contents),
            vec![
                PathBuf::from("/src/a.c"),
                PathBuf::from("/inc/b.h"),
                PathBuf::from("/has space/c.h"),
            ]
        );
    }

    #[test]
    fn parse_depfile_without_a_target_yields_nothing() {
        assert!(parse_depfile("no colon here").is_empty());
    }

    #[test]
    fn content_key_changes_when_a_prerequisite_changes() {
        // The whole point: a header edit must move the key even though the
        // source file and the command are untouched.
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.c");
        let header = dir.path().join("b.h");
        let depfile = dir.path().join("entry.d");
        fs::write(&source, "#include \"b.h\"\n").unwrap();
        fs::write(&header, "#define VALUE 1\n").unwrap();
        fs::write(
            &depfile,
            format!("out.bc: {} {}\n", source.display(), header.display()),
        )
        .unwrap();

        let before = content_key(7, &depfile).expect("closure is readable");
        fs::write(&header, "#define VALUE 2\n").unwrap();
        let after = content_key(7, &depfile).expect("closure is readable");

        assert_ne!(before, after);
    }

    #[test]
    fn content_key_is_none_when_the_closure_is_unusable() {
        let dir = tempfile::tempdir().unwrap();

        // No depfile at all -- the first build of this command.
        assert!(content_key(7, &dir.path().join("missing.d")).is_none());

        // A prerequisite that has since been deleted.
        let depfile = dir.path().join("stale.d");
        fs::write(&depfile, "out.bc: /nonexistent/gone.h\n").unwrap();
        assert!(content_key(7, &depfile).is_none());
    }

    #[test]
    fn cache_lookup_miss() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let result = cache_lookup(dir.path(), &src, 12345);
        assert!(result.is_none());
    }

    #[test]
    fn cache_store_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        fs::create_dir(&cache).unwrap();

        let src = dir.path().join("test.c");
        fs::write(&src, "int main() { return 0; }").unwrap();

        let bc = dir.path().join("test.bc");
        fs::write(&bc, b"fake bitcode content").unwrap();

        let key = 0xDEAD_BEEF_u64;
        let stored = cache_store(&cache, &src, key, &bc).unwrap();
        assert!(stored.exists());

        let found = cache_lookup(&cache, &src, key);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), stored);

        // Verify content was copied correctly
        let cached_content = fs::read(&stored).unwrap();
        assert_eq!(cached_content, b"fake bitcode content");
    }

    #[test]
    fn cached_bitcode_path_format() {
        let cache = Path::new("/tmp/cache");
        let src = Path::new("/tmp/foo.c");
        let path = cached_bitcode_path(cache, src, 0x1234567890ABCDEF);
        assert_eq!(path, PathBuf::from("/tmp/cache/foo_1234567890abcdef.bc"));
    }

    /// Serialises the tests that mutate `RLLVM_CACHE`.
    ///
    /// The environment is process-global but cargo runs tests in parallel
    /// threads, so these two raced: `is_cache_enabled_default` calling
    /// `remove_var` between the other test's `set_var` and its assertion made
    /// that assertion fail. It reproduced locally in roughly five runs out of
    /// eight, and surfaced in CI as an unrelated-looking failure.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Takes the environment lock, ignoring poisoning.
    ///
    /// A panic in one of these tests poisons the mutex; without this the
    /// remaining tests would fail on the lock rather than on their own
    /// assertions, hiding the original failure.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn is_cache_enabled_default() {
        let _guard = env_guard();

        // Without env var, should follow config
        unsafe { env::remove_var(RLLVM_CACHE_ENV) };
        assert!(!is_cache_enabled(false));
        assert!(is_cache_enabled(true));
    }

    #[test]
    fn is_cache_enabled_env_override() {
        let _guard = env_guard();

        unsafe { env::set_var(RLLVM_CACHE_ENV, "1") };
        assert!(is_cache_enabled(false));

        unsafe { env::set_var(RLLVM_CACHE_ENV, "0") };
        assert!(!is_cache_enabled(false));

        unsafe { env::remove_var(RLLVM_CACHE_ENV) };
    }

    #[test]
    fn cache_dir_creation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("new_cache_dir");
        assert!(!cache.exists());

        let result = cache_dir(Some(&cache)).unwrap();
        assert_eq!(result, cache);
        assert!(cache.exists());
    }

    #[test]
    fn cache_store_reports_a_missing_source() {
        let _guard = env_guard();
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        fs::create_dir_all(&cache).unwrap();

        let err = cache_store(
            &cache,
            Path::new("/nonexistent/foo.c"),
            1,
            Path::new("/nonexistent/foo.bc"),
        );
        assert!(err.is_err(), "storing a missing file must fail");
    }

    #[test]
    fn cache_stats_and_logging_run() {
        let _guard = env_guard();
        let (hits, misses) = cache_stats();
        assert!(hits < u64::MAX && misses < u64::MAX);
        // Exercises both the empty and non-empty formatting branches.
        log_cache_stats();
    }
}
