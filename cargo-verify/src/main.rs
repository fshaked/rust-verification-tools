// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(command_access)]

use log::{log, error, info};
use regex::Regex;
use std::{collections::{HashMap, HashSet}, ffi::{OsStr, OsString}, fmt, fs::{remove_dir_all, remove_file}, iter::{self, FromIterator}, process::exit, str::{FromStr, Lines, from_utf8}, string::ParseError};
use std::path::PathBuf;
use structopt::StructOpt;
use lazy_static::lazy_static;
use std::process::Command;
use cargo_metadata::MetadataCommand;
use rustc_demangle::demangle;

// Command line argument parsing
#[derive(StructOpt)]
#[structopt(
    name = "cargo-verify",
    about = "Execute verification tools",
    // version number is taken automatically from Cargo.toml
)]
struct Opt {
    /// Filesystem path to local crate to verify
    #[structopt(name = "PATH", parse(from_os_str))]
    crate_path: PathBuf,

    /// Arguments to pass to program under test
    #[structopt(name = "ARG")]
    args: Vec<String>,

    /// Select verification backend
    #[structopt(
        short,
        long,
        name = "BACKEND",
        default_value = "klee", // FIXME: is that a sensible choice?
    )]
    backend: Backend,

    /// Extra verification flags
    #[structopt(long)]
    backend_flags: Option<String>,

    /// Run `cargo clean` first
    #[structopt(short, long)]
    clean: bool,

    /// Verify all tests instead of 'main'
    #[structopt(short, long)]
    tests: bool,

    /// Only verify tests containing this string in their names
    #[structopt(long, name = "TESTNAME")]
    test: Vec<String>,

    /// Number of parallel jobs, defaults to # of CPUs
    #[structopt(short, long, name = "N")]
    job: Option<Option<usize>>,

    /// Replay to display concrete input values
    #[structopt(short, long, parse(from_occurrences))]
    replay: usize,

    /// Increase message verbosity
    #[structopt(short, long, parse(from_occurrences))]
    verbosity: usize,

}

#[derive(Debug, PartialEq)]
enum Backend {
    Proptest,
    Klee,
    Seahorn,
}

impl FromStr for Backend {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "proptest" => Ok(Backend::Proptest),
            "klee" => Ok(Backend::Klee),
            "seahorn" => Ok(Backend::Seahorn),
            _ => Err(String::from("unrecognised backend")),
        }
    }
}




fn main() {
    let opt = Opt::from_args();

    #[rustfmt::skip]
    stderrlog::new()
        .verbosity(opt.verbosity)
        .init()
        .unwrap();

    if opt.backend == Backend::Proptest {
        if opt.replay > 0 && ! opt.args.is_empty() {
            error!("The proptest backend does not support '--replay' and passing arguments together.");
            exit(1);
        }
    }

    if opt.backend == Backend::Seahorn {
        if ! opt.args.is_empty() {
            error!("The Seahorn backend does not support passing arguments yet.");
            exit(1);
        }
        if opt.replay != 0 {
            error!("The Seahorn backend does not support '--replay' yet.");
            exit(1);
        }
    }

    let features =
        match opt.backend {
            Backend::Klee => vec!["--features", "verifier-klee"],
            Backend::Proptest => vec!["--features", "verifier-seahorn"],
            Backend::Seahorn => vec![],
        };

    if opt.clean {
        Command::new("cargo")
            .arg("clean")
            .current_dir(&opt.crate_path)
            .output()
            .ok(); // Discarding the error on purpose.
    }

    let package = get_meta_package_name(&opt.crate_path);
    info!("Checking {}", &package);

    let status =
        match opt.backend {
            Backend::Proptest => {
                info!("  Invoking cargo run with proptest backend");
                run_proptest(&opt, &features)
            }
            _ => {
                let target = get_default_host(&opt.crate_path);
                info!("target: {}", target);
                verify(&opt, &package, &features, &target)
            }
        };

    println!("VERIFICATION_RESULT: {}", status);

    if status != Status::Verified {
        exit(1);
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
enum Status {
    Unknown,
    Verified,
    Error,
    Timeout,
    Overflow,
    Reachable,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Unknown   => write!(f, "Unknown"),
            Status::Verified  => write!(f, "Verified"),
            Status::Error     => write!(f, "Error"),
            Status::Timeout   => write!(f, "Timeout"),
            Status::Overflow  => write!(f, "Overflow"),
            Status::Reachable => write!(f, "Reachable"),
        }
    }
}

