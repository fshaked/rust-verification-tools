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
use std::process::Command;
use std::str::Lines;

pub fn info_cmd(cmd: &Command, name: &str) {
    info!(
        "Running {} on '{}' with command `{} {}`",
        name,
        cmd.get_current_dir().unwrap().to_str().unwrap(),
        cmd.get_program().to_str().unwrap(),
        cmd.get_args()
            .map(|s| s.to_str().unwrap())
            .collect::<String>()
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
