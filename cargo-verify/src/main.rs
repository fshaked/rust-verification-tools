// Copyright 2020-2021 The Propverify authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(command_access)]

use std::{
    collections::HashSet,
    error,
    ffi::{OsString},
    fmt,
    path::{Path, PathBuf},
    process::{exit, Command},
    str::from_utf8,
};

use cargo_metadata::{CargoOpt, MetadataCommand};
use lazy_static::lazy_static;
use log::error;
use rayon::prelude::*;
use regex::Regex;
use rustc_demangle::demangle;
use structopt::{clap::arg_enum, StructOpt};
use utils::{add_pre_ext, Append};

#[macro_use]
mod utils;
mod backends_common;
mod klee;
mod proptest;
mod seahorn;

// Command line arguments
#[derive(StructOpt)]
#[structopt(
    name = "cargo-verify",
    about = "Execute verification tools",
    // version number is taken automatically from Cargo.toml
)]
pub struct Opt {
    // TODO: make this more like 'cargo test --manifest-path <PATH>'
    // (i.e., path to Cargo.toml)
    /// Filesystem path to local crate to verify
    #[structopt(long = "path", name = "PATH", parse(from_os_str), default_value = ".")]
    crate_dir: PathBuf,

    /// Arguments to pass to program under test
    #[structopt(name = "ARG", last = true)]
    args: Vec<String>,

    // backend_arg is used for hold the CL option. After parsing, if the user
    // didn't specify a backend, we will auto-detect one, and hold it in the
    // `backend` field below.
    /// Select verification backend
    #[structopt(
        short = "b",
        long = "backend",
        name = "BACKEND",
        possible_values = &Backend::variants(),
        // default_value = "Klee", // FIXME: is that a sensible choice?
    )]
    backend_arg: Option<Backend>,

    // See the comment of `backend_arg` above.
    #[structopt(skip)]
    backend: Backend,

    /// Extra verification flags
    #[structopt(long)]
    backend_flags: Option<String>,

    /// Space or comma separated list of features to activate
    #[structopt(long, number_of_values = 1, name = "FEATURES")]
    features: Vec<String>,

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

    // jobs_arg is used for hold the CL option. After parsing, if the user
    // didn't specify this option, we will use num_cpus, and hold it in the
    // `jobs` field below.
    /// Number of parallel jobs, defaults to # of CPUs
    #[structopt(short = "j", long = "jobs", name = "N")]
    jobs_arg: Option<usize>,

    // See the comment of `jobs_arg` above.
    #[structopt(skip)]
    jobs: usize,

    /// Replay to display concrete input values
    #[structopt(short, long, parse(from_occurrences))]
    replay: usize,

    /// Increase message verbosity
    #[structopt(short, long, parse(from_occurrences))]
    verbosity: usize,
}

arg_enum! {
    #[derive(Debug, PartialEq, Copy, Clone)]
    enum Backend {
        Proptest,
        Klee,
        Seahorn,
    }
}

impl Default for Backend {
    fn default() -> Self {
        Backend::Proptest
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum Status {
    Unknown, // E.g. the varifier failed to execute.
    Verified,
    Error, // E.g. the varifier found a violation.
    Timeout,
    Overflow,
    Reachable,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Status::Unknown => write!(f, "Unknown"),
            Status::Verified => {
                if f.alternate() {
                    // "{:#}"
                    write!(f, "ok")
                } else {
                    write!(f, "Verified")
                }
            }
            Status::Error => write!(f, "Error"),
            Status::Timeout => write!(f, "Timeout"),
            Status::Overflow => write!(f, "Overflow"),
            Status::Reachable => write!(f, "Reachable"),
        }
    }
}

type CVResult<T> = Result<T, Box<dyn error::Error>>;