fn get_meta_package_name(crate_dir: &PathBuf) -> String {
    MetadataCommand::new()
        .manifest_path(crate_dir.iter().chain(iter::once(OsStr::new("Cargo.toml"))).collect::<PathBuf>())
        // .features(CargoOpt::AllFeatures)
        .exec()
        .unwrap()
        .root_package()
        .unwrap()
        .name
        .replace(|c| match c {
            'a'..='z' |
            'A'..='Z' |
            '0'..='9' |
            '_' => false,
            _ => true,
        }, "_")
}

fn get_meta_target_directory(crate_dir: &PathBuf) -> PathBuf {
    // FIXME: add '--cfg=verify' to RUSTFLAGS, pass features to the command
    MetadataCommand::new()
        .manifest_path(crate_dir.iter().chain(iter::once(OsStr::new("Cargo.toml"))).collect::<PathBuf>())
        // .features(CargoOpt::AllFeatures)
        .exec()
        .unwrap()
        .target_directory
}

fn info_cmd(cmd: &Command, name: &str) {
    info!("Running {} on '{}' with command `{} {}`",
          name,
          cmd.get_current_dir().unwrap().to_str().unwrap(),
          cmd.get_program().to_str().unwrap(),
          cmd.get_args().map(|s| s.to_str().unwrap()).collect::<String>());
}

fn info_lines(prefix: &str, lines: Lines) {
    for l in lines {
        info!("{}{}", prefix, l);
    }
}

// Invoke proptest to compile and fuzz proptest targets
fn run_proptest(opt: &Opt, features: &[&str]) -> Status {
    let mut flags: Vec<&str> = Vec::from(features);
    if opt.verbosity > 0 {
        flags.push("-v");
    }

    /* FIXME: `cmd` is never use?
    if runtests or tests:
      cmd = 'test'
    else:
      cmd = 'run'
     */

    if opt.tests {
        flags.push("--tests");
    }

    for t in &opt.test {
        flags.push("--test");
        flags.push(&t);
    }

    if opt.replay > 0 {
        assert!(opt.args.is_empty());
        flags.push("--");
        flags.push("--nocapture");
    } else if ! opt.args.is_empty() {
        flags.push("--");
        flags.extend_from_slice(
            // Convert opt.args from Vec<String> to Vec<&str>.
            &opt.args.iter().map(AsRef::as_ref).collect::<Vec<&str>>()
        );
    }

    let mut cmd = Command::new("cargo");
    cmd.arg("test")
        .args(flags)
        .current_dir(&opt.crate_path);

    info_cmd(&cmd, "Proptest");

    let output = cmd.output().expect("Failed to execute `cargo`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDERR: ", stderr.lines());
        info_lines("STDOUT: ", stdout.lines());

        for l in stderr.lines() {
            if l.contains("with overflow") {
                return Status::Overflow
            }
        }
        Status::Error
    } else {
        Status::Verified
    }
}

fn get_default_host(crate_path: &PathBuf) -> String {
    let output = Command::new("rustup")
        .arg("show")
        .current_dir(crate_path)
        .output()
        .expect("Failed to execute `rustup show`");

    if ! output.status.success() {
        error!("`rustup show` terminated unsuccessfully");
        exit(1);
    }

    from_utf8(&output.stdout).unwrap().lines().find_map(|l| {
        l.strip_prefix("Default host:").and_then(|l| Some(l.trim()))
    }).expect("Unable to determine default host").to_string()
}

