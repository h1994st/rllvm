//! Command execution utility functions

use std::{
    ffi::OsStr,
    path::Path,
    process::{Command, ExitStatus, Output},
};

use crate::error::Error;

pub fn execute_command_for_status<P, S>(
    program_filepath: P,
    args: &[S],
) -> Result<ExitStatus, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    let output = execute_command_for_output(program_filepath, args)?;
    Ok(output.status)
}

pub fn execute_command_for_output<P, S>(program_filepath: P, args: &[S]) -> Result<Output, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    let program_filepath = program_filepath.as_ref();
    Command::new(program_filepath)
        .args(args)
        .output()
        .map_err(Error::Io)
}

pub fn execute_command_for_stdout_string<P, S>(
    program_filepath: P,
    args: &[S],
) -> Result<String, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    let output = execute_command_for_output(program_filepath, args)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

pub fn execute_command_for_stderr_string<P, S>(
    program_filepath: P,
    args: &[S],
) -> Result<String, Error>
where
    P: AsRef<Path>,
    S: AsRef<OsStr>,
{
    let output = execute_command_for_output(program_filepath, args)?;
    Ok(String::from_utf8(output.stderr)?.trim().to_string())
}
