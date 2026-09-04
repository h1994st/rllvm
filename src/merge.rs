//! Bitcode merge strategies for combining extracted bitcode files.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{error::Error, utils::*};

/// Strategy for merging multiple bitcode files into a single output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum MergeStrategy {
    /// Link all bitcode files into one module using `llvm-link`.
    Full,
    /// Group bitcode files by parent directory, `llvm-link` each group, then
    /// `llvm-link` the per-group results into a final module.
    Partial,
    /// Archive all bitcode files into a single archive using `llvm-ar`.
    Archive,
}

impl std::fmt::Display for MergeStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeStrategy::Full => write!(f, "full"),
            MergeStrategy::Partial => write!(f, "partial"),
            MergeStrategy::Archive => write!(f, "archive"),
        }
    }
}

/// Merge the given bitcode files into `output_filepath` using the chosen strategy.
pub fn merge_bitcode_files<P: AsRef<Path>>(
    strategy: MergeStrategy,
    bitcode_filepaths: &[P],
    output_filepath: P,
) -> Result<Option<i32>, Error> {
    match strategy {
        MergeStrategy::Full => {
            tracing::info!("Merge strategy: full (llvm-link all)");
            link_bitcode_files(bitcode_filepaths, output_filepath)
        }
        MergeStrategy::Partial => {
            tracing::info!("Merge strategy: partial (group by parent dir, then llvm-link)");
            partial_link_bitcode_files(bitcode_filepaths, output_filepath)
        }
        MergeStrategy::Archive => {
            tracing::info!("Merge strategy: archive (llvm-ar)");
            archive_bitcode_files(bitcode_filepaths, output_filepath)
        }
    }
}

/// Group bitcode files by parent directory, link each group separately, then
/// link the per-group outputs into the final bitcode file.
fn partial_link_bitcode_files<P: AsRef<Path>>(
    bitcode_filepaths: &[P],
    output_filepath: P,
) -> Result<Option<i32>, Error> {
    let output_filepath = output_filepath.as_ref();

    // If there is only one file or all share the same parent, fall back to full link.
    let paths: Vec<&Path> = bitcode_filepaths.iter().map(|p| p.as_ref()).collect();
    let groups = group_by_parent_dir(&paths);
    if groups.len() <= 1 {
        tracing::info!("Partial: single group detected, falling back to full link");
        return link_bitcode_files(&paths, output_filepath);
    }

    tracing::info!("Partial: {} groups detected", groups.len());

    let mut intermediate_files: Vec<PathBuf> = Vec::new();
    let output_stem = output_filepath
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let output_dir = output_filepath.parent().unwrap_or(Path::new("."));

    for (idx, (dir, files)) in groups.iter().enumerate() {
        tracing::debug!(
            "Partial group {}: dir={:?}, {} files",
            idx,
            dir,
            files.len()
        );
        let intermediate = output_dir.join(format!("{}_partial_{}.bc", output_stem, idx));

        let result = link_bitcode_files(files.as_slice(), intermediate.as_path())?;
        if result.is_some_and(|code| code != 0) {
            // Clean up any intermediates produced so far.
            cleanup_files(&intermediate_files);
            return Ok(result);
        }

        intermediate_files.push(intermediate);
    }

    // Final link of the per-group intermediates.
    let intermediates_as_paths: Vec<&Path> =
        intermediate_files.iter().map(|p| p.as_path()).collect();
    let result = link_bitcode_files(&intermediates_as_paths, output_filepath);

    // Clean up intermediate files.
    cleanup_files(&intermediate_files);

    result
}

/// Group paths by their parent directory. Paths without a parent are grouped
/// under an empty path.
fn group_by_parent_dir<P: AsRef<Path>>(paths: &[P]) -> BTreeMap<PathBuf, Vec<&Path>> {
    let mut groups: BTreeMap<PathBuf, Vec<&Path>> = BTreeMap::new();
    for p in paths {
        let p = p.as_ref();
        let parent = p.parent().unwrap_or(Path::new("")).to_path_buf();
        groups.entry(parent).or_default().push(p);
    }
    groups
}

fn cleanup_files(files: &[PathBuf]) {
    for f in files {
        if f.exists()
            && let Err(e) = std::fs::remove_file(f)
        {
            tracing::warn!("Failed to clean up intermediate file {:?}: {}", f, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn merge_strategy_display() {
        assert_eq!(MergeStrategy::Full.to_string(), "full");
        assert_eq!(MergeStrategy::Partial.to_string(), "partial");
        assert_eq!(MergeStrategy::Archive.to_string(), "archive");
    }

    #[test]
    fn group_by_parent_dir_splits_on_directory() {
        let paths = [
            Path::new("/a/one.bc"),
            Path::new("/b/two.bc"),
            Path::new("/a/three.bc"),
        ];
        let groups = group_by_parent_dir(&paths);

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[&PathBuf::from("/a")].len(), 2);
        assert_eq!(groups[&PathBuf::from("/b")].len(), 1);
    }

    #[test]
    fn group_by_parent_dir_single_directory_is_one_group() {
        // `partial` falls back to a full link when this returns one group, so
        // the boundary matters.
        let paths = [Path::new("/a/one.bc"), Path::new("/a/two.bc")];
        assert_eq!(group_by_parent_dir(&paths).len(), 1);
    }

    #[test]
    fn group_by_parent_dir_handles_paths_without_a_parent() {
        let paths = [Path::new("bare.bc")];
        let groups = group_by_parent_dir(&paths);
        assert_eq!(groups.len(), 1);
        assert!(groups.contains_key(&PathBuf::from("")));
    }

    #[test]
    fn cleanup_files_removes_what_exists_and_tolerates_what_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let present = dir.path().join("present.bc");
        let absent = dir.path().join("absent.bc");
        fs::write(&present, b"x").unwrap();

        // The absent path must not panic or abort the cleanup of the rest.
        cleanup_files(&[present.clone(), absent.clone()]);

        assert!(!present.exists(), "existing intermediate was not removed");
        assert!(!absent.exists());
    }
}
