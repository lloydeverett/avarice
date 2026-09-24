//! `process` is original to avarice-rt, not derived from Astra: ADR 0016 records why. It starts
//! programs from a **Command**, a Lua table, without a shell; `run` waits for the **Child** and
//! gives back its **Output**, and `spawn` gives back the Child, whose standard streams Lua reads
//! and writes as it runs.
//!
//! Every Child is owned by a watching task on the executor, which waits for it to exit and kills it
//! if the task is dropped: by `abort_tasks`, or with the runtime. So a Child counts as a task, and
//! whatever the runtime does with tasks it does with Children. The Child itself sits behind a lock
//! that the watching task and the Lua handle share, so that killing it works from either, and so
//! that nothing signals it after its exit status has been collected, when its pid may already
//! belong to another program.
//!
//! What fails to start, fails a `check` or times out is not raised here: the primitives return
//! `nil`, the error's fields and its message, and `process.lua` raises them as a table.

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use mlua::{
    IntoLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Table, UserData,
    UserDataFields, UserDataMethods, Value,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::ChildStdin;
use tokio::sync::{Mutex, watch};

use super::{AstraBuffer, AstraBufferMut};

/// The most a reader's `read` gives at once.
const CHUNK: usize = 8 * 1024;

pub fn register_to_lua(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__process_run",
        lua.create_async_function(|lua, command: Value| async move {
            run(&lua, command).await
        })?,
    )?;
    lua.globals().set(
        "astra_internal__process_spawn",
        lua.create_async_function(|lua, command: Value| async move {
            spawn(&lua, command).await
        })?,
    )?;
    Ok(())
}

// A Command.

/// Which function a Command was given to, for its defaults, what it accepts, and its messages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Run,
    Spawn,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Tier::Run => "process.run",
            Tier::Spawn => "process.spawn",
        }
    }
}

/// Where a stream goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamSetting {
    Pipe,
    Inherit,
    Null,
}

enum Stdin {
    Setting(StreamSetting),
    /// Bytes written to the Child's input, which is then closed.
    Feed(Bytes),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stderr {
    Setting(StreamSetting),
    /// Wherever stdout goes, interleaved with it.
    Stdout,
}

struct Command {
    /// The program as the Command gave it: what errors and `tostring` name it by, and what the
    /// Child is told its own name is.
    program: Vec<u8>,
    program_os: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    /// Each variable to set, or with `None`, to remove.
    env: Vec<(String, Option<OsString>)>,
    clear_env: bool,
    stdin: Stdin,
    stdout: StreamSetting,
    stderr: Stderr,
    check: bool,
    /// The timeout, and the milliseconds it was given as, for its message.
    timeout: Option<(Duration, String)>,
}

impl Command {
    fn display(&self) -> String {
        String::from_utf8_lossy(&self.program).into_owned()
    }
}

fn error(message: String) -> mlua::Error {
    mlua::Error::runtime(message)
}

/// Checks a Command eagerly, before anything starts, so that a mistake in it is an error where it
/// was made and never input left out (ADR 0017).
async fn parse(lua: &Lua, tier: Tier, value: Value) -> mlua::Result<Command> {
    let name = tier.name();
    let table = match value {
        Value::Table(table) => table,
        Value::String(_) => {
            return Err(error(format!(
                r#"{name}: a Command is a table, such as {name}({{ "ls", "-la" }}): a string is "#
            ) + "not split into a program and its arguments, since no shell is involved"));
        }
        other => {
            return Err(error(format!(
                r#"{name}: a Command is a table, such as {name}({{ "ls", "-la" }}), not a {}"#,
                other.type_name()
            )));
        }
    };

    let length = table.raw_len();
    let mut fields = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        match key {
            Value::Integer(index) if index >= 1 && index as usize <= length => {}
            Value::String(key) => fields.push((key.to_string_lossy(), value)),
            _ => {
                return Err(error(format!(
                    "{name}: a Command's program and arguments are its entries 1 to n, with no gaps"
                )));
            }
        }
    }