fn compile(opt: &Opt, package: &str, features: &[&str], target: &str) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
    let rustflags = vec![
        "-Clto",                 // Generate linked bitcode for entire crate
        "-Cembed-bitcode=yes",
        "--emit=llvm-bc",

        "-Copt-level=1",         // Avoid generating SSE instructions
                                 // Any value except 0 seems to work

        "--cfg=verify",          // Select verification versions of libraries

        // "-Ccodegen-units=1",     // Optimize a bit more?

        "-Zpanic_abort_tests",   // Panic abort is simpler
        "-Cpanic=abort",

        "-Warithmetic-overflow", // Detecting errors is good!
        "-Coverflow-checks=yes",

        "-Cno-vectorize-loops",  // KLEE does not support vector intrinisics
        "-Cno-vectorize-slp",
        "-Ctarget-feature=-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-3dnow,-3dnowa,-avx,-avx2",

        // use clang to link with LTO - to handle calls to C libraries
        "-Clinker-plugin-lto",
        "-Clinker=clang-10",
        "-Clink-arg=-fuse-ld=lld",
    ].join(" ");

    let rustflags =
        match std::env::var_os("RUSTFLAGS") {
            Some(mut env_rustflags) => {
                env_rustflags.push(" ");
                env_rustflags.push(rustflags);
                env_rustflags
            }
            None => OsString::from(rustflags)
        };

    // Find the target directory
    // (This may not be inside the crate if using workspaces)
    let target_dir = get_meta_target_directory(&opt.crate_path);

    let mut flags = vec![];

    flags.extend_from_slice(features);

    if opt.verbosity > 0 {
        flags.push("-v");
    }

    if opt.tests {
        flags.push("--tests");
    }

    // The following line is not present because we care about the target It is
    // there to allow us to use -Clto to build crates whose dependencies invoke
    // proc_macros.
    // FIXME: "=="?
    let target_flag = format!("--target=={}", target);
    flags.push(&target_flag);

    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .args(flags)
        .current_dir(&opt.crate_path)
        .env("RUSTFLAGS", &rustflags)
        // .env("PATH", ...)
        .env("CRATE_CC_NO_DEFAULTS", "true")
        .env("CFLAGS", "-flto=thin")
        .env("CC", "clang-10");

    info_cmd(&cmd, "cargo");
    info!("RUSTFLAGS='{}'", rustflags.to_str().unwrap());

    let output = cmd.output().expect("Failed to execute `cargo`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("FAILED: Couldn't compile");
        return None
    }

    let mut deps_dir = target_dir.clone();
    deps_dir.extend([target, "debug", "deps"].iter());
    // {target_dir}/{target}/debug/deps/{package}*.bc
    let bc_files = deps_dir.read_dir().unwrap().map(|e| e.unwrap().path()).filter(|p| {
        p.file_name().and_then(OsStr::to_str).map(|f| f.starts_with(package)).unwrap_or(false) &&
            p.extension() == Some(&OsString::from("bc"))
    }).collect::<Vec<_>>();

    let mut build_dir = target_dir.clone();
    build_dir.extend([target, "debug", "build"].iter());
    // {targetdir}/{target}/debug/build/ * /out/ *.o"
    let c_files = build_dir.read_dir().unwrap()
        .filter_map(Result::ok)
        .map(|d| { let mut p = d.path(); p.push("out"); p })
        .filter_map(|d| d.read_dir().ok())
        .flatten()
        .map(|f| f.unwrap().path()).filter(|p| {
            p.is_file() &&
                p.extension() == Some(&OsString::from("o"))
        }).collect::<Vec<_>>();

    // build_plan = read_build_plan(crate, flags)
    // print(json.dumps(build_plan, indent=4, sort_keys=True))
    Some((bc_files, c_files))
}

// Count how many functions in fs are present in bitcode file
fn count_symbols(bcfile: &PathBuf, fs: &[&str]) -> usize {
    info!("    Counting symbols {:?} in {:?}", fs, bcfile);

    let mut cmd = Command::new("llvm-nm");
    cmd.arg("--defined-only")
        .arg(bcfile);
        // .current_dir(&opt.crate_path)

    info_cmd(&cmd, "llvm-nm");

    let output = cmd.output().expect("Failed to execute `cargo`");

    let stdout = from_utf8(&output.stdout).unwrap();
    // let stderr = from_utf8(&output.stderr).unwrap();

    // TODO:
    // if ! output.status.success() {

    let count = stdout.lines()
        .map(|l| l.split(" ").collect::<Vec<_>>())
        .filter(|l| l.len() == 3 && l[1] == "T" && fs.iter().any(|f| f == &l[2]))
        .count();

    info!("    Found {} functions", count);
    count
}

// Link multiple bitcode files together.
fn link(crate_path: &PathBuf, out_file: &PathBuf, in_files: &[PathBuf]) -> bool {
    let mut cmd = Command::new("llvm-link");
    cmd.arg("-o").arg(out_file)
        .args(in_files)
        .current_dir(&crate_path);

    info_cmd(&cmd, "llvm-link");
    let output = cmd.output().expect("Failed to execute `llvm-link`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("FAILED: Couldn't link");
        false
    } else {
        true
    }
}