fn process_command_line() -> CVResult<Opt> {
    // cargo-verify can be called directly, or by placing it on the `PATH` and
    // calling it through `cargo` (i.e. `cargo verify ...`.
    let mut args: Vec<_> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "verify" {
        // Looks like the script was invoked by `cargo verify` - we have to
        // remove the second argument.
        args.remove(1);
    }
    let mut opt = Opt::from_iter(args.into_iter());
    // let mut opt = Opt::from_args();

    // Check if the backend that was specified on the CL is installed; if none
    // was specified, use the first one that we find.
    opt.backend = match opt.backend_arg {
        Some(Backend::Klee) => {
            if !klee::check_install() {
                Err("Klee is not installed")?;
            }
            Backend::Klee
        }
        Some(Backend::Seahorn) => {
            if !seahorn::check_install() {
                Err("Seahorn is not installed")?;
            }
            Backend::Seahorn
        }
        Some(Backend::Proptest) => {
            assert!(proptest::check_install());
            Backend::Proptest
        }
        None => {
            let backend = if klee::check_install() {
                Backend::Klee
            } else if seahorn::check_install() {
                Backend::Seahorn
            } else {
                assert!(proptest::check_install());
                Backend::Proptest
            };
            println!("Using {} as backend", backend);
            backend
        }
    };

    // Backend specific options.
    match opt.backend {
        Backend::Proptest => {
            if opt.replay > 0 && !opt.args.is_empty() {
                Err("The Proptest backend does not support '--replay' and passing arguments together.")?;
            }
        }
        Backend::Seahorn => {
            if !opt.args.is_empty() {
                Err("The Seahorn backend does not support passing arguments yet.")?;
            }
            if opt.replay != 0 {
                Err("The Seahorn backend does not support '--replay' yet.")?;
            }

            opt.features.push(String::from("verifier-seahorn"));
        }
        Backend::Klee => {
            opt.features.push(String::from("verifier-klee"));
        }
    }

    opt.jobs = opt.jobs_arg.unwrap_or(num_cpus::get());

    Ok(opt)
}

fn main() -> CVResult<()> {
    let opt = process_command_line()?;
    stderrlog::new().verbosity(opt.verbosity).init()?;

    if opt.clean {
        info_at!(&opt, 1, "Running `cargo clean`");
        Command::new("cargo")
            .arg("clean")
            .current_dir(&opt.crate_dir)
            .output()
            .ok(); // Discarding the error on purpose.
    }

    let package = get_meta_package_name(&opt)?;
    info_at!(&opt, 1, "Checking {}", &package);

    let status = match opt.backend {
        Backend::Proptest => {
            info_at!(&opt, 1, "  Invoking cargo run with proptest backend");
            proptest::run(&opt)
        }
        _ => {
            let target = get_default_host(&opt.crate_dir)?;
            info_at!(&opt, 4, "target: {}", target);
            verify(&opt, &package, &target)
        }
    }
    .unwrap_or_else(|err| {
        error!("{}", err);
        exit(1)
    });

    println!("VERIFICATION_RESULT: {}", status);
    if status != Status::Verified {
        exit(1);
    }
    Ok(())
}

// Compile a Rust crate to generate bitcode
// and run one of the LLVM verifier backends on the result.
fn verify(opt: &Opt, package: &str, target: &str) -> CVResult<Status> {
    // Compile and link the patched file using LTO to generate the entire
    // application in a single LLVM file
    info_at!(&opt, 1, "  Building {} for verificatuin", package);
    let bcfile = build(&opt, &package, &target)?;

    // Get the functions we need to verify, and their mangled names.
    let tests = if opt.tests || !opt.test.is_empty() {
        // If using the --tests or --test flags, generate a list of tests and
        // their mangled names.
        info_at!(&opt, 3, "  Getting list of tests in {}", &package);
        let mut tests = list_tests(&opt)?;
        if !opt.test.is_empty() {
            tests = tests
                .into_iter()
                .filter(|t| opt.test.iter().any(|f| t.contains(f)))
                .collect();
        }
        if tests.is_empty() {
            Err("  No tests found")?
        }
        let tests: Vec<String> = tests
            .iter()
            .map(|t| format!("{}::{}", package, t))
            .collect();

        // then look up their mangled names in the bcfile
        mangle_functions(&opt, &bcfile, &tests)?
    } else if opt.backend == Backend::Seahorn {
        // Find the entry function (mangled main)
        let mains = mangle_functions(&opt, &bcfile, &[String::from(package) + "::main"])?;
        match mains.as_slice() {
            [(_, _)] => mains,
            [] => Err("  FAILED: can't find the 'main' function")?,
            _ => Err("  FAILED: found more than one 'main' function")?,
        }
    } else {
        vec![("main".to_string(), "main".to_string())]
    };
    // Remove the package name from the function names (important for Klee?).
    let tests: Vec<_> = tests
        .into_iter()
        .map(|(name, mangled)| {
            if let Some(name) = name.strip_prefix(&format!("{}::", package)) {
                (name.to_string(), mangled)
            } else {
                (name, mangled)
            }
        })
        .collect();

    #[rustfmt::skip]
    info_at!(&opt, 1, "  Checking {}",
             tests.iter().cloned().unzip::<_, _, Vec<_>, Vec<_>>().0.join(", ")
    );
    info_at!(opt, 4, "Mangled: {:?}", tests);

    // For each test function, we run the backend and sift through its
    // output to generate an appropriate status string.
    println!("Running {} test(s)", tests.len());

    let results: Vec<Status> = if opt.jobs > 1 {
        // Run the verification in parallel.

        // `build_global` must not be called more than once!
        // This call configures the thread-pool for `par_iter` below.
        rayon::ThreadPoolBuilder::new()
            .num_threads(opt.jobs)
            .build_global()?;

        tests
            .par_iter() // <- parallelised iterator
            .map(|(name, entry)| verifier_run(&opt, &bcfile, &name, &entry))
            .collect()
    } else {
        // Same as above but without the overhead of rayon
        tests
            .iter() // <- this is the only difference
            .map(|(name, entry)| verifier_run(&opt, &bcfile, &name, &entry))
            .collect()
    };

    // Count pass/fail
    let passes = results.iter().filter(|r| **r == Status::Verified).count();
    let fails = results.len() - passes;
    // randomly pick one failing status (if any)
    let status = results
        .into_iter()
        .find(|r| *r != Status::Verified)
        .unwrap_or(Status::Verified);

    println!(
        "test result: {:#}. {} passed; {} failed",
        status, passes, fails
    );
    Ok(status)
}

