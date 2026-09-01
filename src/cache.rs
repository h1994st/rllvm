//! Incremental bitcode cache to avoid recompilation of unchanged files.
//!
//! The cache hashes source file contents and compiler flags to produce a cache key.
//! If a cached bitcode file exists with a matching key, the cached version is reused
//! instead of re-running the compiler.
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

/// Computes the cache key for a source file and its compilation flags.
///
/// The key is a hash of:
/// - The source file contents
/// - The sorted compile arguments
/// - Any bitcode generation flags from the config
pub fn compute_cache_key(
    src_filepath: &Path,
    compile_args: &[String],
    bitcode_generation_flags: Option<&Vec<String>>,
) -> Result<u64, Error> {
    let src_contents = fs::read(src_filepath).map_err(|err| {
        tracing::error!("Failed to read source file {:?}: {}", src_filepath, err);
        err
    })?;

    let mut hasher = DefaultHasher::new();

    // Hash the source file contents
    src_contents.hash(&mut hasher);

    // Hash the compile arguments (sorted for determinism)
    let mut sorted_args = compile_args.to_vec();
    sorted_args.sort();
    sorted_args.hash(&mut hasher);

    // Hash the bitcode generation flags if any
    if let Some(flags) = bitcode_generation_flags {
        let mut sorted_flags = flags.clone();
        sorted_flags.sort();
        sorted_flags.hash(&mut hasher);
    }

    Ok(hasher.finish())
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
    use super::*;

    #[test]
    fn test_compute_cache_key_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        fs::write(&src, "int main() { return 0; }").unwrap();

        let args = vec!["-O2".to_string(), "-Wall".to_string()];

        let key1 = compute_cache_key(&src, &args, None).unwrap();
        let key2 = compute_cache_key(&src, &args, None).unwrap();
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_changes_with_content() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let args = vec!["-O2".to_string()];

        fs::write(&src, "int main() { return 0; }").unwrap();
        let key1 = compute_cache_key(&src, &args, None).unwrap();

        fs::write(&src, "int main() { return 1; }").unwrap();
        let key2 = compute_cache_key(&src, &args, None).unwrap();

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_changes_with_flags() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        fs::write(&src, "int main() { return 0; }").unwrap();

        let key1 = compute_cache_key(&src, &["-O2".to_string()], None).unwrap();
        let key2 = compute_cache_key(&src, &["-O3".to_string()], None).unwrap();

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_compute_cache_key_arg_order_independent() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        fs::write(&src, "int main() { return 0; }").unwrap();

        let key1 =
            compute_cache_key(&src, &["-O2".to_string(), "-Wall".to_string()], None).unwrap();
        let key2 =
            compute_cache_key(&src, &["-Wall".to_string(), "-O2".to_string()], None).unwrap();

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_cache_lookup_miss() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("test.c");
        let result = cache_lookup(dir.path(), &src, 12345);
        assert!(result.is_none());
    }

    #[test]
    fn test_cache_store_and_lookup() {
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
    fn test_cached_bitcode_path_format() {
        let cache = Path::new("/tmp/cache");
        let src = Path::new("/tmp/foo.c");
        let path = cached_bitcode_path(cache, src, 0x1234567890ABCDEF);
        assert_eq!(path, PathBuf::from("/tmp/cache/foo_1234567890abcdef.bc"));
    }

    #[test]
    fn test_is_cache_enabled_default() {
        // Without env var, should follow config
        unsafe { env::remove_var(RLLVM_CACHE_ENV) };
        assert!(!is_cache_enabled(false));
        assert!(is_cache_enabled(true));
    }

    #[test]
    fn test_is_cache_enabled_env_override() {
        unsafe { env::set_var(RLLVM_CACHE_ENV, "1") };
        assert!(is_cache_enabled(false));

        unsafe { env::set_var(RLLVM_CACHE_ENV, "0") };
        assert!(!is_cache_enabled(false));

        unsafe { env::remove_var(RLLVM_CACHE_ENV) };
    }

    #[test]
    fn test_cache_dir_creation() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("new_cache_dir");
        assert!(!cache.exists());

        let result = cache_dir(Some(&cache)).unwrap();
        assert_eq!(result, cache);
        assert!(cache.exists());
    }
}