// Patch LLVM file to enable verification
//
// While this varies a bit according to the backend, some of the patching
// performed includes
//
// - arranging for initializers to be executed
//   (this makes std::env::args() work)
// - redirecting panic! to invoke backend-specific intrinsic functions
//   for reporting errors
fn patch_llvm(options: &[&str], bcfile: &PathBuf, new_bcfile: &PathBuf) -> bool {
    let mut cmd = Command::new("rvt-patch-llvm");
    cmd.arg(bcfile)
        .arg("-o").arg(new_bcfile)
        .args(options);
        // .current_dir(&crate_path)

    info_cmd(&cmd, "rvt-patch-llvm");
    let output = cmd.output().expect("Failed to execute `rvt-patch-llvm`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("FAILED: Couldn't run rvt-patch-llvm");
        false
    } else {
        true
    }
}

// Generate a list of tests in the crate
// by parsing the output of "cargo test -- --list"
fn list_tests(crate_path: &PathBuf, features: &[&str]) -> Vec<String> {
    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(mut env_rustflags) => {
            env_rustflags.push(" --cfg=verify");
            env_rustflags
        }
        None => OsString::from("--cfg=verify")
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("test")
        .args(features)
        .args(["--", "--list"].iter())
        // .arg("--exclude-should-panic")
        .current_dir(&crate_path)
        .env("RUSTFLAGS", rustflags);
        // .env("PATH", ...)

    info_cmd(&cmd, "rvt-patch-llvm");
    let output = cmd.output().expect("Failed to execute `rvt-patch-llvm`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if false && ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("Couldn't get list of tests");
        exit(1)
    }

    lazy_static! {
        static ref TEST: Regex = Regex::new(r"(\S+):\s+test\s*$").unwrap();
    }
    stdout.lines().filter_map(|l| {
        TEST.captures(l).map(|caps| caps.get(1).unwrap().as_str().into())
    }).collect()
}

// Find a function defined in LLVM bitcode file
//
// This amounts to mangling the function names but is
// more complicated because we don't have the hash value in our hand
fn mangle_functions(bcfile: &PathBuf, names: &[&str]) -> Vec<(String, String)> {
    info!("    Looking up {:?} in {:?}", names, bcfile);

    // apply rustc-style name mangling
    // let names: HashMap<String, String> = names.iter()
    //     .map(|name| {
    //         let mangled = name.iter().map(|s| format!("{}{}", s.len(), s)).collect::<Vec<_>>().join("");
    //         (mangled, name.join("::"))
    //     }).collect();
    let names: HashSet<&str> = HashSet::from_iter(names.iter().cloned());

    let mut cmd = Command::new("llvm-nm");
    cmd.arg("--defined-only")
        .arg(bcfile);
        // .current_dir(&crate_path)

    info_cmd(&cmd, "llvm-nm");
    let output = cmd.output().expect("Failed to execute `llvm-nm`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("FAILED: Couldn't run llvm-nm");
        exit(1)
    }

    let rs: Vec<(String, String)> = stdout.lines()
        .map(|l| l.split(" ").collect::<Vec<&str>>())
        .filter_map(|l| {
            if l.len() == 3 &&
                l[1].to_lowercase() == "t" &&
                (l[2].starts_with("__ZN") || l[2].starts_with("_ZN"))
            {
                let mangled = l[2];
                let (_prefix, suffix) =
                    if l[2].starts_with("__ZN") {
                        // on OSX, llvm-nm shows a double underscore prefix
                        (&mangled[1..4], &mangled[4..])
                    } else {
                        (&mangled[0..3], &mangled[3..])
                    };
                let dname = format!("{:#}", demangle(suffix));
                if names.contains(dname.as_str()) {
                    Some((dname, String::from("_ZN") + suffix))
                } else {
                    None
                }
            } else {
                None
            }
        }).collect();

    info!("      Found {:?}", rs);

    // TODO: this doesn't look right:
    // missing = set(paths) - paths.keys()
    let missing = names.len() - rs.len();
    if missing > 0 {
        error!("Unable to find {} tests in bytecode file", missing);
        exit(1)
    }

    rs
}

