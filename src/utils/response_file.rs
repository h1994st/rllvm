//! GNU host response-file expansion and transport for Clang/LLVM invocations.
//!
//! `execve` rejects an argument list larger than `ARG_MAX`, which a
//! whole-program link reaches at a few thousand translation units. LLVM tools
//! accept `@<path>` in place of arguments and read the arguments out of that
//! file instead, which keeps the argument list two entries long however many
//! bitcode files there are.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
};

use tempfile::NamedTempFile;

use crate::{constants::RESPONSE_FILE_ARGUMENT_THRESHOLD, error::Error};

use super::execute_command_for_status;

// Bound recursive and exponentially repeated input before allocating its
// expanded argument list. These limits apply to classification, not argv.
const MAX_RESPONSE_DEPTH: usize = 128;
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RESPONSE_ARGUMENTS: usize = 1_000_000;

/// Expand Clang's GNU host response syntax for classification only. The real
/// compiler must still receive the original argv. Nested filenames, like all
/// other relative arguments, resolve from the process working directory.
pub(crate) fn expand_response_files(args: &[String]) -> Result<Vec<String>, Error> {
    let mut expansion = ResponseExpansion {
        active: Vec::new(),
        bytes_left: MAX_RESPONSE_BYTES,
        arguments_left: MAX_RESPONSE_ARGUMENTS,
        args: Vec::new(),
    };
    for arg in args {
        expansion.append(arg.clone())?;
    }
    Ok(expansion.args)
}

struct ResponseExpansion {
    active: Vec<PathBuf>,
    bytes_left: u64,
    arguments_left: usize,
    args: Vec<String>,
}

impl ResponseExpansion {
    fn append(&mut self, arg: String) -> Result<(), Error> {
        self.arguments_left = self.arguments_left.checked_sub(1).ok_or_else(|| {
            Error::InvalidArguments("response file expansion exceeds argument limit".into())
        })?;
        let Some(filename) = arg.strip_prefix('@') else {
            self.args.push(arg);
            return Ok(());
        };
        let invalid = |reason: String| {
            Error::InvalidArguments(format!("response file {filename:?}: {reason}"))
        };
        if self.active.len() >= MAX_RESPONSE_DEPTH {
            return Err(invalid("expansion exceeds depth limit".into()));
        }
        let path = match Path::new(filename).canonicalize() {
            Ok(path) => path,
            // Clang leaves missing @names literal. They may be valid option
            // values such as Mach-O's @rpath install names; the compiler
            // diagnoses a missing response used as an input filename.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.args.push(arg);
                return Ok(());
            }
            Err(e) => return Err(invalid(e.to_string())),
        };
        if self.active.contains(&path) {
            return Err(invalid("expansion cycle".into()));
        }
        let metadata = fs::metadata(&path).map_err(|e| invalid(e.to_string()))?;
        // Opening a FIFO can block forever. Only regular response files are
        // supported, including symlinks resolving to regular files.
        if !metadata.is_file() {
            return Err(invalid("expected a regular file".into()));
        }
        if metadata.len() > self.bytes_left {
            return Err(invalid("expansion exceeds size limit".into()));
        }
        let mut contents = String::new();
        File::open(&path)
            .map_err(|e| invalid(e.to_string()))?
            .take(self.bytes_left + 1)
            .read_to_string(&mut contents)
            .map_err(|e| invalid(format!("cannot read UTF-8 text: {e}")))?;
        if contents.len() as u64 > self.bytes_left {
            return Err(invalid("expansion exceeds size limit".into()));
        }
        self.bytes_left -= contents.len() as u64;
        if contents.contains('\0') {
            return Err(invalid("NUL bytes are not supported".into()));
        }
        self.active.push(path);
        let contents = contents.strip_prefix('\u{feff}').unwrap_or(&contents);
        // GNU response syntax is not shell syntax: backslashes escape the
        // next character even in single quotes, no substitutions run, and
        // an unfinished quote extends to EOF. Clang discards empty tokens.
        let mut chars = contents.chars();
        let mut quote = None;
        let mut token = String::new();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => token.push(chars.next().unwrap_or('\\')),
                '\'' | '"' if quote.is_none() => quote = Some(ch),
                ch if quote == Some(ch) => quote = None,
                ' ' | '\t' | '\r' | '\n' if quote.is_none() => {
                    if !token.is_empty() {
                        self.append(std::mem::take(&mut token))?;
                    }
                }
                _ => token.push(ch),
            }
        }
        if !token.is_empty() {
            self.append(token)?;
        }
        self.active.pop();
        Ok(())
    }
}

