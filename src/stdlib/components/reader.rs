//! What a **Reader** is made of, shared by `process`, whose Child's output streams are Readers, and
//! `http`, whose response bodies are. Original to avarice, not derived from Astra.
//!
//! A Reader takes bytes in order, a chunk, a line or all that is left at a time, and is read by
//! one task at a time. Each module gives Lua its own userdata over this, since each raises its
//! own errors: `process` plain ones, and `http` tables with a `kind`. So what a source fails with
//! is handed back as an `io::Error` for the module to describe, inside the `mlua::Error` for a
//! Reader misused, which every module raises the same way.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::{Mutex, MutexGuard};

/// The most a Reader's `read` gives at once.
pub const CHUNK: usize = 8 * 1024;

pub type ReadStream = BufReader<Box<dyn AsyncRead + Send + Unpin>>;

pub fn buffered(stream: Box<dyn AsyncRead + Send + Unpin>) -> ReadStream {
    BufReader::with_capacity(CHUNK, stream)
}

/// A stream a Reader reads, shared between every Lua handle to it. `None` once closed.
#[derive(Clone)]
pub struct Reader {
    stream: Arc<Mutex<Option<ReadStream>>>,
    /// The module it belongs to, and what it is, for messages: `process` and `the Child's stdout`.
    module: &'static str,
    what: &'static str,
}

impl Reader {
    pub fn new(stream: ReadStream, module: &'static str, what: &'static str) -> Self {
        Reader {
            stream: Arc::new(Mutex::new(Some(stream))),
            module,
            what,
        }
    }

    /// The stream, for as long as one task reads it. A second task at once is an error, rather
    /// than each getting some of the other's data, and so is reading one that is closed.
    pub fn take(&self) -> mlua::Result<Taken<'_>> {
        let guard = self.stream.try_lock().map_err(|_| {
            mlua::Error::runtime(format!(
                "{}: {} is already being read by another task",
                self.module, self.what
            ))
        })?;
        if guard.is_none() {
            return Err(mlua::Error::runtime(format!(
                "{}: {} is closed",
                self.module, self.what
            )));
        }
        Ok(Taken(guard))
    }

    /// Closes it, dropping the stream, unless a task is reading it, which then finds it closed
    /// when it next asks. Closing one already closed does nothing.
    pub async fn close(&self) {
        self.stream.lock().await.take();
    }

    /// The next chunk, of at most [`CHUNK`] bytes, or `None` at the end.
    pub async fn read(&self) -> mlua::Result<io::Result<Option<Vec<u8>>>> {
        let mut taken = self.take()?;
        Ok(taken.read().await)
    }

    /// The next line, without its `\n` or `\r\n`, or `None` at the end.
    pub async fn line(&self) -> mlua::Result<io::Result<Option<Vec<u8>>>> {
        let mut taken = self.take()?;
        Ok(taken.line().await)
    }

    /// Everything left.
    pub async fn rest(&self) -> mlua::Result<io::Result<Vec<u8>>> {
        let mut taken = self.take()?;
        let mut rest = Vec::new();
        Ok(taken.drain_into(&mut rest).await.map(|()| rest))
    }
}

/// A Reader's stream, held by the one task reading it.
pub struct Taken<'a>(MutexGuard<'a, Option<ReadStream>>);

impl Taken<'_> {
    fn stream(&mut self) -> &mut ReadStream {
        self.0.as_mut().expect("a Reader is taken only while open")
    }

    pub async fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
        let stream = self.stream();
        let chunk = stream.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(None);
        }
        let chunk = chunk[..chunk.len().min(CHUNK)].to_vec();
        stream.consume(chunk.len());
        Ok(Some(chunk))
    }

    pub async fn line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        if self.stream().read_until(b'\n', &mut line).await? == 0 {
            return Ok(None);
        }
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        Ok(Some(line))
    }

    /// Reads to the end into `into`, keeping what was read if it fails or is dropped part way.
    pub async fn drain_into(&mut self, into: &mut Vec<u8>) -> io::Result<()> {
        drain_into(self.stream(), into).await
    }
}

/// Reads `stream` to its end into `into`, keeping what was read if it fails or is dropped part
/// way.
pub async fn drain_into(stream: &mut ReadStream, into: &mut Vec<u8>) -> io::Result<()> {
    while stream.read_buf(into).await? != 0 {}
    Ok(())
}