// Replay a KLEE "ktest" file
fn replay_klee(opt: &Opt, name: &str, ktest: &PathBuf, features: &[&str]) {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&opt.crate_path);

    if opt.tests || ! opt.test.is_empty() {
        cmd.arg("test")
            .args(features)
            .arg(&name)
            .args(["--", "--nocapture"].iter());
    } else {
        cmd.arg("run").args(features);
        if ! opt.args.is_empty() {
            cmd.arg("--").args(opt.args.iter());
        }
    }

    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(mut env_rustflags) => {
            env_rustflags.push(" --cfg=verify");
            env_rustflags
        }
        None => OsString::from("--cfg=verify")
    };
    cmd.env("RUSTFLAGS", rustflags)
        .env("KTEST_FILE", ktest);


    info_cmd(&cmd, "Replay");
    let output = cmd.output().expect("Failed to execute `cargo`");

    let stdout = from_utf8(&output.stdout).unwrap();
    let stderr = from_utf8(&output.stderr).unwrap();

    if ! output.status.success() {
        info_lines("STDOUT: ", stdout.lines());
        info_lines("STDERR: ", stderr.lines());
        error!("FAILED: Couldn't run llvm-nm");
        exit(1)
    }
}

// encoding_rs (https://docs.rs/encoding_rs/), seems to be the standard crate
// for encoding/decoding, has this to say about ISO-8859-1: "ISO-8859-1 does not
// exist as a distinct encoding from windows-1252 in the Encoding
// Standard. Therefore, an encoding that maps the unsigned byte value to the
// same Unicode scalar value is not available via Encoding in this crate."
// The following is from https://stackoverflow.com/a/28175593
fn from_latin1(s: &[u8]) -> String {
    s.iter().map(|&c| c as char).collect()
}

// Detect lines that match #[should_panic(expected = ...)] string
fn is_expected_panic(line: &str, expect: &Option<&str>, name: &str) -> bool {
    lazy_static! {
        static ref PANOCKED: Regex = Regex::new(r" panicked at '([^']*)',\s+(.*)").unwrap();
    }

    if let Some(expect) = expect {
        if let Some(caps) = PANOCKED.captures(line) {
            let message = caps.get(1).unwrap().as_str();
            let srcloc = caps.get(2).unwrap().as_str();
            if message.contains(expect) {
                info!("     {}: Detected expected failure '{}' at {}", name, message, srcloc);
                info!("     Error message: {}", line);
                return true
            }
        }
    }

    false
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
fn klee_importance(line: &str, expect: &Option<&str>, name: &str) -> i8 {
    if line.starts_with("VERIFIER_EXPECT:") {
        4
    } else if is_expected_panic(&line, &expect, &name) {
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

fn klee_run(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, kleedir: &PathBuf) -> (Status,HashMap<String, isize>) {
    let args = vec![
        "--exit-on-error",
        "--entry-point", entry,
        // "--posix-runtime",
        // "--libcxx",
        "--libc=klee",
        "--silent-klee-assume",
        "--output-dir", kleedir.to_str().unwrap(),
        "--disable-verify", // workaround https://github.com/klee/klee/issues/937
    ];

    let opt_args =
        match &opt.backend_flags {
            // FIXME: I'm assuming multiple flags are comma separated?
            // Make sure this is also the case when using the cli arg multiple times.
            Some(opt_flags) => opt_flags.split(',').collect::<Vec<&str>>(),
            None => vec![],
        };

    let args = [args, opt_args].concat();

    let mut cmd = Command::new("klee");
    cmd.args(&args)
        .arg(bcfile.to_str().unwrap())
        .args(&opt.args)
        .current_dir(&opt.crate_path);

    info_cmd(&cmd, "KLEE");

    let output = cmd.output().expect("Failed to execute `klee`");

    if ! output.status.success() {
        error!("`klee` terminated unsuccessfully");
        exit(1);
    }

    let stdout = from_latin1(&output.stdout);
    let stderr = from_latin1(&output.stderr);

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
        } else if let Some(e) = l.strip_prefix("VERIFIER_EXPECT: should_panic(expected = \"")
            .and_then(|l| l.strip_suffix("\")")) {
            info!("Expecting '{}'", e);
            expect = Some(e);
        }
    }

    // Scan for first message that indicates result
    let status = stderr.lines().find_map(|l| {
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
        } else if l.starts_with("VERIFIER_EXPECT:") { // don't confuse this line with an error!
            None
        } else if is_expected_panic(&l, &expect, &name) {
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
            if expect == None {
                Some(Status::Verified)
            } else {
                Some(Status::Error)
            }
        } else {
            None
        }
    }).unwrap_or_else(|| {
        info!("Unable to determine status of {}", name);
        Status::Unknown
    });

    info!("Status: '{}' expected: '{:?}'", status, expect);

    // Scan for statistics
    lazy_static! {
        static ref KLEE_DONE: Regex = Regex::new(r"^KLEE: done:\s+(.*)= (\d+)").unwrap();
    }

    let stats: HashMap<String, isize> = stderr.lines()
        // .filter(|l| l.starts_with("KLEE: done:"))
        .filter_map(|l| {
            KLEE_DONE.captures(l).map(|caps| {
                (caps.get(1).unwrap().as_str().trim().to_string(),
                 caps.get(1).unwrap().as_str().parse::<isize>().unwrap())
            })
        }).collect();

    info_lines("STDOUT: ", stdout.lines());

    for l in stderr.lines() {
        if klee_importance(&l, &expect, &name) < opt.verbosity as i8 {
            println!("{}", l);
        }
    }

    (status, stats)
}

