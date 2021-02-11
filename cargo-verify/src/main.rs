// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(command_access)]

use cargo_metadata::MetadataCommand;
use lazy_static::lazy_static;
use log::{error, info};
use regex::Regex;
use rustc_demangle::demangle;
use utils::{Append, add_pre_ext};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{
    collections::HashSet,
    error,
    ffi::{OsStr, OsString},
    fmt,
    iter,
    process::exit,
    str::from_utf8,
};
use structopt::{clap::arg_enum, StructOpt};

mod backends_common;
mod klee;
mod seahorn;
mod utils;

// Command line argument parsing
#[derive(StructOpt)]
#[structopt(
    name = "cargo-verify",
    about = "Execute verification tools",
    // version number is taken automatically from Cargo.toml
)]
pub struct Opt {
    // TODO: make this more like 'cargo test --manifest-path <PATH>'
    // i.e., path to Cargo.toml
    /// Filesystem path to local crate to verify
    #[structopt(long = "path", name = "PATH", parse(from_os_str), default_value = ".")]
    crate_path: PathBuf,

    /// Arguments to pass to program under test
    #[structopt(name = "ARG", last = true)]
    args: Vec<String>,

    /// Select verification backend
    #[structopt(
        short,
        long,
        name = "BACKEND",
        possible_values = &Backend::variants(),
        default_value = "Klee", // FIXME: is that a sensible choice?
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

    // TODO: make this more like 'cargo test [TESTNAME]'
    /// Only verify tests containing this string in their names
    #[structopt(long, number_of_values = 1, name = "TESTNAME")]
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

arg_enum! {
    #[derive(Debug, PartialEq)]
    enum Backend {
        Proptest,
        Klee,
        Seahorn,
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum Status {
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
            Status::Unknown => write!(f, "Unknown"),
            Status::Verified => write!(f, "Verified"),
            Status::Error => write!(f, "Error"),
            Status::Timeout => write!(f, "Timeout"),
            Status::Overflow => write!(f, "Overflow"),
            Status::Reachable => write!(f, "Reachable"),
        }
    }
}

type CVResult<T> = Result<T, Box<dyn error::Error>>;

fn main() -> CVResult<()> {
    let opt = Opt::from_args();

    #[rustfmt::skip]
    stderrlog::new()
        .verbosity(opt.verbosity)
        .init()?;

    if opt.backend == Backend::Proptest {
        if opt.replay > 0 && !opt.args.is_empty() {
            error!(
                "The Proptest backend does not support '--replay' and passing arguments together."
            );
            exit(1);
        }
    }

    if opt.backend == Backend::Seahorn {
        if !opt.args.is_empty() {
            error!("The Seahorn backend does not support passing arguments yet.");
            exit(1);
        }
        if opt.replay != 0 {
            error!("The Seahorn backend does not support '--replay' yet.");
            exit(1);
        }
    }

    let features = match opt.backend {
        Backend::Klee => vec!["verifier-klee"],
        Backend::Proptest => vec!["verifier-seahorn"],
        Backend::Seahorn => vec!["verifier-seahorn"],
    };

    if opt.clean {
        info!("Running `cargo clean`");
        Command::new("cargo")
            .arg("clean")
            .current_dir(&opt.crate_path)
            .output()
            .ok(); // Discarding the error on purpose.
    }

    let package = get_meta_package_name(&opt.crate_path)?;
    info!("Checking {}", &package);

    let status = match opt.backend {
        Backend::Proptest => {
            info!("  Invoking cargo run with proptest backend");
            run_proptest(&opt, &features)
        }
        _ => {
            let target = get_default_host(&opt.crate_path)?;
            info!("target: {}", target);
            verify(&opt, &package, &features, &target)?
        }
    };

    println!("VERIFICATION_RESULT: {}", status);

    if status != Status::Verified {
        exit(1);
    }

    Ok(())
}

fn get_meta_package_name(crate_dir: &Path) -> CVResult<String> {
    let mut cmd = MetadataCommand::new();
    cmd.manifest_path(
        crate_dir
            .iter()
            .chain(iter::once(OsStr::new("Cargo.toml")))
            .collect::<PathBuf>(),
    );
    // .features(CargoOpt::AllFeatures)

    let name = cmd
        .exec()?
        .root_package()
        .ok_or("no root package")?
        .name
        .replace(
            |c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => false,
                _ => true,
            },
            "_",
        );

    Ok(name)
}

