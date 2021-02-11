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

use log::info;
use std::{
    borrow::{Borrow, ToOwned},
    ffi::{OsStr, OsString},
    path::PathBuf,
    str::Lines,
};
use std::{path::Path, process::Command};

pub fn info_cmd(cmd: &Command, name: &str) {
    info!(
        "Running {} on '{}' with command `{} {}`",
        name,
        cmd.get_current_dir().and_then(Path::to_str).unwrap_or("."),
        cmd.get_program().to_str().unwrap_or("???"),
        cmd.get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    );
}

pub fn info_lines(prefix: &str, lines: Lines) {
    for l in lines {
        info!("{}{}", prefix, l);
    }
}

// encoding_rs (https://docs.rs/encoding_rs/), seems to be the standard crate
// for encoding/decoding, has this to say about ISO-8859-1: "ISO-8859-1 does not
// exist as a distinct encoding from windows-1252 in the Encoding
// Standard. Therefore, an encoding that maps the unsigned byte value to the
// same Unicode scalar value is not available via Encoding in this crate."
// The following is from https://stackoverflow.com/a/28175593
pub fn from_latin1(s: &[u8]) -> String {
    s.iter().map(|&c| c as char).collect()
}

pub trait Append<Segment: ?Sized>: Sized
where
    Segment: ToOwned<Owned = Self>,
    Self: Borrow<Segment>,
{
    fn append(self: Self, s: impl AsRef<Segment>) -> Self;
}

impl Append<str> for String {
    fn append(mut self: String, s: impl AsRef<str>) -> String {
        self.push_str(s.as_ref());
        self
    }
}

impl Append<OsStr> for OsString {
    fn append(mut self: OsString, s: impl AsRef<OsStr>) -> OsString {
        self.push(s);
        self
    }
}

impl Append<Path> for PathBuf {
    fn append(mut self: PathBuf, s: impl AsRef<Path>) -> PathBuf {
        self.push(s);
        self
    }
}

pub fn add_pre_ext<T: AsRef<OsStr>>(file: &Path, ext: T) -> PathBuf {
    assert!(file.is_file());

    let new_ext = match file.extension() {
        None => OsString::from(ext.as_ref()),
        Some(old_ext) => OsString::from(ext.as_ref()).append(".").append(old_ext),
    };
    let mut new_file = PathBuf::from(&file);
    new_file.set_extension(&new_ext);
    new_file
}