    let program: Vec<u8> = match table.raw_get::<Value>(1)? {
        Value::String(program) if !program.as_bytes().is_empty() => program.as_bytes().to_vec(),
        _ => {
            return Err(error(format!(
                "{name}: a Command needs a program, as a string in its first entry"
            )));
        }
    };
    let program_os = os_string(&program, || format!("{name}: the program"))?;
    let mut args = Vec::new();
    for index in 2..=length {
        let arg = match table.raw_get::<Value>(index)? {
            Value::String(arg) => arg,
            number @ (Value::Integer(_) | Value::Number(_)) => lua
                .coerce_string(number)?
                .expect("a number converts to a string"),
            other => {
                return Err(error(format!(
                    "{name}: entry {index} of the Command is a {}, where an argument is a string",
                    other.type_name()
                )));
            }
        };
        args.push(os_string(&arg.as_bytes(), || {
            format!("{name}: entry {index} of the Command")
        })?);
    }

    let mut command = Command {
        program,
        program_os,
        args,
        cwd: None,
        env: Vec::new(),
        clear_env: false,
        stdin: Stdin::Setting(match tier {
            Tier::Run => StreamSetting::Null,
            Tier::Spawn => StreamSetting::Pipe,
        }),
        stdout: StreamSetting::Pipe,
        stderr: Stderr::Setting(StreamSetting::Pipe),
        check: false,
        timeout: None,
    };