fn get_meta_target_directory(crate_dir: &Path) -> CVResult<PathBuf> {
    // FIXME: add '--cfg=verify' to RUSTFLAGS, pass features to the command
    let dir = MetadataCommand::new()
        .manifest_path(
            crate_dir
                .iter()
                .chain(iter::once(OsStr::new("Cargo.toml")))
                .collect::<PathBuf>(),
        )
        // .features(CargoOpt::AllFeatures)
        .exec()?
        .target_directory;

    Ok(dir)
}

// Invoke proptest to compile and fuzz proptest targets
fn run_proptest(opt: &Opt, features: &[&str]) -> Status {
    let mut cmd = Command::new("cargo");
    cmd.arg("test")
        .args(vec!["-v"; opt.verbosity])
        .current_dir(&opt.crate_path);

    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
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
                return Status::Overflow;
            }
        }
        Status::Error
    } else {
        Status::Verified
    }
}

fn get_default_host(crate_path: &Path) -> CVResult<String> {
    let mut cmd = Command::new("rustup");
    cmd.arg("show").current_dir(crate_path);

    utils::info_cmd(&cmd, "rustup");

    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDERR: ", stderr.lines());
        utils::info_lines("STDOUT: ", stdout.lines());
        Err("`rustup show` terminated unsuccessfully")?
    }

    Ok(stdout
        .lines()
        .find_map(|l| l.strip_prefix("Default host:").and_then(|l| Some(l.trim())))
        .ok_or("Unable to determine default host")?
        .to_string())
}

fn compile(
    opt: &Opt,
    package: &str,
    features: &[&str],
    target: &str,
) -> CVResult<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut rustflags = vec![
        "-Clto", // Generate linked bitcode for entire crate
        "-Cembed-bitcode=yes",
        "--emit=llvm-bc",
        // Any value except 0 seems to work
        "--cfg=verify", // Select verification versions of libraries
        // "-Ccodegen-units=1",     // Optimize a bit more?
        "-Zpanic_abort_tests", // Panic abort is simpler
        "-Cpanic=abort",
        "-Warithmetic-overflow", // Detecting errors is good!
        "-Coverflow-checks=yes",
        "-Cno-vectorize-loops", // KLEE does not support vector intrinisics
        "-Cno-vectorize-slp",
        "-Ctarget-feature=-mmx,-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-3dnow,-3dnowa,-avx,-avx2",
        // use clang to link with LTO - to handle calls to C libraries
        "-Clinker-plugin-lto",
        "-Clinker=clang-10",
        "-Clink-arg=-fuse-ld=lld",
    ]
    .join(" ");

    if opt.backend != Backend::Seahorn {
        // Avoid generating SSE instructions
        rustflags.push_str(" -Copt-level=1");
    }

    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(env_rustflags) => env_rustflags.append(" ").append(rustflags),
        None => OsString::from(rustflags),
    };

    // Find the target directory
    // (This may not be inside the crate if using workspaces)
    let target_dir = get_meta_target_directory(&opt.crate_path)?;

    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }

    if opt.tests || !opt.test.is_empty() {
        cmd.arg("--tests");
    }

    // The following line is not present because we care about the target It is
    // there to allow us to use -Clto to build crates whose dependencies invoke
    // proc_macros.
    // FIXME: "=="?
    cmd.arg(format!("--target=={}", target))
        .args(vec!["-v"; opt.verbosity])
        .current_dir(&opt.crate_path)
        .env("RUSTFLAGS", &rustflags)
        // .env("PATH", ...)
        .env("CRATE_CC_NO_DEFAULTS", "true")
        .env("CFLAGS", "-flto=thin")
        .env("CC", "clang-10");

    utils::info_cmd(&cmd, "cargo");
    info!("RUSTFLAGS='{}'", rustflags.to_str().ok_or("not UTF-8")?);

    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't compile")?
    }

    let mut deps_dir = PathBuf::from(&target_dir);
    deps_dir.extend([target, "debug", "deps"].iter());
    // {target_dir}/{target}/debug/deps/{package}*.bc
    let bc_files = deps_dir
        .read_dir()?
        .filter_map(Result::ok)
        .map(|d| d.path())
        .filter(|p| {
            p.file_name()
                .and_then(OsStr::to_str)
                .map(|f| f.starts_with(package))
                .unwrap_or(false)
                && p.extension() == Some(&OsString::from("bc"))
        })
        .collect::<Vec<_>>();

    let mut build_dir = PathBuf::from(&target_dir);
    build_dir.extend([target, "debug", "build"].iter());
    // {targetdir}/{target}/debug/build/ * /out/ *.o"
    let c_files = build_dir
        .read_dir()?
        .filter_map(Result::ok)
        .map(|d| d.path().append("out"))
        .filter_map(|d| d.read_dir().ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|d| d.path())
        .filter(|p| p.is_file() && p.extension() == Some(&OsString::from("o")))
        .collect::<Vec<_>>();

    // build_plan = read_build_plan(crate, flags)
    // print(json.dumps(build_plan, indent=4, sort_keys=True))
    Ok((bc_files, c_files))
}