fn klee_verify(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, features: &[&str]) -> Status {
    let mut kleedir = opt.crate_path.clone();
    kleedir.push(&format!("kleeout-{}", name));

    // Ignoring result. We don't care if it fails because the path doesn't
    // exist.
    remove_dir_all(&kleedir).unwrap_or_default();

    if kleedir.exists() {
        error!("Directory or file '{}' already exists, and can't be removed", kleedir.to_str().unwrap());
        return Status::Unknown
    }

    info!("     Running KLEE to verify {}", name);
    info!("      file: {}", bcfile.to_str().unwrap());
    info!("      entry: {}", entry);
    info!("      results: {}", kleedir.to_str().unwrap());

    let (status, stats) = klee_run(&opt, &name, &entry, &bcfile, &kleedir);
    if ! stats.is_empty() {
        log!(log::Level::Warn, "     {}: {} paths", name, stats.get("completed paths").unwrap());
        info!("     {}: {:?}", name, stats);
    }

    lazy_static! {
        static ref TEST_ERR: Regex = Regex::new(r"test.*\.err$").unwrap();
        static ref TEST_KTEST: Regex = Regex::new(r"test.*\.ktest$").unwrap();
    }

    // {kleedir}/test*.err
    let mut failures = kleedir.read_dir().unwrap().map(|e| e.unwrap().path()).filter(|p| {
        p.is_file() &&
            TEST_ERR.is_match(p.file_name().unwrap().to_str().unwrap())
            // p.file_name().unwrap().to_string_lossy().starts_with("test") &&
            // p.extension() == Some(&OsString::from("err"))
    }).collect::<Vec<_>>();
    failures.sort_unstable();
    info!("      Failing test: {:?}", failures);

    if opt.replay > 0 {
        // use -r -r to see all tests, not just failing tests
        let mut ktests =
            if opt.replay > 1 {
                // {kleedir}/test*.ktest
                kleedir.read_dir().unwrap().map(|e| e.unwrap().path()).filter(|p| {
                    p.is_file() &&
                        TEST_KTEST.is_match(p.file_name().unwrap().to_str().unwrap())
                        // p.file_name().unwrap().to_string_lossy().starts_with("test") &&
                        // p.extension() == Some(&OsString::from("ktest"))
                }).collect::<Vec<_>>()
            } else {
                // Remove the '.err' extension and replace the '.*' ('.abort' or
                // '.ptr') with '.ktest'.
                failures.iter().map(|p| {
                    p.with_extension("").with_extension("ktest")
                }).collect::<Vec<_>>()
            };
        ktests.sort_unstable();

        for ktest in ktests {
            println!("    Test input {}", ktest.to_str().unwrap());
            replay_klee(&opt, &name, &ktest, &features);
        }
    }

    status
}

fn seahorn_verify(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, features: &[&str]) -> Status {
    todo!()
}

fn verifier_run(opt: &Opt, name: &str, entry: &str, bcfile: &PathBuf, features: &[&str]) -> Status {
    match opt.backend {
        Backend::Klee => klee_verify(&opt, &name, &entry, &bcfile, &features),
        Backend::Seahorn => seahorn_verify(&opt, &name, &entry, &bcfile, &features),
        Backend::Proptest => unreachable!(),
    }
}

