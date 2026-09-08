//! Response files for LLVM tool invocations.
//!
//! `execve` rejects an argument list larger than `ARG_MAX`, which a
//! whole-program link reaches at a few thousand translation units. LLVM tools
//! accept `@<path>` in place of arguments and read the arguments out of that
//! file instead, which keeps the argument list two entries long however many
//! bitcode files there are.

use std::{ffi::OsStr, io::Write, path::Path, process::ExitStatus};

use tempfile::NamedTempFile;

use crate::{constants::RESPONSE_FILE_ARGUMENT_THRESHOLD, error::Error};

use super::execute_command_for_status;

/// Characters that LLVM's GNU-style tokenizer reads as separators or quoting,
/// and which therefore have to be escaped to survive as part of an argument.
///
/// Unescaped, a path containing a space arrives as two arguments and the tool
/// reports a file it was never asked for. The Windows tokenizer uses different
/// rules, but rllvm has no Windows host support -- COFF appears here only as a
/// cross-compilation target -- so only the GNU form is produced.
const CHARACTERS_NEEDING_ESCAPE: &[char] = &['\\', '"', '\'', ' ', '\t', '\n', '\r'];

/// Escape one argument for a response file.
fn escape_response_file_argument(argument: &str) -> String {
    let mut escaped = String::with_capacity(argument.len());
    for character in argument.chars() {
        if CHARACTERS_NEEDING_ESCAPE.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Whether this many arguments should travel in a response file.
fn needs_response_file(argument_count: usize) -> bool {
    argument_count > RESPONSE_FILE_ARGUMENT_THRESHOLD
}

/// Run an LLVM tool, passing the arguments through a response file when there
/// are more of them than an argument list is guaranteed to hold.
pub(crate) fn execute_llvm_tool<P>(
    program_filepath: P,
    args: &[String],
) -> Result<ExitStatus, Error>
where
    P: AsRef<Path>,
{
    if !needs_response_file(args.len()) {
        return execute_command_for_status(program_filepath, args);
    }

    let mut response_file = NamedTempFile::new().map_err(Error::Io)?;
    for arg in args {
        writeln!(response_file, "{}", escape_response_file_argument(arg)).map_err(Error::Io)?;
    }
    // The child reads the file through its own descriptor, so anything still
    // sitting in this process's buffer would be invisible to it.
    response_file.flush().map_err(Error::Io)?;

    tracing::debug!(
        "Passing {} arguments to {:?} through the response file {:?}",
        args.len(),
        program_filepath.as_ref(),
        response_file.path()
    );

    let response_argument = {
        let mut argument = OsStr::new("@").to_os_string();
        argument.push(response_file.path());
        argument
    };

    // `response_file` stays bound until this returns: dropping it deletes the
    // file, and the child has not read it yet.
    execute_command_for_status(program_filepath, &[response_argument])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_leaves_an_ordinary_path_alone() {
        assert_eq!(
            escape_response_file_argument("/tmp/build/foo.bc"),
            "/tmp/build/foo.bc"
        );
    }

    /// Documents the mapping. What proves the mapping is right for LLVM is
    /// `links_bitcode_files_whose_paths_need_escaping`, which runs the tool.
    #[test]
    fn escaping_covers_separators_and_quoting() {
        assert_eq!(
            escape_response_file_argument("/tmp/a dir/x.bc"),
            r"/tmp/a\ dir/x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/it's/x.bc"),
            r"/tmp/it\'s/x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/say\"hi\"/x.bc"),
            r#"/tmp/say\"hi\"/x.bc"#
        );
        assert_eq!(
            escape_response_file_argument(r"C:\tmp\x.bc"),
            r"C:\\tmp\\x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/tab\there/x.bc"),
            "/tmp/tab\\\there/x.bc"
        );
    }

    #[test]
    fn the_threshold_is_the_last_count_passed_directly() {
        assert!(!needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD - 1));
        assert!(!needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD));
        assert!(needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD + 1));
    }
}