fn verifier_run(opt: &Opt, bcfile: &Path, name: &str, entry: &str) -> Status {
    let status = match opt.backend {
        Backend::Klee => klee::verify(&opt, &name, &entry, &bcfile),
        Backend::Seahorn => seahorn::verify(&opt, &name, &entry, &bcfile),
        Backend::Proptest => unreachable!(),
    }
    .unwrap_or_else(|err| {
        error!("{}", err);
        error!("Failed to run test '{}'.", name);
        Status::Unknown
    });

    println!("test {} ... {:#}", name, status);
    status
}

// Compile, link and do transformations on LLVM bitcode.
fn build(opt: &Opt, package: &str, target: &str) -> CVResult<PathBuf> {
    let (mut bc_file, c_files) = compile(&opt, &package, target)?;

    // Link bc file (from all the Rust code) against the c_files from
    // any C/C++ code generated by build scripts
    if !c_files.is_empty() {
        info_at!(&opt, 1, "  Linking with c files.");
        let new_bc_file = add_pre_ext(&bc_file, "link");
        link(
            &opt.crate_dir,
            &new_bc_file,
            &[vec![bc_file], c_files].concat(),
        )?;
        bc_file = new_bc_file;
    }

    if opt.backend == Backend::Seahorn {
        info_at!(&opt, 1, "  Patching LLVM file for Seahorn");
        let new_bc_file = add_pre_ext(&bc_file, "patch");
        patch_llvm(&["--seahorn"], &bc_file, &new_bc_file)?;
        bc_file = new_bc_file;
    }

    if !opt.args.is_empty() {
        info_at!(&opt, 1, "  Patching LLVM file for initializers");
        let new_bc_file = add_pre_ext(&bc_file, "init");
        patch_llvm(&["--initializers"], &bc_file, &new_bc_file)?;
        bc_file = new_bc_file;
    }

    Ok(bc_file)
}