fn verify(opt: &Opt, package: &str, features: &[&str], target: &str) -> Status {
    // Compile and link the patched file using LTO to generate the entire
    // application in a single LLVM file
    info!("  Compiling {}", package);

    let (bcfiles, c_files) =
        match compile(opt, package, features, target) {
            Some(files) => files,
            None => return Status::Unknown,
        };

    let bcs: Vec<PathBuf> = bcfiles.into_iter().filter(|bc| -> bool {
        count_symbols(&bc, &["main", "_main"]) > 0
    }).collect();

    let rust_file =
        match bcs.as_slice() {
            [bc] => bc.clone(),
            [] => {
                if opt.tests || ! opt.test.is_empty() {
                    error!("  FAILED: Use --tests with library crates");
                } else {
                    error!("  FAILED: Test {} compilation error", &package);
                }
                return Status::Unknown
            }
            _ => {
                error!("  FAILED: Test {} compilation error", &package);
                info!("    Ambiguous bitcode files {:?}", &bcs);
                return Status::Unknown
            }
        };

    let mut bcfile =
        if ! c_files.is_empty() {
            // Link bc file (from all the Rust code) against the c_files from
            // any C/C++ code generated by build scripts

            let bcfile = PathBuf::from("linked.bc");
            if ! link(&opt.crate_path, &bcfile, &[vec![rust_file.clone()], c_files].concat()) {
                return Status::Unknown;
            }
            bcfile
        } else {
            rust_file.clone()
        };

    if opt.backend == Backend::Seahorn {
        info!("  Patching LLVM file for Seahorn");
        let mut ext = OsString::from("patch.");
        ext.push(bcfile.extension().unwrap());
        let mut new_bcfile = bcfile.clone();
        new_bcfile.set_extension(&ext);
        if ! patch_llvm(&["--seahorn"], &bcfile, &new_bcfile) {
            return Status::Unknown;
        }
        bcfile = new_bcfile;
    }

    let tests = {
        // If using the --tests flag, generate a list of tests and their mangled names
        if opt.tests || ! opt.test.is_empty() {
            // get a list of the tests
            info!("  Getting list of tests in {}", package);
            let mut tests = list_tests(&opt.crate_path, &features);
            if ! opt.test.is_empty() {
                tests = tests.into_iter().filter(|t| {
                    opt.test.iter().any(|f| t.contains(f))
                }).collect();
            }
            if tests.is_empty() {
                error!("No tests found");
                return Status::Unknown
            }

            info!("  Checking {:?}", tests);

            // then look up their mangled names in the bcfile
            mangle_functions(&rust_file, &tests.iter().map(AsRef::as_ref).collect::<Vec<&str>>())
                // &tests.iter().map(|t| { t.split("::").collect::<Vec<_>>() }).collect::<Vec<_>>()
        } else if opt.backend == Backend::Seahorn {
            // Find the entry function (mangled main)
            let mains = mangle_functions(&rust_file, &[&(String::from(package) + "::main")]);
            match mains.as_slice() {
                [] => {
                    error!("  FAILED: can't find the 'main' function");
                    return Status::Unknown
                }
                [(_, _)] => {
                    vec![("main".to_string(), (mains as Vec<(_, String)>).remove(0).1)]
                }
                _ => {
                    error!("  FAILED: found more than one 'main' function");
                    return Status::Unknown
                }
            }
        } else {
            vec![("main".to_string(), "main".to_string())]
        }
    };
    info!("  Mangled: {:?}", tests);

    if ! opt.args.is_empty() {
        info!("  Patching LLVM file for initializers");
        let new_bcfile = PathBuf::from("linked.bc"); // FIXME: use a proper name
        if ! patch_llvm(&["--initializers"], &bcfile, &new_bcfile) {
            return Status::Unknown
        }
        bcfile = new_bcfile;
    }

    // For each test function, we run the backend and sift through its
    // output to generate an appropriate status string.
    info!("Running {} test(s)", tests.len());

    let mut passes = 0;
    let mut fails = 0;
    let mut failure = None;

    // TODO: use thread-pool to run the tests.
    for (name, entry) in tests {
        let status = verifier_run(&opt, &name, &entry, &bcfile, &features);

        if status == Status::Verified {
            println!("test {} ... ok", name);
            passes += 1;
        } else {
            println!("test {} ... {:?}", name, status);
            fails += 1;
            failure = Some(status);
        }
    }

    let (msg, status) =
        match failure {
            Some(failure) => {
                // randomly pick one failing message
                (failure.to_string(), failure)
            }
            None => ("ok".to_string(), Status::Verified),
        };

    println!("test result: {}. {} passed; {} failed", msg, passes, fails);
    status
}
