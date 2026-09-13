//! Prefix classification shared by compiler wrappers, inventory, and CLIs.

use std::{
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
};

use crate::error::Error;

/// Content prefixes that need distinct handling before object/archive parsing.
///
/// Classification does not validate the complete bitcode or JSON document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputKind {
    /// Raw LLVM bitcode or the bitcode wrapper emitted on Darwin.
    Bitcode,
    /// A leading JSON object delimiter, possibly after ASCII whitespace.
    /// A catalog reader must still validate the JSON and its schema.
    JsonObject,
    /// Any other prefix, including native objects, archives, and empty files.
    Other,
}

impl InputKind {
    /// Classify bytes or a buffered stream by its prefix, consuming as needed.
    ///
    /// Pass a byte slice to reuse data already read from a file or archive.
    pub fn from_reader(mut input: impl BufRead) -> Result<Self, Error> {
        // Magic must start at byte zero; whitespace is allowed only for JSON.
        if matches!(input.fill_buf()?.first(), Some(b'B' | 0xde)) {
            let mut magic = [0; 4];
            return match input.read_exact(&mut magic) {
                Ok(()) => Ok(
                    if magic == *b"BC\xc0\xde" || magic == [0xde, 0xc0, 0x17, 0x0b] {
                        Self::Bitcode
                    } else {
                        Self::Other
                    },
                ),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(Self::Other),
                Err(error) => Err(error.into()),
            };
        }

        loop {
            let prefix = input.fill_buf()?;
            if prefix.is_empty() {
                return Ok(Self::Other);
            }
            if let Some(byte) = prefix.iter().find(|byte| !byte.is_ascii_whitespace()) {
                return Ok(if *byte == b'{' {
                    Self::JsonObject
                } else {
                    Self::Other
                });
            }
            let consumed = prefix.len();
            input.consume(consumed);
        }
    }

    /// Open a file once and classify its prefix without loading its contents.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_reader(BufReader::new(File::open(path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::InputKind;
    use std::io::{self, BufReader, Cursor};

    #[test]
    fn input_classification_agrees_for_files_and_fragmented_readers() {
        let directory = tempfile::tempdir().unwrap();
        // Deliberately misleading suffix: dispatch depends on content.
        let path = directory.path().join("input.bc");
        let cases: &[(&[u8], InputKind)] = &[
            (b"BC\xc0\xdepayload", InputKind::Bitcode),
            (b"\xde\xc0\x17\x0bpayload", InputKind::Bitcode),
            (b"", InputKind::Other),
            (b"BC\xc0", InputKind::Other),
            (b"\xde\xc0\x17", InputKind::Other),
            (b"Bogus magic", InputKind::Other),
            (b"\xde\xc0\x17\x00", InputKind::Other),
            (b"\x7fELF", InputKind::Other),
            (b"!<arch>\n", InputKind::Other),
            (b" \t\n", InputKind::Other),
            (b" BC\xc0\xde", InputKind::Other),
            (b"[{}]", InputKind::Other),
            (b"{", InputKind::JsonObject),
            (b" \r\n\t{not valid JSON", InputKind::JsonObject),
        ];
        for &(bytes, expected) in cases {
            assert_eq!(InputKind::from_reader(bytes).unwrap(), expected);
            let fragmented = BufReader::with_capacity(1, Cursor::new(bytes));
            assert_eq!(InputKind::from_reader(fragmented).unwrap(), expected);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(InputKind::from_path(&path).unwrap(), expected);
        }

        let mut padded = vec![b' '; 32 * 1024];
        padded.extend_from_slice(b"{\"kind\": \"rllvm-module-catalog\"}");
        std::fs::write(&path, padded).unwrap();
        assert_eq!(InputKind::from_path(path).unwrap(), InputKind::JsonObject);
    }

    #[test]
    fn input_classification_stops_after_the_deciding_prefix() {
        for (bytes, consumed) in [
            (&b"BC\xc0\xdepayload"[..], 4),
            (&b"\xde\xc0\x17\x0bpayload"[..], 4),
            (&b"\x7fELFpayload"[..], 1),
            (&b" \t{payload"[..], 3),
        ] {
            let mut input = Cursor::new(bytes);
            InputKind::from_reader(&mut input).unwrap();
            assert!(input.position() <= consumed);
        }
    }

    #[test]
    fn input_classification_propagates_io_errors() {
        struct Broken;
        impl io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("unreadable input"))
            }
        }
        let broken = BufReader::new(Broken);
        assert!(InputKind::from_reader(broken).is_err());
        // Also propagate an error after recognizing the first magic byte.
        let broken = io::Read::chain(Cursor::new(b"B"), Broken);
        assert!(InputKind::from_reader(BufReader::with_capacity(1, broken)).is_err());
        let broken = io::Read::chain(Cursor::new(b" \t"), Broken);
        assert!(InputKind::from_reader(BufReader::with_capacity(1, broken)).is_err());
        let directory = tempfile::tempdir().unwrap();
        assert!(InputKind::from_path(directory.path().join("missing")).is_err());
    }
}
