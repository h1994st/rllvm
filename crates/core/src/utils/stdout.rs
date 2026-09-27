use std::io::{ErrorKind, Write};

use crate::error::Error;

/// Writes `text` to stdout and flushes it.
///
/// A reader that closed the pipe early, as `| head` does, has taken all it
/// wanted, so that is success rather than the panic `print!` raises.
pub fn print_stdout(text: &str) -> Result<(), Error> {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        result => Ok(result?),
    }
}