    // `stdio` first, so that a stream's own field wins over it whatever order `pairs` gives.
    if let Some((_, stdio)) = fields.iter().find(|(key, _)| key == "stdio") {
        let stream = stream(name, "stdio", stdio)?.ok_or_else(|| {
            error(format!(r#"{name}: stdio is "pipe", "inherit" or "null""#))
        })?;
        command.stdin = Stdin::Setting(stream);
        command.stdout = stream;
        command.stderr = Stderr::Setting(stream);
    }
    for (key, value) in fields {
        match key.as_str() {
            "stdio" => {}
            "cwd" => {
                let Value::String(cwd) = &value else {
                    return Err(error(format!(
                        "{name}: cwd is a path, as a string, not a {}",
                        value.type_name()
                    )));
                };
                command.cwd = Some(
                    os_string(&cwd.as_bytes(), || format!("{name}: cwd"))?.into(),
                );
            }
            "env" => command.env = env(name, value)?,
            "clear_env" => command.clear_env = boolean(name, "clear_env", value)?,
            "stdin" => {
                command.stdin = match stream(name, "stdin", &value)? {
                    Some(StreamSetting::Pipe) if tier == Tier::Run => {
                        return Err(error(format!(
                            r#"{name}: stdin cannot be "pipe", since nothing could write to it: "#
                        ) + "feed the Child a string or a Buffer, or use process.spawn"));
                    }
                    Some(stream) => Stdin::Setting(stream),
                    None => Stdin::Feed(bytes_of(&value).await.ok_or_else(|| {
                        error(format!(
                            "{name}: stdin is a stream setting, a string or a Buffer"
                        ))
                    })?),
                }
            }
            "stdout" => {
                command.stdout = stream(name, "stdout", &value)?.ok_or_else(|| {
                    error(format!(r#"{name}: stdout is "pipe", "inherit" or "null""#))
                })?;
            }
            "stderr" => {
                command.stderr = match &value {
                    Value::String(s) if s.as_bytes().as_ref() == b"stdout" => Stderr::Stdout,
                    _ => Stderr::Setting(stream(name, "stderr", &value)?.ok_or_else(|| {
                        error(format!(
                            r#"{name}: stderr is "pipe", "inherit", "null" or "stdout""#
                        ))
                    })?),
                }
            }
            "check" | "timeout" if tier == Tier::Spawn => {
                return Err(error(format!(
                    "{name}: `{key}` is for process.run only; a spawned Child is killed or waited \
                     for by the script holding it"
                )));
            }
            "check" => command.check = boolean(name, "check", value)?,
            "timeout" => command.timeout = Some(timeout(name, value)?),
            _ => {
                return Err(error(format!("{name}: a Command has no field `{key}`")));
            }
        }
    }
    Ok(command)
}

/// A stream setting, `None` for a value that is not one, and an error for a value no stream takes.
fn stream(name: &str, field: &str, value: &Value) -> mlua::Result<Option<StreamSetting>> {
    match value {
        Value::String(setting) => Ok(match setting.as_bytes().as_ref() {
            b"pipe" => Some(StreamSetting::Pipe),
            b"inherit" => Some(StreamSetting::Inherit),
            b"null" => Some(StreamSetting::Null),
            _ => None,
        }),
        Value::UserData(_) if field == "stdin" => Ok(None),
        other => Err(error(format!(
            "{name}: {field} is a string, not a {}",
            other.type_name()
        ))),
    }
}

/// Bytes to feed a Child's input: a string's, or a Buffer's. `None` for anything else.
async fn bytes_of(value: &Value) -> Option<Bytes> {
    match value {
        Value::String(bytes) => Some(Bytes::copy_from_slice(&bytes.as_bytes())),
        Value::UserData(buffer) => {
            if let Ok(buffer) = buffer.borrow::<AstraBuffer>() {
                let buffer = buffer.clone();
                return Some(buffer.lock().await.clone());
            }
            if let Ok(buffer) = buffer.borrow::<AstraBufferMut>() {
                let buffer = buffer.clone();
                return Some(Bytes::copy_from_slice(&buffer.lock().await));
            }
            None
        }
        _ => None,
    }
}

fn env(name: &str, value: Value) -> mlua::Result<Vec<(String, Option<OsString>)>> {
    let Value::Table(table) = value else {
        return Err(error(format!(
            "{name}: env is a table of variables, not a {}",
            value.type_name()
        )));
    };
    let mut env = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let key = match key {
            Value::String(key) => key
                .to_str()
                .map_err(|_| error(format!("{name}: a variable's name in env is not UTF-8")))?
                .to_owned(),
            other => {
                return Err(error(format!(
                    "{name}: env's keys are variable names, not a {}",
                    other.type_name()
                )));
            }
        };
        if key.is_empty() || key.contains(['=', '\0']) {
            return Err(error(format!(
                "{name}: `{key}` in env is not a variable name: one is not empty, and holds no \
                 `=` or NUL"
            )));
        }
        let value = match value {
            Value::String(value) => Some(os_string(&value.as_bytes(), || {
                format!("{name}: env.{key}")
            })?),
            Value::Boolean(false) => None,
            other => {
                return Err(error(format!(
                    "{name}: env.{key} is a {}; a variable is set to a string, or removed with \
                     false",
                    other.type_name()
                )));
            }
        };
        env.push((key, value));
    }
    Ok(env)
}

fn boolean(name: &str, field: &str, value: Value) -> mlua::Result<bool> {
    match value {
        Value::Boolean(value) => Ok(value),
        other => Err(error(format!(
            "{name}: {field} is a boolean, not a {}",
            other.type_name()
        ))),
    }
}

fn timeout(name: &str, value: Value) -> mlua::Result<(Duration, String)> {
    let milliseconds = match value {
        Value::Integer(ms) => ms as f64,
        Value::Number(ms) => ms,
        _ => f64::NAN,
    };
    if !(milliseconds.is_finite() && milliseconds > 0.0) {
        return Err(error(format!(
            "{name}: timeout is a number of milliseconds greater than 0"
        )));
    }
    let shown = if milliseconds.fract() == 0.0 {
        format!("{}", milliseconds as i64)
    } else {
        milliseconds.to_string()
    };
    Ok((Duration::from_secs_f64(milliseconds / 1000.0), shown))
}

/// What the operating system is given for a string: its exact bytes on Unix, where they are what a
/// program receives, and elsewhere the string if it is valid Unicode (ADR 0015). `what` names it,
/// never quotes it, since arguments are where secrets are passed.
fn os_string(bytes: &[u8], what: impl FnOnce() -> String) -> mlua::Result<OsString> {
    if bytes.contains(&0) {
        return Err(error(format!(
            "{} holds a NUL byte, which cannot be passed to a program",
            what()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(not(unix))]
    {
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(text.into()),
            Err(_) => Err(error(format!(
                "{} is not valid Unicode, which a program cannot be given on this platform",
                what()
            ))),
        }
    }
}

// Starting a Child.

/// Why a Child did not start, for a `"start"` error.
struct StartFailure {
    reason: Reason,
    message: String,
}

/// A `"start"` error's `reason`.
#[derive(Clone, Copy)]
enum Reason {
    NotFound,
    PermissionDenied,
    BadCwd,
    Other,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Reason::NotFound => "not_found",
            Reason::PermissionDenied => "permission_denied",
            Reason::BadCwd => "bad_cwd",
            Reason::Other => "other",
        }
    }
}

impl StartFailure {
    fn describe(&self) -> String {
        match self.reason {
            Reason::NotFound => "program not found".to_owned(),
            Reason::PermissionDenied => "permission denied".to_owned(),
            Reason::BadCwd => format!("bad working directory: {}", self.message),
            Reason::Other => self.message.clone(),
        }
    }
}

impl From<std::io::Error> for StartFailure {
    fn from(error: std::io::Error) -> Self {
        let reason = match error.kind() {
            std::io::ErrorKind::NotFound => Reason::NotFound,
            std::io::ErrorKind::PermissionDenied => Reason::PermissionDenied,
            _ => Reason::Other,
        };
        StartFailure {
            reason,
            message: error.to_string(),
        }
    }
}

type ReadStream = BufReader<Box<dyn AsyncRead + Send + Unpin>>;

/// A Child that has started, with its streams not yet handed to anyone.
struct Started {
    shared: Arc<Shared>,
    /// `None` if piped only to be fed, which the watching task does.
    stdin: Option<ChildStdin>,
    stdout: Option<ReadStream>,
    stderr: Option<ReadStream>,
}

/// What the watching task and every Lua handle to a Child share.
struct Shared {
    /// Locked only for as long as it takes to poll it or signal it, never across an `await`.
    child: std::sync::Mutex<tokio::process::Child>,
    pid: u32,
    program: String,
    /// `None` while the Child runs. The watching task holds the sender, so a Child whose task was
    /// dropped is one whose sender is gone.
    status: watch::Receiver<Option<Result<ExitStatus, String>>>,
}

impl Shared {
    fn child(&self) -> MutexGuard<'_, tokio::process::Child> {
        self.child.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forcefully, and at once. tokio refuses to signal a Child whose exit it has parts.
    fn kill(&self) -> std::io::Result<()> {
        match self.child().start_kill() {
            Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => Ok(()),
            other => other,
        }
    }

    /// Politely: `SIGTERM` on Unix, where the Child may clean up, and elsewhere the same as
    /// [`Shared::kill`].
    fn terminate(&self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            let child = self.child();
            // `None` once tokio has collected its exit status, and the lock keeps it from doing so
            // meanwhile: until then the pid is the Child's, and after it, maybe not.
            let Some(pid) = child.id() else {
                return Ok(());
            };
            let pid = libc::pid_t::try_from(pid)
                .ok()
                .filter(|pid| *pid > 0)
                .ok_or_else(|| std::io::Error::other("the Child's pid is out of range"))?;
            // SAFETY: `kill` takes no pointers. A positive pid names one process, never a group.
            if unsafe { libc::kill(pid, libc::SIGTERM) } == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            self.kill()
        }
    }

    /// Waits for the Child to exit, which the watching task reports.
    async fn exit_status(&self) -> mlua::Result<ExitStatus> {
        let mut status = self.status.clone();
        match status.wait_for(Option::is_some).await {
            Ok(status) => match status.as_ref().expect("waited for") {
                Ok(status) => Ok(*status),
                Err(e) => Err(error(format!(
                    "process: could not wait for {}: {e}",
                    self.program
                ))),
            },
            Err(_) => Err(error(format!(
                "process: {} was killed when the tasks were aborted",
                self.program
            ))),
        }
    }

    fn state(&self) -> String {
        let status = self.status.borrow();
        match &*status {
            Some(Ok(status)) => match (status.code(), signal(status)) {
                (Some(code), _) => format!("exited {code}"),
                (None, Some(signal)) => format!("killed by signal {signal}"),
                (None, None) => "exited".to_owned(),
            },
            Some(Err(_)) => "lost".to_owned(),
            None if self.status.has_changed().is_err() => "killed".to_owned(),
            None => "running".to_owned(),
        }
    }
}

/// Kills the Child when dropped, unless it has already exited. The watching task holds one, so
/// that the Child dies with the task; `run` holds another, so that it dies with `run`.
struct KillOnDrop(Arc<Shared>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

async fn start(command: &Command) -> Result<Started, StartFailure> {
    if let Some(cwd) = &command.cwd {
        match tokio::fs::metadata(cwd).await {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(StartFailure {
                    reason: Reason::BadCwd,
                    message: "not a directory".to_owned(),
                });
            }
            Err(e) => {
                return Err(StartFailure {
                    reason: Reason::BadCwd,
                    message: e.to_string(),
                });
            }
        }
    }
    let program = resolve(command)?;

    let mut builder = tokio::process::Command::new(&program);
    #[cfg(unix)]
    builder.arg0(&command.program_os);
    builder.args(&command.args).kill_on_drop(true);
    if let Some(cwd) = &command.cwd {
        builder.current_dir(cwd);
    }
    if command.clear_env {
        builder.env_clear();
    }
    for (key, value) in &command.env {
        match value {
            Some(value) => builder.env(key, value),
            None => builder.env_remove(key),
        };
    }

    builder.stdin(match &command.stdin {
        Stdin::Setting(stream) => stdio(*stream),
        Stdin::Feed(_) => Stdio::piped(),
    });
    let mut merged = None;
    match command.stderr {
        Stderr::Setting(stream) => {
            builder.stdout(stdio(command.stdout));
            builder.stderr(stdio(stream));
        }
        Stderr::Stdout => match command.stdout {
            StreamSetting::Pipe => {
                let (reader, writer) = std::io::pipe()?;
                builder.stdout(writer.try_clone()?);
                builder.stderr(writer);
                merged = Some(reader);
            }
            StreamSetting::Inherit => {
                builder.stdout(Stdio::inherit());
                builder.stderr(host_stdout()?);
            }
            StreamSetting::Null => {
                builder.stdout(Stdio::null());
                builder.stderr(Stdio::null());
            }
        },
    }

    let mut child = builder.spawn()?;
    // The builder holds our copy of a merged pipe's writing end, which would keep it open.
    drop(builder);

    let mut stdin = child.stdin.take();
    let feeding = match &command.stdin {
        Stdin::Feed(bytes) => stdin.take().map(|stdin| feed(stdin, bytes.clone())),
        Stdin::Setting(_) => None,
    };
    let stdout = match merged {
        Some(reader) => Some(buffered(merged_reader(reader)?)),
        None => child.stdout.take().map(|s| buffered(Box::new(s))),
    };
    let stderr = child.stderr.take().map(|s| buffered(Box::new(s)));
    let pid = child.id().unwrap_or_default();

    let (report, status) = watch::channel(None);
    let shared = Arc::new(Shared {
        child: std::sync::Mutex::new(child),
        pid,
        program: command.display(),
        status,
    });
    let watching = KillOnDrop(shared.clone());
    tokio::spawn(async move {
        let exited = std::future::poll_fn(|cx| {
            let mut child = watching.0.child();
            let wait = child.wait();
            std::pin::pin!(wait).poll(cx)
        });
        let status = match feeding {
            None => exited.await,
            // Fed until it has all been written or the Child exits, whichever is first: only the
            // Child is managed, and a program it started may hold its input open for ever.
            Some(feeding) => {
                let mut exited = std::pin::pin!(exited);
                tokio::select! {
                    status = exited.as_mut() => status,
                    () = feeding => exited.await,
                }
            }
        };
        let _ = report.send(Some(status.map_err(|e| e.to_string())));
        drop(watching);
    });

    Ok(Started {
        shared,
        stdin,
        stdout,
        stderr,
    })
}

/// Finds the program the way ADR 0016 says, the same on every platform: a path with a separator
/// against the Command's `cwd`, and a bare name on the `PATH` the Child will get.
fn resolve(command: &Command) -> Result<PathBuf, StartFailure> {
    let path = child_path(command);
    let base = match &command.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir()?,
    };
    let program = Path::new(&command.program_os);
    let found = match which::which_in(program, path.as_deref(), &base) {
        Ok(found) => found,
        Err(_) => match unrunnable(program, path.as_deref(), &base) {
            Some(file) => file,
            None => {
                return Err(StartFailure {
                    reason: Reason::NotFound,
                    message: "program not found".to_owned(),
                });
            }
        },
    };
    // A relative `cwd` gives a relative path, which the Child's own working directory would
    // otherwise be taken to be relative to.
    Ok(if found.is_relative() {
        std::env::current_dir()?.join(found)
    } else {
        found
    })
}

/// A file that is there but that `which` passed over because it cannot be run, so that starting it
/// has the operating system say why, where `which` would only say it is not found. A path with a
/// separator names one; a bare name is searched for on `path` as `execvp` does, where only a file
/// will do. On Windows a bare name only names a program with an extension from `PATHEXT`, which
/// `which` has tried, and a file without one is not a program at all.
fn unrunnable(program: &Path, path: Option<&OsStr>, base: &Path) -> Option<PathBuf> {
    if program.components().count() > 1 {
        let file = base.join(program);
        return file.exists().then_some(file);
    }
    if cfg!(windows) {
        return None;
    }
    std::env::split_paths(path?)
        .map(|dir| base.join(dir).join(program))
        .find(|file| file.is_file())
}

/// The `PATH` the Child will get, after `env` and `clear_env`.
fn child_path(command: &Command) -> Option<OsString> {
    let is_path = |key: &str| {
        if cfg!(windows) {
            key.eq_ignore_ascii_case("PATH")
        } else {
            key == "PATH"
        }
    };
    match command.env.iter().rev().find(|(key, _)| is_path(key)) {
        Some((_, value)) => value.clone(),
        None if command.clear_env => None,
        None => std::env::var_os("PATH"),
    }
}

fn stdio(stream: StreamSetting) -> Stdio {
    match stream {
        StreamSetting::Pipe => Stdio::piped(),
        StreamSetting::Inherit => Stdio::inherit(),
        StreamSetting::Null => Stdio::null(),
    }
}

/// The host's own standard output, for a Child whose stderr goes where its inherited stdout does.
fn host_stdout() -> std::io::Result<Stdio> {
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        Ok(std::io::stdout().as_fd().try_clone_to_owned()?.into())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        Ok(std::io::stdout().as_handle().try_clone_to_owned()?.into())
    }
}

