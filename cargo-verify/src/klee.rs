// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use lazy_static::lazy_static;
use log::{info, log, warn};
use regex::Regex;
use std::path::Path;
use std::process::Command;
use std::{collections::HashMap, ffi::OsString, fs::remove_dir_all, str::from_utf8};

use crate::utils::Append;

use super::{backends_common, utils, CVResult, Opt, Status};

pub fn verify(
    opt: &Opt,
    name: &str,
    entry: &str,
    bcfile: &Path,
    features: &[&str],
) -> CVResult<Status> {
    let out_dir = opt.crate_path.clone().append(&format!("kleeout-{}", name));

    // Ignoring result. We don't care if it fails because the path doesn't
    // exist.
    remove_dir_all(&out_dir).unwrap_or_default();
    if out_dir.exists() {
        Err(format!(
            "Directory or file '{:?}' already exists, and can't be removed",
            out_dir
        ))?
    }

    info!("     Running KLEE to verify {}", name);
    info!("      file: {:?}", bcfile);
    info!("      entry: {}", entry);
    info!("      results: {:?}", out_dir);

    let (status, stats) = run(&opt, &name, &entry, &bcfile, &out_dir)?;
    if !stats.is_empty() {
        match stats.get("completed paths") {
            Some(n) => log!(log::Level::Warn, "     {}: {} paths", name, n),
            None => (),
        }
        info!("     {}: {:?}", name, stats);
    }

    lazy_static! {
        static ref TEST_ERR: Regex = Regex::new(r"test.*\.err$").unwrap();
        static ref TEST_KTEST: Regex = Regex::new(r"test.*\.ktest$").unwrap();
    }

    // {out_dir}/test*.err
    let mut failures = out_dir
        .read_dir()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file() && TEST_ERR.is_match(p.file_name().unwrap().to_str().expect("not UTF-8"))
        })
        .collect::<Vec<_>>();
    failures.sort_unstable();
    info!("      Failing test: {:?}", failures);

    if opt.replay > 0 {
        // use -r -r to see all tests, not just failing tests
        let mut ktests = if opt.replay > 1 {
            // {out_dir}/test*.ktest
            out_dir
                .read_dir()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && TEST_KTEST.is_match(p.file_name().unwrap().to_str().expect("not UTF-8"))
                })
                .collect::<Vec<_>>()
        } else {
            // Remove the '.err' extension and replace the '.*' ('.abort' or
            // '.ptr') with '.ktest'.
            failures
                .iter()
                .map(|p| {
                    p.with_extension("") // Remove '.err'
                        .with_extension("ktest") // Replace '.*' with '.ktest'
                })
                .collect::<Vec<_>>()
        };
        ktests.sort_unstable();

        for ktest in ktests {
            println!("    Test input {}", ktest.to_str().unwrap_or("???"));
            match replay_klee(&opt, &name, &ktest, &features) {
                Ok(()) => (),
                Err(err) => warn!("Failed to replay: {}", err),
            }
        }
    }

    Ok(status)
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
    } else if backends_common::is_expected_panic(&line, &expect, &name) {
        // low priority because we report it directly
        5
    } else if line.contains("assertion failed") {
        1
    } else if line.contains("verification failed") {
        1
    } else if line.contains("with overflow") {
        1
    } else if line.starts_with("KLEE: ERROR: Could not link") {
        -1
    } else if line.starts_with("KLEE: ERROR: Unable to load symbol") {
        -1
    } else if line.starts_with("KLEE: ERROR:") {
        2
    } else if line.starts_with("warning: Linking two modules of different data layouts") {
        4
    } else if line.contains("KLEE: WARNING:") {
        4
    } else if line.contains("KLEE: WARNING ONCE:") {
        4
    } else if line.starts_with("KLEE: output directory") {
        5
    } else if line.starts_with("KLEE: Using") {
        5
    } else if line.starts_with("KLEE: NOTE: Using POSIX model") {
        5
    } else if line.starts_with("KLEE: done:") {
        5
    } else if line.starts_with("KLEE: HaltTimer invoked") {
        5
    } else if line.starts_with("KLEE: halting execution, dumping remaining states") {
        5
    } else if line.starts_with("KLEE: NOTE: now ignoring this error at this location") {
        5
    } else if line.starts_with("KLEE:") {
        // Really high priority to force me to categorize it
        0
    } else {
        // Remaining output is probably output from the application, stack dumps, etc.
        3
    }
}

