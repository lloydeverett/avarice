//! `avrt`: the command-line interpreter.

mod escape;
mod interrupt;
mod output;
mod repl;

use std::future::Future;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use avarice_rt::mlua::{Table, Variadic};
use avarice_rt::{was_cancelled, CancelHandle, Error, FsStore, Profile, Runtime};
use clap::Parser;
use output::{eprintln, println};

/// Exit code for a Lua error, matching stock `lua`.
const EXIT_LUA_ERROR: u8 = 1;

/// Exit code for a run cut short by Ctrl-C: 128 plus SIGINT's number, which is what a shell
/// reports for a process that died of it, so `avrt script && next` still stops.
const EXIT_INTERRUPTED: u8 = 130;

/// Exit code for a usage error. clap uses the same code for a parse failure.
const EXIT_USAGE: u8 = 2;

/// Anything that stops `avrt` short of running to completion.
///
/// Reading a script is the command's own business rather than the runtime's, so it has its own
/// variants here: [`avarice_rt::Error::Config`] means the runtime could not be built, and
/// borrowing it to report a missing file would make the two indistinguishable.
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// A script named on the command line could not be read.
    #[error("cannot open {}: {source}", path.display())]
    OpenScript {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Standard input could not be read.
    #[error("cannot read standard input: {source}")]
    ReadStdin { source: std::io::Error },

    /// Ctrl-C stopped the run.
    #[error("interrupted")]
    Interrupted,

    /// Anything the runtime, or the Lua code it ran, reported.
    #[error(transparent)]
    Runtime(#[from] Error),
}

impl From<avarice_rt::mlua::Error> for CliError {
    fn from(err: avarice_rt::mlua::Error) -> Self {
        CliError::Runtime(err.into())
    }
}

/// A Lua 5.4 interpreter.
///
/// With no script and no `-e`, `avrt` starts a REPL — or reads a program from standard input,
/// if standard input is not a terminal.
#[derive(Debug, Parser)]
#[command(
    name = "avrt",
    version,
    about,
    disable_version_flag = true,
    trailing_var_arg = true
)]
struct Cli {
    /// Execute a statement. May be repeated; statements run in order, before the script.
    #[arg(short = 'e', value_name = "stat")]
    execute: Vec<String>,

    /// Enter interactive mode after running the script and any -e statements.
    #[arg(short = 'i')]
    interactive: bool,

    /// Show version information.
    #[arg(short = 'v', long = "version")]
    version: bool,

    /// Run in the sandbox profile: no io or os, memory capped, no bytecode.
    #[arg(long)]
    sandbox: bool,

    /// Stop after this long, in seconds. Applies to each script, -e statement or REPL entry, and
    /// again to waiting for the tasks it left running.
    #[arg(long, value_name = "seconds")]
    timeout: Option<f64>,

    /// Directory to resolve `require` against. Defaults to the script's directory.
    #[arg(long, value_name = "dir")]
    path: Vec<PathBuf>,

