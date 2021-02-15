// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use crate::*;

pub fn check_install() -> bool {
    true
}

pub fn run(opt: &Opt) -> CVResult<Status> {
    let mut cmd = Command::new("cargo");
    cmd.arg("test")
        .arg("--manifest-path")
        .arg(&opt.cargo_toml)
        .args(vec!["-v"; opt.verbosity]);

    if !opt.features.is_empty() {
        cmd.arg("--features").arg(opt.features.join(","));
    }

    if opt.tests {
        cmd.arg("--tests");
    }

    for t in &opt.test {
        cmd.arg("--test").arg(t);
    }

    if opt.replay > 0 {
        assert!(opt.args.is_empty());
        cmd.arg("--").arg("--nocapture");
    } else if !opt.args.is_empty() {
        cmd.arg("--").args(&opt.args);
    }

    utils::info_cmd(&cmd, "Proptest");

    let output = cmd.output().expect("Failed to execute `cargo`");

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDERR: ", stderr.lines());
        utils::info_lines("STDOUT: ", stdout.lines());

        for l in stderr.lines() {
            if l.contains("with overflow") {
                return Ok(Status::Overflow);
            }
        }
        Ok(Status::Error)
    } else {
        Ok(Status::Verified)
    }
}