/// Reads the pipe a Child's stdout and stderr share. On Unix it is read without blocking, like
/// tokio's own pipes. On Windows an anonymous pipe cannot be, so it is read on tokio's blocking
/// pool, which is what tokio does with a Child's own pipes there.
fn merged_reader(
    reader: std::io::PipeReader,
) -> std::io::Result<Box<dyn AsyncRead + Send + Unpin>> {
    #[cfg(unix)]
    {
        let fd: std::os::fd::OwnedFd = reader.into();
        Ok(Box::new(tokio::net::unix::pipe::Receiver::from_owned_fd(fd)?))
    }
    #[cfg(windows)]
    {
        let handle: std::os::windows::io::OwnedHandle = reader.into();
        Ok(Box::new(tokio::fs::File::from_std(std::fs::File::from(handle))))
    }
}

fn buffered(stream: Box<dyn AsyncRead + Send + Unpin>) -> ReadStream {
    BufReader::with_capacity(CHUNK, stream)
}

#[cfg(unix)]
fn signal(status: &ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn signal(_: &ExitStatus) -> Option<i32> {
    None
}

// `run`.

/// A failure `process.lua` raises as a table.
enum Failure {
    Start(StartFailure),
    Exit(OutputParts),
    Timeout(OutputParts),
}

/// An Output, before it is made a Lua table.
struct OutputParts {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run(lua: &Lua, command: Value) -> mlua::Result<MultiValue> {
    let command = parse(lua, Tier::Run, command).await?;
    let started = match start(&command).await {
        Ok(started) => started,
        Err(failure) => return raise(lua, &command, Failure::Start(failure)),
    };
    let Started {
        shared,
        stdin,
        stdout,
        stderr,
    } = started;
    // If this future is dropped, by an aborted task or a cancel, so is the Child.
    let _kill = KillOnDrop(shared.clone());
    // Only piped here by `stdio = "pipe"`, since `stdin = "pipe"` is refused: closed at once. Bytes
    // to feed are fed by the watching task.
    drop(stdin);

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let collect = async {
        let (stdout, stderr, status) = tokio::join!(
            drain(stdout, &mut out),
            drain(stderr, &mut err),
            shared.exit_status(),
        );
        stdout?;
        stderr?;
        status
    };
    let status = match &command.timeout {
        None => Some(collect.await?),
        Some((timeout, _)) => match tokio::time::timeout(*timeout, collect).await {
            Ok(status) => Some(status?),
            Err(_) => None,
        },
    };

    let Some(status) = status else {
        // Only the Child is killed, and only it is waited for: a program it started may hold its
        // pipes open for as long as it likes.
        shared.kill().map_err(mlua::Error::external)?;
        let status = shared.exit_status().await?;
        let parts = OutputParts {
            status,
            stdout: out,
            stderr: err,
        };
        return raise(lua, &command, Failure::Timeout(parts));
    };
    let parts = OutputParts {
        status,
        stdout: out,
        stderr: err,
    };
    if command.check && !status.success() {
        return raise(lua, &command, Failure::Exit(parts));
    }
    output_table(lua, parts)?.into_lua_multi(lua)
}

/// Writes `bytes` to a Child's input and closes it. A Child that exits or closes its input before
/// reading it all has chosen not to, and is not an error, as in Python's `communicate`; nor is any
/// other failure to write, which only ends the Child's input early too.
async fn feed(mut stdin: ChildStdin, bytes: Bytes) {
    let _ = stdin.write_all(&bytes).await;
}

/// Reads a stream to its end into `into`, keeping what was read if dropped part way.
async fn drain(stream: Option<ReadStream>, into: &mut Vec<u8>) -> mlua::Result<()> {
    let Some(mut stream) = stream else {
        return Ok(());
    };
    drain_into(&mut stream, into).await
}

async fn drain_into(stream: &mut ReadStream, into: &mut Vec<u8>) -> mlua::Result<()> {
    loop {
        match stream.read_buf(into).await {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) => {
                return Err(error(format!(
                    "process: could not read the Child's output: {e}"
                )));
            }
        }
    }
}

fn raise(lua: &Lua, command: &Command, failure: Failure) -> mlua::Result<MultiValue> {
    let program = command.display();
    let fields = lua.create_table()?;
    fields.set("program", lua.create_string(&command.program)?)?;
    let message = match failure {
        Failure::Start(failure) => {
            fields.set("kind", "start")?;
            fields.set("reason", failure.reason.as_str())?;
            fields.set("message", failure.message.as_str())?;
            format!("process: could not start '{program}': {}", failure.describe())
        }
        Failure::Exit(parts) => {
            fields.set("kind", "exit")?;
            let message = match (parts.status.code(), signal(&parts.status)) {
                (Some(code), _) => format!("process: {program} exited with code {code}"),
                (None, Some(signal)) => {
                    format!("process: {program} was killed by signal {signal}")
                }
                (None, None) => format!("process: {program} exited unsuccessfully"),
            };
            fields.set("output", output_table(lua, parts)?)?;
            message
        }
        Failure::Timeout(parts) => {
            fields.set("kind", "timeout")?;
            fields.set("output", output_table(lua, parts)?)?;
            let (_, shown) = command.timeout.as_ref().expect("only a timeout times out");
            format!("process: {program} timed out after {shown} ms")
        }
    };
    (Value::Nil, fields, message).into_lua_multi(lua)
}

fn status_table(lua: &Lua, status: ExitStatus) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("ok", status.success())?;
    table.set("code", status.code())?;
    table.set("signal", signal(&status))?;
    Ok(table)
}