fn run(
    opt: &Opt,
    name: &str,
    entry: &str,
    bcfile: &Path,
    out_dir: &Path,
) -> CVResult<(Status, HashMap<String, isize>)> {
    let mut cmd = Command::new("klee");
    cmd.args(&[
        "--exit-on-error",
        "--entry-point",
        entry,
        // "--posix-runtime",
        // "--libcxx",
        "--libc=klee",
        "--silent-klee-assume",
        "--disable-verify", // workaround https://github.com/klee/klee/issues/937
    ])
    .arg("--output-dir")
    .arg(out_dir);

    match &opt.backend_flags {
        // FIXME: I'm assuming multiple flags are comma separated?
        Some(opt_flags) => {
            cmd.args(opt_flags.split(',').collect::<Vec<&str>>());
        }
        None => (),
    }

    cmd.arg(bcfile).args(&opt.args).current_dir(&opt.crate_path);

    utils::info_cmd(&cmd, "KLEE");

    let output = cmd.output()?;

    let stdout = utils::from_latin1(&output.stdout);
    let stderr = utils::from_latin1(&output.stderr);

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
        .find_map(|l| {
            if l.starts_with("KLEE: HaltTimer invoked") {
                Some(Status::Timeout)
            } else if l.starts_with("KLEE: halting execution, dumping remaining states") {
                Some(Status::Timeout)
            } else if l.starts_with("KLEE: ERROR: Could not link") {
                Some(Status::Unknown)
            } else if l.starts_with("KLEE: ERROR: Unable to load symbol") {
                Some(Status::Unknown)
            } else if l.starts_with("KLEE: ERROR:") && l.contains("unreachable") {
                Some(Status::Reachable)
            } else if l.starts_with("KLEE: ERROR:") && l.contains("overflow") {
                Some(Status::Overflow)
            } else if l.starts_with("KLEE: ERROR:") {
                Some(Status::Error)
            } else if l.starts_with("VERIFIER_EXPECT:") {
                // don't confuse this line with an error!
                None
            } else if backends_common::is_expected_panic(&l, &expect, &name) {
                Some(Status::Verified)
            } else if l.contains("assertion failed") {
                Some(Status::Error)
            } else if l.contains("verification failed") {
                Some(Status::Error)
            } else if l.contains("with overflow") {
                Some(Status::Overflow)
            } else if l.contains("note: run with `RUST_BACKTRACE=1`") {
                Some(Status::Error)
            } else if l.contains("KLEE: done:") {
                match expect {
                    None => Some(Status::Verified),
                    _ => Some(Status::Error),
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| {
            warn!("Unable to determine status of {}", name);
            Status::Unknown
        });

    info!("Status: '{}' expected: '{:?}'", status, expect);

    // Scan for statistics
    lazy_static! {
        static ref KLEE_DONE: Regex = Regex::new(r"^KLEE: done:\s+(.*)= (\d+)").unwrap();
    }

    let stats: HashMap<String, isize> = stderr
        .lines()
        // .filter(|l| l.starts_with("KLEE: done:"))
        .filter_map(|l| {
            KLEE_DONE.captures(l).and_then(|caps| {
                // If the value doesn't parse we throw the line.
                caps.get(2)
                    .unwrap()
                    .as_str()
                    .parse::<isize>()
                    .ok()
                    .map(|v| (caps.get(1).unwrap().as_str().trim().to_string(), v))
            })
        })
        .collect();

    utils::info_lines("STDOUT: ", stdout.lines());

    for l in stderr.lines() {
        if importance(&l, &expect, &name) < opt.verbosity as i8 {
            println!("{}", l);
        }
    }

    Ok((status, stats))
}

// Replay a KLEE "ktest" file
fn replay_klee(opt: &Opt, name: &str, ktest: &Path, features: &[&str]) -> CVResult<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&opt.crate_path);

    if opt.tests || !opt.test.is_empty() {
        cmd.arg("test");

        if !features.is_empty() {
            cmd.arg("--features").arg(features.join(","));
        }

        cmd.arg(&name).args(&["--", "--nocapture"]);
    } else {
        cmd.arg("run");

        if !features.is_empty() {
            cmd.arg("--features").arg(features.join(","));
        }

        if !opt.args.is_empty() {
            cmd.arg("--").args(opt.args.iter());
        }
    }

    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(env_rustflags) => env_rustflags.append(" --cfg=verify"),
        None => OsString::from("--cfg=verify"),
    };
    cmd.env("RUSTFLAGS", rustflags).env("KTEST_FILE", ktest);

    utils::info_cmd(&cmd, "Replay");
    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout)?;
    let stderr = from_utf8(&output.stderr)?;

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't replay")?
    }

    Ok(())
}