// Count how many functions in fs are present in bitcode file
fn count_symbols(bcfile: &Path, fs: &[&str]) -> usize {
    info!("    Counting symbols {:?} in {:?}", fs, bcfile);

    let mut cmd = Command::new("llvm-nm");
    cmd.arg("--defined-only").arg(bcfile);
    // .current_dir(&opt.crate_path)

    utils::info_cmd(&cmd, "llvm-nm");

    let output = cmd.output().expect("Failed to execute `llvm-nm`");

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    // let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    // TODO:
    // if ! output.status.success() {

    let count = stdout
        .lines()
        .map(|l| l.split(" ").collect::<Vec<_>>())
        .filter(|l| l.len() == 3 && l[1] == "T" && fs.iter().any(|f| f == &l[2]))
        .count();

    info!("    Found {} functions", count);
    count
}

// Link multiple bitcode files together.
fn link(crate_path: &Path, out_file: &Path, in_files: &[PathBuf]) -> CVResult<()> {
    let mut cmd = Command::new("llvm-link");
    cmd.arg("-o")
        .arg(out_file)
        .args(in_files)
        .current_dir(&crate_path);

    utils::info_cmd(&cmd, "llvm-link");
    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't link")?
    }

    Ok(())
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
fn patch_llvm(options: &[&str], bcfile: &Path, new_bcfile: &Path) -> CVResult<()> {
    let mut cmd = Command::new("rvt-patch-llvm");
    cmd.arg(bcfile)
        .arg("-o").arg(new_bcfile)
        .args(options);
        // .current_dir(&crate_path)

    utils::info_cmd(&cmd, "rvt-patch-llvm");
    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't run rvt-patch-llvm")?
    }

    Ok(())
}

// Generate a list of tests in the crate
// by parsing the output of "cargo test -- --list"
fn list_tests(crate_path: &Path, features: &[&str]) -> CVResult<Vec<String>> {
    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(env_rustflags) => env_rustflags.append(" --cfg=verify"),
        None => OsString::from("--cfg=verify"),
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("test");

    if !features.is_empty() {
        cmd.arg("--features").arg(features.join(","));
    }

    cmd.args(&["--", "--list"])
        // .arg("--exclude-should-panic")
        .current_dir(&crate_path)
        .env("RUSTFLAGS", rustflags);
        // .env("PATH", ...)

    utils::info_cmd(&cmd, "cargo");
    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if false && !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("Couldn't get list of tests")?;
    }

    lazy_static! {
        static ref TEST: Regex = Regex::new(r"(\S+):\s+test\s*$").unwrap();
    }

    let tests = stdout.lines()
        .filter_map(|l| {
            TEST.captures(l)
                .map(|caps| caps.get(1).unwrap().as_str().into())
        })
        .collect();

    Ok(tests)
}