fn output_table(lua: &Lua, parts: OutputParts) -> mlua::Result<Table> {
    let table = status_table(lua, parts.status)?;
    table.set("stdout", AstraBuffer::new(parts.stdout.into()))?;
    table.set("stderr", AstraBuffer::new(parts.stderr.into()))?;
    Ok(table)
}

// `spawn`, and the Child it gives.

async fn spawn(lua: &Lua, command: Value) -> mlua::Result<MultiValue> {
    let command = parse(lua, Tier::Spawn, command).await?;
    let started = match start(&command).await {
        Ok(started) => started,
        Err(failure) => return raise(lua, &command, Failure::Start(failure)),
    };
    let child = Child {
        shared: started.shared,
        // `None` if piped only to be fed, which the watching task does.
        stdin: started
            .stdin
            .map(|stdin| Writer(Arc::new(Mutex::new(Some(stdin))))),
        stdout: started.stdout.map(|s| Reader::new(s, "stdout")),
        stderr: started.stderr.map(|s| Reader::new(s, "stderr")),
    };
    child.into_lua_multi(lua)
}

struct Child {
    shared: Arc<Shared>,
    stdin: Option<Writer>,
    stdout: Option<Reader>,
    stderr: Option<Reader>,
}

impl Child {
    /// Closes the Child's input, if Lua has it, as std and tokio do before waiting: a Child
    /// waiting for input that never comes would never exit.
    async fn close_stdin(&self) {
        if let Some(stdin) = &self.stdin {
            stdin.0.lock().await.take();
        }
    }
}