fn compile(opt: &Opt, package: &str, target: &str) -> CVResult<(PathBuf, Vec<PathBuf>)> {
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

    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    if !opt.features.is_empty() {
        cmd.arg("--features").arg(opt.features.join(","));
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
        .current_dir(&opt.crate_dir)
        .env("RUSTFLAGS", &rustflags)
        // .env("PATH", ...)
        .env("CRATE_CC_NO_DEFAULTS", "true")
        .env("CFLAGS", "-flto=thin")
        .env("CC", "clang-10");

    utils::info_cmd(&cmd, "cargo");
    info_at!(
        &opt,
        4,
        "RUSTFLAGS='{}'",
        rustflags.to_str().ok_or("not UTF-8")?
    );

    let output = cmd.output()?;

    let stdout = from_utf8(&output.stdout).expect("stdout is not in UTF-8");
    let stderr = from_utf8(&output.stderr).expect("stderr is not in UTF-8");

    if !output.status.success() {
        utils::info_lines("STDOUT: ", stdout.lines());
        utils::info_lines("STDERR: ", stderr.lines());
        Err("FAILED: Couldn't compile")?
    }

    // Find the target directory
    // (This may not be inside the crate if using workspaces)
    let target_dir = get_meta_target_directory(&opt)?;

    // {target_dir}/{target}/debug/deps/{package}*.bc
    let bc_files = target_dir
        .clone()
        .append(target)
        .append("debug")
        .append("deps")
        .read_dir()?
        .filter_map(Result::ok)
        .map(|d| d.path())
        .filter(|p| {
            p.file_name()
                .map(|f| f.to_string_lossy().starts_with(package))
                .unwrap_or(false)
                && p.extension() == Some(&OsString::from("bc"))
        })
        // Only files that include a main function (should be exactly one file)
        .filter(|p| count_symbols(&opt, &p, &["main", "_main"]) > 0)
        .collect::<Vec<_>>();

    // Make sure there is only one such file.
    let bc_file: PathBuf = match bc_files.as_slice() {
        [_] => {
            // Move element 0 out of the Vec (and into `bcfile`).
            (bc_files as Vec<_>).remove(0)
        }
        [] => {
            if opt.tests || !opt.test.is_empty() {
                Err("  FAILED: Use --tests with library crates")?
            } else {
                Err(format!("  FAILED: Test {} compilation error", &package))?
            }
        }
        _ => {
            error!("    Ambiguous bitcode files {}", bc_files.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join(", "));
            Err(format!("  FAILED: Test {} compilation error", &package))?
        }
    };

    // {targetdir}/{target}/debug/build/ * /out/ *.o"
    let c_files = target_dir
        .append(target)
        .append("debug")
        .append("build")
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
    Ok((bc_file, c_files))
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
    cmd.arg(bcfile).arg("-o").arg(new_bcfile).args(options);
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

fn get_meta_package_name(opt: &Opt) -> CVResult<String> {
    let name = MetadataCommand::new()
        .manifest_path(opt.crate_dir.clone().append("Cargo.toml"))
        .features(CargoOpt::SomeFeatures(opt.features.clone()))
        .exec()?
        .root_package()
        .ok_or("no root package")?
        .name
        .replace(
            |c| match c {
                // Allowed characters.
                'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => false,
                // Anything else will be replaced with the '_' character.
                _ => true,
            },
            "_",
        );

    Ok(name)
}

fn get_meta_target_directory(opt: &Opt) -> CVResult<PathBuf> {
    // FIXME: add '--cfg=verify' to RUSTFLAGS?
    let dir = MetadataCommand::new()
        .manifest_path(opt.crate_dir.clone().append("Cargo.toml"))
        .features(CargoOpt::SomeFeatures(opt.features.clone()))
        .exec()?
        .target_directory;

    Ok(dir)
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

// Count how many functions in fs are present in bitcode file
fn count_symbols(opt: &Opt, bcfile: &Path, fs: &[&str]) -> usize {
    info_at!(&opt, 4, "    Counting symbols {:?} in {}", fs, bcfile.to_string_lossy());

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

    info_at!(&opt, 4, "    Found {} functions", count);
    count
}

// Generate a list of tests in the crate by parsing the output of `cargo test --
// --list`
fn list_tests(opt: &Opt) -> CVResult<Vec<String>> {
    let rustflags = match std::env::var_os("RUSTFLAGS") {
        Some(env_rustflags) => env_rustflags.append(" --cfg=verify"),
        None => OsString::from("--cfg=verify"),
    };

    let mut cmd = Command::new("cargo");
    cmd.arg("test");

    if !opt.features.is_empty() {
        cmd.arg("--features").arg(opt.features.join(","));
    }

    cmd.args(&["--", "--list"])
        // .arg("--exclude-should-panic")
        .current_dir(&opt.crate_dir)
        // .env("PATH", ...)
        .env("RUSTFLAGS", rustflags);

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

    let tests = stdout
        .lines()
        .filter_map(|l| {
            TEST.captures(l)
                .map(|caps| caps.get(1).unwrap().as_str().into())
        })
        .collect();

    Ok(tests)
}

// Find a function defined in LLVM bitcode file
// Demangle all the function names, and compare tham to `names`.
fn mangle_functions(
    opt: &Opt,
    bcfile: &Path,
    names: &[impl AsRef<str>],
) -> CVResult<Vec<(String, String)>> {
    let names: HashSet<&str> = names.iter().map(AsRef::as_ref).collect();

    info_at!(&opt, 4, "    Looking up {:?} in {}", names, bcfile.to_string_lossy());

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
                // The alternative format ({:#}) is without the hash at the end.
                let dname = format!("{:#}", demangle(mangled));
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

    info_at!(&opt, 4, "      Found {:?}", rs);

    // TODO: this doesn't look right:
    // missing = set(paths) - paths.keys()
    let missing = names.len() - rs.len();
    if missing > 0 {
        Err(format!("Unable to find {} tests in bytecode file", missing))?
    }
    Ok(rs)
}
