// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use log::{error, info};
use std::path::PathBuf;
use std::process::Command;
use std::{fs::remove_dir_all, str::from_utf8};

use super::{backends_common, utils, Opt, Status};

pub fn verify(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, _features: &[&str]) -> Status {
    let mut outdir = opt.crate_path.clone();
    outdir.push(&format!("seaout-{}", name));

    // Ignoring result. We don't care if it fails because the path doesn't
    // exist.
    remove_dir_all(&outdir).unwrap_or_default();

    if outdir.exists() {
        error!(
            "Directory or file '{}' already exists, and can't be removed",
            outdir.to_str().unwrap()
        );
        return Status::Unknown;
    }

    info!("     Running Seahorn to verify {}", name);
    info!("      file: {}", bcfile.to_str().unwrap());
    info!("      entry: {}", entry);
    info!("      results: {}", outdir.to_str().unwrap());

    run(&opt, &name, &entry, &bcfile, &outdir)
}

// Return an int indicating importance of a line from KLEE's output
// Low numbers are most important, high numbers least important
//
// -1: script error (always shown)
// 1: brief description of error
// 2: long details about an error
// 3: warnings
// 4: non-KLEE output
// 5: any other KLEE output
fn importance(line: &str, expect: &Option<&str>, name: &str) -> i8 {
    if line.starts_with("VERIFIER_EXPECT:") {
        4
    } else if line == "sat" {
        1
    } else if line.starts_with("Warning: Externalizing function:")
        || line.starts_with("Warning: not lowering an initializer for a global struct:")
    {
        4
    } else if backends_common::is_expected_panic(&line, &expect, &name) || line == "unsat" {
        5
    } else if line.starts_with("Warning:") {
        // Really high priority to force me to categorize it
        0
    } else {
        // Remaining output is probably output from the application, stack dumps, etc.
        3
    }
}

fn run(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, outdir: &PathBuf) -> Status {
    let mut cmd = Command::new("sea");
    cmd.args(&["bpf",
               // The following was extracted from `sea yama -y VCC/seahorn/sea_base.yaml`
               "-O3",
               "--inline",
               "--enable-loop-idiom",
               "--enable-indvar",
               "--no-lower-gv-init-struct",
               "--externalize-addr-taken-functions",
               "--no-kill-vaarg",
               "--with-arith-overflow=true",
               "--horn-unify-assumes=true",
               "--horn-gsa",
               "--no-fat-fns=bcmp,memcpy,assert_bytes_match,ensure_linked_list_is_allocated,sea_aws_linked_list_is_valid",
               "--dsa=sea-cs-t",
               "--devirt-functions=types",
               "--bmc=opsem",
               "--horn-vcgen-use-ite",
               "--horn-vcgen-only-dataflow=true",
               "--horn-bmc-coi=true",
               "--sea-opsem-allocator=static",
               "--horn-explicit-sp0=false",
               "--horn-bv2-lambdas",
               "--horn-bv2-simplify=true",
               "--horn-bv2-extra-widemem",
               "--horn-stats=true",
               "--keep-temps",
    ]);

    cmd.arg(String::from("--temp-dir=") + outdir.to_str().unwrap())
        .arg(String::from("--entry=") + entry);

    match &opt.backend_flags {
        // FIXME: I'm assuming multiple flags are comma separated?
        // Make sure this is also the case when using the cli arg multiple times.
        Some(flags) => {
            cmd.args(flags.split(',').collect::<Vec<&str>>());
        }
        None => (),
    };

    cmd.arg(bcfile.to_str().unwrap())
        // .args(&opt.args)
        .current_dir(&opt.crate_path);

    utils::info_cmd(&cmd, "Seahorn");

    let output = cmd.output().expect("Failed to execute `sea`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    // if !output.status.success() {
    //     utils::info_lines("STDOUT: ", stdout.lines());
    //     utils::info_lines("STDERR: ", stderr.lines());
    //     error!("`sea` terminated unsuccessfully");
    //     exit(1);
    // }

    // We scan stderr for:
    // 1. Indications of the expected output (eg from #[should_panic])
    // 2. Indications of success/failure
    // 3. Information relevant at the current level of verbosity
    // 4. Statistics

    // Scan for expectation message
    let mut expect = None;
    for l in stderr.lines() {
        if l == "VERIFIER_EXPECT: should_panic" {
            expect = Some("");
        } else if let Some(e) = l
            .strip_prefix("VERIFIER_EXPECT: should_panic(expected = \"")
            .and_then(|l| l.strip_suffix("\")"))
        {
            info!("Expecting '{}'", e);
            expect = Some(e);
        }
    }

    // Scan for first message that indicates result
    let status = stderr
        .lines()
        .chain(stdout.lines())
        .find_map(|l| {
            if l.starts_with("VERIFIER_EXPECT:") {
                // don't confuse this line with an error!
                None
            } else if backends_common::is_expected_panic(&l, &expect, &name) {
                Some(Status::Verified)
            } else if l == "sat" {
                Some(Status::Error)
            } else if l == "unsat" {
                match expect {
                    None => Some(Status::Verified),
                    _ => Some(Status::Error),
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            info!("Unable to determine status of {}", name);
            Status::Unknown
        });

    info!("Status: '{}' expected: '{:?}'", status, expect);

    // TODO: Scan for statistics

    utils::info_lines("STDOUT: ", stdout.lines());

    for l in stderr.lines() {
        if importance(&l, &expect, &name) < opt.verbosity as i8 {
            println!("{}", l);
        }
    }

    status
}