impl UserData for Child {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("stdin", |_, this| Ok(this.stdin.clone()));
        fields.add_field_method_get("stdout", |_, this| Ok(this.stdout.clone()));
        fields.add_field_method_get("stderr", |_, this| Ok(this.stderr.clone()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "Child({}, pid {}, {})",
                this.shared.program,
                this.shared.pid,
                this.shared.state()
            ))
        });
        methods.add_method("pid", |_, this, ()| Ok(this.shared.pid));
        methods.add_method("kill", |_, this, ()| {
            this.shared.kill().map_err(|e| {
                error(format!("process: could not kill {}: {e}", this.shared.program))
            })
        });
        methods.add_method("terminate", |_, this, ()| {
            this.shared.terminate().map_err(|e| {
                error(format!(
                    "process: could not terminate {}: {e}",
                    this.shared.program
                ))
            })
        });
        methods.add_async_method("wait", |lua, this, ()| async move {
            this.close_stdin().await;
            status_table(&lua, this.shared.exit_status().await?)
        });
        methods.add_async_method("output", |lua, this, ()| async move {
            this.close_stdin().await;
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let (stdout, stderr, status) = tokio::join!(
                drain_reader(this.stdout.as_ref(), &mut out),
                drain_reader(this.stderr.as_ref(), &mut err),
                this.shared.exit_status(),
            );
            stdout?;
            stderr?;
            output_table(
                &lua,
                OutputParts {
                    status: status?,
                    stdout: out,
                    stderr: err,
                },
            )
        });
    }
}