// Find a function defined in LLVM bitcode file
//
// This amounts to mangling the function names but is
// more complicated because we don't have the hash value in our hand
fn mangle_functions<T: AsRef<str>>(bcfile: &Path, names: &[T]) -> CVResult<Vec<(String, String)>> {
    let names: HashSet<&str> = names.iter().map(AsRef::as_ref).collect();

    info!("    Looking up {:?} in {:?}", names, bcfile);

    let mut cmd = Command::new("llvm-nm");
    cmd.arg("--defined-only").arg(bcfile);
    // .current_dir(&crate_path)

    utils::info_cmd(&cmd, "llvm-nm");
    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't run llvm-nm")?
    }

    let rs: Vec<(String, String)> = stdout
        .lines()
        .map(|l| l.split(" ").collect::<Vec<&str>>())
        .filter_map(|l| {
            if l.len() == 3
                && l[1].to_lowercase() == "t"
                && (l[2].starts_with("__ZN") || l[2].starts_with("_ZN"))
            {
                let mangled = if l[2].starts_with("__ZN") {
                    // on OSX, llvm-nm shows a double underscore prefix
                    &l[2][1..]
                } else {
                    &l[2]
                };
                let mut dname = format!("{:#}", demangle(mangled));
                if let Some(i) = dname.find("::") {
                    dname = dname[i + 2..].to_string();
                }
                if names.contains(dname.as_str()) {
                    Some((dname, mangled.into()))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    info!("      Found {:?}", rs);

    // TODO: this doesn't look right:
    // missing = set(paths) - paths.keys()
    let missing = names.len() - rs.len();
    if missing > 0 {
        Err(format!("Unable to find {} tests in bytecode file", missing))?
    }
    Ok(rs)
}

fn verifier_run(opt: &Opt, name: &str, entry: &str, bcfile: &Path, features: &[&str]) -> CVResult<Status> {
    match opt.backend {
        Backend::Klee => klee::verify(&opt, &name, &entry, &bcfile, &features),
        Backend::Seahorn => seahorn::verify(&opt, &name, &entry, &bcfile, &features),
        Backend::Proptest => unreachable!(),
    }
}

fn verify(opt: &Opt, package: &str, features: &[&str], target: &str) -> CVResult<Status> {
    // Compile and link the patched file using LTO to generate the entire
    // application in a single LLVM file
    info!("  Compiling {}", package);

    let (bcfiles, c_files) = compile(opt, package, features, target)?;

    let bcfiles: Vec<PathBuf> = bcfiles
        .into_iter()
        .filter(|bc| count_symbols(&bc, &["main", "_main"]) > 0)
        .collect();

    let mut bcfile: PathBuf = match bcfiles.as_slice() {
        [_] => {
            // Move element 0 out of the Vec (and into `bcfile`).
            (bcfiles as Vec<_>).remove(0)
        }
        [] => {
            if opt.tests || !opt.test.is_empty() {
                Err("  FAILED: Use --tests with library crates")?
            } else {
                Err(format!("  FAILED: Test {} compilation error", &package))?
            }
        }
        _ => {
            info!("    Ambiguous bitcode files {:?}", &bcfiles);
            Err(format!("  FAILED: Test {} compilation error", &package))?
        }
    };

    let tests = if opt.tests || !opt.test.is_empty() {
        // If using the --tests flag, generate a list of tests and their mangled names
        info!("  Getting list of tests in {}", &package);
        let mut tests = list_tests(&opt.crate_path, &features)?;
        if !opt.test.is_empty() {
            tests = tests
                .into_iter()
                .filter(|t| opt.test.iter().any(|f| t.contains(f)))
                .collect();
        }
        if tests.is_empty() {
            Err("No tests found")?
        }
        // let tests: Vec<String> = tests.iter().map(|t| format!("{}::{}", package, t)).collect();

        info!("  Checking {:?}", tests);

        // then look up their mangled names in the bcfile
        mangle_functions(&bcfile, &tests)?
    } else if opt.backend == Backend::Seahorn {
        // Find the entry function (mangled main)
        let mains = mangle_functions(&bcfile, &["main"])?;
        match mains.as_slice() {
            [(_, _)] => mains,
            [] => Err("  FAILED: can't find the 'main' function")?,
            _ => Err("  FAILED: found more than one 'main' function")?,
        }
    } else {
        vec![("main".to_string(), "main".to_string())]
    };
    info!("  Mangled: {:?}", tests);

    if !c_files.is_empty() {
        // Link bc file (from all the Rust code) against the c_files from
        // any C/C++ code generated by build scripts
        info!("  Linking with c files.");
        let new_bcfile = add_pre_ext(&bcfile, "link");
        link(&opt.crate_path, &new_bcfile, &[vec![bcfile], c_files].concat())?;
        bcfile = new_bcfile;
    }

    if opt.backend == Backend::Seahorn {
        info!("  Patching LLVM file for Seahorn");
        let new_bcfile = add_pre_ext(&bcfile, "patch");
        patch_llvm(&["--seahorn"], &bcfile, &new_bcfile)?;
        bcfile = new_bcfile;
    }

    if !opt.args.is_empty() {
        info!("  Patching LLVM file for initializers");
        let new_bcfile = add_pre_ext(&bcfile, "init");
        patch_llvm(&["--initializers"], &bcfile, &new_bcfile)?;
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
        let status = match verifier_run(&opt, &name, &entry, &bcfile, &features) {
            Ok(status) => status,
            Err(error) => {
                error!("{}", error);
                error!("Failed to run test '{}'.", name);
                Status::Unknown
            }
        };

        if status == Status::Verified {
            println!("test {} ... ok", name);
            passes += 1;
        } else {
            println!("test {} ... {:?}", name, status);
            fails += 1;
            failure = Some(status);
        }
    }

    let (msg, status) = match failure {
        Some(failure) => {
            // randomly pick one failing message
            (failure.to_string(), failure)
        }
        None => ("ok".to_string(), Status::Verified),
    };

    println!("test result: {}. {} passed; {} failed", msg, passes, fails);
    Ok(status)
}