/// Characters that LLVM's GNU-style tokenizer reads as separators or quoting,
/// and which therefore have to be escaped to survive as part of an argument.
///
/// Unescaped, a path containing a space arrives as two arguments and the tool
/// reports a file it was never asked for. The Windows tokenizer uses different
/// rules, but rllvm has no Windows host support -- COFF appears here only as a
/// cross-compilation target -- so only the GNU form is produced.
const CHARACTERS_NEEDING_ESCAPE: &[u8] = b"\\\"' \t\n\r";

/// Escape one argument for a response file.
fn escape_response_file_argument(argument: impl AsRef<OsStr>) -> Vec<u8> {
    // Escape only ASCII syntax bytes, preserving the rest of Unix OsStr
    // arguments rather than replacing non-UTF-8 filename bytes.
    let argument = argument.as_ref().as_encoded_bytes();
    let mut escaped = Vec::with_capacity(argument.len());
    for &byte in argument {
        if CHARACTERS_NEEDING_ESCAPE.contains(&byte) {
            escaped.push(b'\\');
        }
        escaped.push(byte);
    }
    escaped
}

/// Whether this many arguments should travel in a response file.
fn needs_response_file(argument_count: usize) -> bool {
    argument_count > RESPONSE_FILE_ARGUMENT_THRESHOLD
}

/// Run an LLVM tool, passing nonempty argument runs through response files
/// when their count or total size exceeds the direct-argument threshold.
pub(crate) fn execute_llvm_tool<P, S>(program_filepath: P, args: &[S]) -> Result<ExitStatus, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    // A few large defines can exceed ARG_MAX even below the count limit.
    let argument_bytes = args.iter().fold(0usize, |total, arg| {
        total.saturating_add(arg.as_ref().as_encoded_bytes().len() + 1)
    });
    if !needs_response_file(args.len()) && argument_bytes <= 32 * 1024 {
        return execute_command_for_status(program_filepath, args);
    }

    let mut response_files = Vec::new();
    let mut invocation_args = Vec::new();
    // Clang drops empty tokens read from GNU response files, even quoted
    // ones. Keep direct empty argv inline so options such as `-I ""` do
    // not consume the next flag. Each nonempty run gets its own response.
    for (index, run) in args.split(|arg| arg.as_ref().is_empty()).enumerate() {
        if index > 0 {
            invocation_args.push(OsStr::new("").to_os_string());
        }
        if run.is_empty() {
            continue;
        }
        let mut response_file = NamedTempFile::new().map_err(Error::Io)?;
        for arg in run {
            response_file.write_all(&escape_response_file_argument(arg.as_ref()))?;
            response_file.write_all(b"\n")?;
        }
        // The child reads through its own descriptor after all runs exist.
        response_file.flush().map_err(Error::Io)?;

        tracing::debug!(
            "Passing {} arguments to {:?} through the response file {:?}",
            run.len(),
            program_filepath.as_ref(),
            response_file.path()
        );

        let mut argument = OsStr::new("@").to_os_string();
        argument.push(response_file.path());
        invocation_args.push(argument);
        response_files.push(response_file);
    }

    // Every temporary stays alive until the child has finished reading it.
    execute_command_for_status(program_filepath, &invocation_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_leaves_an_ordinary_path_alone() {
        assert_eq!(
            escape_response_file_argument("/tmp/build/foo.bc"),
            b"/tmp/build/foo.bc"
        );
    }

    /// Documents the mapping. What proves the mapping is right for LLVM is
    /// `links_bitcode_files_whose_paths_need_escaping`, which runs the tool.
    #[test]
    fn escaping_covers_separators_and_quoting() {
        assert_eq!(
            escape_response_file_argument("/tmp/a dir/x.bc"),
            br"/tmp/a\ dir/x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/it's/x.bc"),
            br"/tmp/it\'s/x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/say\"hi\"/x.bc"),
            br#"/tmp/say\"hi\"/x.bc"#
        );
        assert_eq!(
            escape_response_file_argument(r"C:\tmp\x.bc"),
            br"C:\\tmp\\x.bc"
        );
        assert_eq!(
            escape_response_file_argument("/tmp/tab\there/x.bc"),
            b"/tmp/tab\\\there/x.bc"
        );
    }

    #[test]
    fn the_threshold_is_the_last_count_passed_directly() {
        assert!(!needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD - 1));
        assert!(!needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD));
        assert!(needs_response_file(RESPONSE_FILE_ARGUMENT_THRESHOLD + 1));
    }
}