async fn drain_reader(reader: Option<&Reader>, into: &mut Vec<u8>) -> mlua::Result<()> {
    match reader {
        Some(reader) => drain_into(&mut *reader.take()?, into).await,
        None => Ok(()),
    }
}

/// One of a Child's output streams, as Lua reads it.
#[derive(Clone)]
struct Reader {
    stream: Arc<Mutex<ReadStream>>,
    name: &'static str,
}

impl Reader {
    fn new(stream: ReadStream, name: &'static str) -> Self {
        Reader {
            stream: Arc::new(Mutex::new(stream)),
            name,
        }
    }

    /// The stream, for as long as one task reads it. A second task at once is an error, rather
    /// than each getting some of the other's data.
    fn take(&self) -> mlua::Result<tokio::sync::MutexGuard<'_, ReadStream>> {
        self.stream.try_lock().map_err(|_| {
            error(format!(
                "process: the Child's {} is already being read by another task",
                self.name
            ))
        })
    }

    fn failed(&self, e: std::io::Error) -> mlua::Error {
        error(format!("process: could not read the Child's {}: {e}", self.name))
    }

    async fn line(&self, lua: &Lua) -> mlua::Result<Option<LuaString>> {
        let mut stream = self.take()?;
        let mut line = Vec::new();
        if stream
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| self.failed(e))?
            == 0
        {
            return Ok(None);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        lua.create_string(line).map(Some)
    }
}