    /// The script to run, then its arguments. `-` reads the script from standard input.
    #[arg(value_name = "script")]
    script: Vec<String>,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            report(&err);
            ExitCode::from(match err {
                CliError::Interrupted => EXIT_INTERRUPTED,
                _ => EXIT_LUA_ERROR,
            })
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, CliError> {
    if cli.version {
        print_version();
    }

    let script = cli.script.first().map(String::as_str);
    let profile = if cli.sandbox {
        Profile::Sandbox
    } else {
        Profile::Trusted
    };

    // Always cancellable, because Ctrl-C has to be able to stop whatever is running. That installs
    // the limit hook, so every run pays a little throughput for it.
    let cancel = CancelHandle::new();
    let mut builder = Runtime::builder(profile)
        .cancel_handle(cancel.clone())
        .write_sink(output::Stdout::auto());
    if let Some(seconds) = cli.timeout {
        if !(seconds.is_finite() && seconds > 0.0) {
            eprintln!("avrt: --timeout must be a positive number of seconds");
            return Ok(ExitCode::from(EXIT_USAGE));
        }
        builder = builder.time_limit(std::time::Duration::from_secs_f64(seconds));
    }
    let rt = builder.store(store_for(cli, script)).build()?;
    if let Err(e) = interrupt::install(cancel) {
        eprintln!("avrt: Ctrl-C will not be caught: {e}");
    }
    set_arg_table(&rt, &cli.script)?;

    // What to run, if anything. With nothing given, behave like stock lua: a REPL on a terminal,
    // otherwise read the program from standard input.
    let mut main_chunk = match script {
        Some("-") => Some(MainChunk::Stdin),
        Some(path) => Some(MainChunk::File(Path::new(path))),
        None => None,
    };
    let idle = main_chunk.is_none() && cli.execute.is_empty() && !cli.version;
    let interactive = cli.interactive || (idle && std::io::stdin().is_terminal());
    if idle && !interactive {
        main_chunk = Some(MainChunk::Stdin);
    }

    // All of it is one run, so that the tasks it leaves behind are waited for once, after the last
    // of it, rather than after each statement.
    let script_args = cli.script.get(1..).unwrap_or_default();
    run_and_settle_tasks(&rt, async {
        for statement in &cli.execute {
            rt.exec(statement.as_str(), "=(command line)").await?;
        }
        if let Some(chunk) = main_chunk {
            run_main_chunk(&rt, chunk, script_args).await?;
        }
        Ok(())
    })?;

    if interactive {
        repl::run(&rt)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Runs `work` on the runtime's executor, then waits for the tasks it left running, so that a
/// task spawned on a program's last line is not silently abandoned.
///
/// A run that ends in an error gives up its tasks instead, and says how many, so that the person
/// at the terminal knows whether work was lost. That covers a Lua error, a time limit, and
/// Ctrl-C, which is reported as [`CliError::Interrupted`]. Either way no task outlives the call,
/// which is what lets the REPL treat the prompt as a place where nothing is running.
fn run_and_settle_tasks(
    rt: &Runtime,
    work: impl Future<Output = Result<(), CliError>>,
) -> Result<(), CliError> {
    let outcome = rt.block_on(async {
        work.await?;
        rt.wait_for_tasks().await?;
        Ok(())
    });
    let Err(err) = outcome else { return Ok(()) };

    // Called once `block_on` has returned, as `abort_tasks` requires.
    let aborted = rt.abort_tasks()?;
    if aborted > 0 {
        let s = if aborted == 1 { "" } else { "s" };
        eprintln!("avrt: aborting {aborted} running task{s}");
    }
    match err {
        CliError::Runtime(Error::Lua(ref lua)) if was_cancelled(lua) => Err(CliError::Interrupted),
        err => Err(err),
    }
}

/// Where the main chunk comes from: the script named on the command line, or standard input.
#[derive(Clone, Copy)]
enum MainChunk<'a> {
    Stdin,
    File(&'a Path),
}

/// Reads the main chunk and runs it, passing its arguments as its varargs.
///
/// A script reaches them either as `...` or through the `arg` table; stock `lua` provides both,
/// and scripts use both.
///
/// The read is synchronous although this is async: it happens once, before the chunk starts, and
/// whatever tasks an earlier `-e` statement spawned can wait for it.
async fn run_main_chunk(
    rt: &Runtime,
    chunk: MainChunk<'_>,
    args: &[String],
) -> Result<(), CliError> {
    let (source, name) = match chunk {
        MainChunk::File(path) => {
            let source = std::fs::read(path).map_err(|source| CliError::OpenScript {
                path: path.to_path_buf(),
                source,
            })?;
            (source, format!("@{}", path.display()))
        }
        MainChunk::Stdin => {
            let mut source = Vec::new();
            std::io::stdin()
                .read_to_end(&mut source)
                .map_err(|source| CliError::ReadStdin { source })?;
            (source, "=stdin".to_string())
        }
    };
    // A leading `#!` line is not Lua, but a script with one should still run.
    let source = strip_shebang(source);
    let args: Variadic<String> = args.to_vec().into();
    rt.run(rt.load(source, name).call_async::<()>(args)).await?;
    Ok(())
}

fn strip_shebang(mut source: Vec<u8>) -> Vec<u8> {
    if source.starts_with(b"#") {
        let line_end = source
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(source.len());
        // Blank the line rather than removing it, so reported line numbers still line up.
        source[..line_end].fill(b' ');
    }
    source
}

/// Where `require` looks, by default the script's own directory.
fn store_for(cli: &Cli, script: Option<&str>) -> FsStore {
    if !cli.path.is_empty() {
        return FsStore::with_roots(cli.path.clone());
    }
    let root = match script {
        Some("-") | None => PathBuf::from("."),
        Some(path) => Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
    };
    FsStore::new(root)
}

/// Builds Lua's `arg` table.
///
/// `arg[0]` is the script, `arg[1]` onwards its arguments, and the negative indices walk back
/// through the words before the script to `arg[-n]`, the interpreter itself — the same shape
/// stock `lua` produces, so a script that inspects `arg` behaves the same under `avrt`.
fn set_arg_table(rt: &Runtime, script: &[String]) -> Result<(), Error> {
    let argv: Vec<String> = std::env::args().collect();
    // clap's trailing var arg takes a contiguous tail of argv, so the script sits exactly that
    // many words from the end.
    let script_index = argv.len().saturating_sub(script.len());

    let arg: Table = rt.lua().create_table()?;
    for (offset, word) in argv[..script_index].iter().rev().enumerate() {
        arg.raw_set(-(offset as i64 + 1), word.as_str())?;
    }
    for (offset, word) in argv[script_index..].iter().enumerate() {
        arg.raw_set(offset as i64, word.as_str())?;
    }
    rt.lua().globals().raw_set("arg", arg)?;
    Ok(())
}

fn print_version() {
    println!("avrt {}", env!("CARGO_PKG_VERSION"));
    println!("PUC-Rio Lua 5.4, statically linked");
}

/// Prints an error the way stock `lua` does.
///
/// A traceback is not printed separately: mlua runs Lua code under a message handler that
/// appends the traceback to the error message itself, so it is already part of what is printed
/// here.
fn report(err: &CliError) {
    eprintln!("avrt: {}", message_of(err));
}

/// Strips the wrapper mlua puts around a Lua error, leaving what the script would have seen.
fn message_of(err: &CliError) -> String {
    match err {
        CliError::Runtime(Error::Lua(avarice_rt::mlua::Error::RuntimeError(msg))) => msg.clone(),
        CliError::Runtime(Error::Lua(avarice_rt::mlua::Error::SyntaxError { message, .. })) => {
            message.clone()
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shebang_is_blanked_rather_than_removed() {
        let source = strip_shebang(b"#!/usr/bin/env avrt\nreturn 1\n".to_vec());
        assert_eq!(&source[..19], b"                   ");
        assert!(source.ends_with(b"\nreturn 1\n"));
    }

    #[test]
    fn a_file_with_only_a_shebang_is_handled() {
        assert_eq!(strip_shebang(b"#!/bin/avrt".to_vec()), b"           ");
    }

    #[test]
    fn the_store_follows_the_script() {
        let cli = Cli::parse_from(["avrt", "/srv/app/main.lua"]);
        let store = store_for(&cli, Some("/srv/app/main.lua"));
        assert_eq!(store.roots(), [PathBuf::from("/srv/app")]);

        let cli = Cli::parse_from(["avrt", "main.lua"]);
        let store = store_for(&cli, Some("main.lua"));
        assert_eq!(store.roots(), [PathBuf::from(".")]);
    }

    #[test]
    fn explicit_paths_win_over_the_script_directory() {
        let cli = Cli::parse_from(["avrt", "--path", "/a", "--path", "/b", "/srv/main.lua"]);
        let store = store_for(&cli, Some("/srv/main.lua"));
        assert_eq!(store.roots(), [PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn script_arguments_are_not_parsed_as_options() {
        let cli = Cli::parse_from(["avrt", "-e", "x = 1", "main.lua", "-i", "--sandbox"]);
        assert_eq!(cli.execute, ["x = 1"]);
        assert!(
            !cli.interactive,
            "-i after the script belongs to the script"
        );
        assert!(!cli.sandbox);
        assert_eq!(cli.script, ["main.lua", "-i", "--sandbox"]);
    }
}
