// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

////////////////////////////////////////////////////////////////////////////////
// Put here reusable general purpose utility functions (nothing that is part of
// core functionality).
////////////////////////////////////////////////////////////////////////////////

use std::{
    borrow::{Borrow, ToOwned},
    ffi::{OsStr, OsString},
    iter,
    path::{Path, PathBuf},
    process::Command,
    str::Lines,
};

use log::info;

/// `info_at!(&opt, level, ...)` will print the formatted message `...` if
/// verbosity level is `level` or higher.
macro_rules! info_at {
    ($opt:expr, $lvl:expr, $($arg:tt)+) => ({
        let lvl = $lvl;
        if lvl <= $opt.verbosity {
            println!($($arg)+);
        }
    });
}

/// Use `info!` to print the `cmd`.
pub fn info_cmd(cmd: &Command, name: &str) {
    info!(
        "Running {} on '{}' with command:\n`{}`",
        name,
        cmd.get_current_dir()
            .unwrap_or(&PathBuf::from("."))
            .to_string_lossy(),
        iter::once(cmd.get_program())
            .chain(cmd.get_args())
            .map(|s| shell_escape::escape(s.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    );

    let envs = cmd.get_envs();
    if envs.len() > 0 {
        info!(
            "with environment variables:\n{}",
            envs.map(|(var, val)| match val {
                Some(val) => format!(
                    "{}={}",
                    var.to_string_lossy(),
                    shell_escape::escape(val.to_string_lossy())
                ),
                None => format!("{}=''", var.to_string_lossy()), // explicitly removed
            })
            .collect::<Vec<_>>()
            .join("\n")
        );
    }
}

/// Print each line of `Lines` using `info!`, prefixed with `prefix`.
pub fn info_lines(prefix: &str, lines: Lines) {
    for l in lines {
        info!("{}{}", prefix, l);
    }
}

/// encoding_rs (https://docs.rs/encoding_rs/), seems to be the standard crate
/// for encoding/decoding, has this to say about ISO-8859-1: "ISO-8859-1 does not
/// exist as a distinct encoding from windows-1252 in the Encoding
/// Standard. Therefore, an encoding that maps the unsigned byte value to the
/// same Unicode scalar value is not available via Encoding in this crate."
/// The following is from https://stackoverflow.com/a/28175593
pub fn from_latin1(s: &[u8]) -> String {
    s.iter().map(|&c| c as char).collect()
}

/// The Append trait lets you chain `append` calls where usually you would have
/// to mutate (e.g. using `push`).
/// Example:
/// assert_eq!(String::from("foo").append("bar"), { let mut x = String::from("foo"); x.push_str("bar"); x })
pub trait Append<Segment: ?Sized>: Sized
where
    Segment: ToOwned<Owned = Self>,
    Self: Borrow<Segment>,
{
    fn append(self: Self, s: impl AsRef<Segment>) -> Self;
}

/// Concatenate `s` to the end of `self`.
impl Append<str> for String {
    fn append(mut self: String, s: impl AsRef<str>) -> String {
        self.push_str(s.as_ref());
        self
    }
}

/// Concatenate `s` to the end of `self`.
impl Append<OsStr> for OsString {
    fn append(mut self: OsString, s: impl AsRef<OsStr>) -> OsString {
        self.push(s);
        self
    }
}

/// Add `s` to the end of `self`, as a new component.
impl Append<Path> for PathBuf {
    fn append(mut self: PathBuf, s: impl AsRef<Path>) -> PathBuf {
        self.push(s);
        self
    }
}

/// Example:
/// assert_eq!(add_pre_ext(&PathBuf::from("foo.bar"), "baz"), PathBuf::from("foo.baz.bar"))
pub fn add_pre_ext(file: &Path, ext: impl AsRef<OsStr>) -> PathBuf {
    assert!(file.is_file());

    let new_ext = match file.extension() {
        None => OsString::from(ext.as_ref()),
        Some(old_ext) => OsString::from(ext.as_ref()).append(".").append(old_ext),
    };
    let mut new_file = PathBuf::from(&file);
    new_file.set_extension(&new_ext);
    new_file
}