impl UserData for Reader {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Reader({})", this.name))
        });
        methods.add_async_method("read", |lua, this, ()| async move {
            let mut stream = this.take()?;
            let chunk = stream.fill_buf().await.map_err(|e| this.failed(e))?;
            if chunk.is_empty() {
                return Ok(None);
            }
            let length = chunk.len().min(CHUNK);
            let chunk = lua.create_string(&chunk[..length])?;
            stream.consume(length);
            Ok(Some(chunk))
        });
        methods.add_async_method("line", |lua, this, ()| async move {
            this.line(&lua).await
        });
        methods.add_method("lines", |lua, this, ()| {
            let reader = this.clone();
            lua.create_async_function(move |lua, _: MultiValue| {
                let reader = reader.clone();
                async move { reader.line(&lua).await }
            })
        });
        methods.add_async_method("rest", |_, this, ()| async move {
            let mut rest = Vec::new();
            drain_into(&mut *this.take()?, &mut rest).await?;
            Ok(AstraBuffer::new(rest.into()))
        });
    }
}

/// A Child's input, as Lua writes it. `None` once closed.
#[derive(Clone)]
struct Writer(Arc<Mutex<Option<ChildStdin>>>);

impl UserData for Writer {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(match this.0.try_lock() {
                Ok(stdin) if stdin.is_none() => "Writer(stdin, closed)",
                _ => "Writer(stdin)",
            })
        });
        methods.add_async_method("write", |_, this, value: Value| async move {
            let bytes = bytes_of(&value).await.ok_or_else(|| {
                error(format!(
                    "process: a Child's input is written as a string or a Buffer, not a {}",
                    value.type_name()
                ))
            })?;
            let mut stdin = this.0.try_lock().map_err(|_| {
                error("process: the Child's stdin is already being written by another task".into())
            })?;
            let Some(writer) = stdin.as_mut() else {
                return Err(error("process: the Child's stdin is closed".into()));
            };
            writer
                .write_all(&bytes)
                .await
                .map_err(|e| error(format!("process: could not write the Child's stdin: {e}")))
        });
        methods.add_async_method("close", |_, this, ()| async move {
            this.0.lock().await.take();
            Ok(())
        });
    }
}
