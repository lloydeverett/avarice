//! `http` is original to avarice, not derived from Astra: ADR 0025 records why. A **Client** sends
//! requests and keeps its connections to reuse; a response comes back once its headers have
//! arrived, and its body is read as it arrives.
//!
//! This is the half of the module that touches the network. `http.lua` is the rest: it checks
//! and merges options, joins URLs, and gives Lua the Client, the response and its **Reader**.
//! What fails here is not raised: the primitives return `false`, the error's fields and its
//! message, and `http.lua` raises them as a table, as `process` does.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use mlua::{IntoLuaMulti, Lua, LuaString, MultiValue, Table, UserData, UserDataMethods, Value};
use tokio::time::Instant;

use super::AstraBuffer;
use super::astra_serde::encode_json;
use super::reader::{Reader, buffered};

pub fn register_to_lua(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__http_client",
        lua.create_function(|_, connect_timeout: Option<f64>| Client::new(connect_timeout))?,
    )?;
    lua.globals().set(
        "astra_internal__http_redacted",
        lua.create_function(|_, url: String| Ok(redacted_str(&url)))?,
    )
}

/// A Client, as Rust holds it: one reqwest client, whose connections every request through it
/// shares, and its connect timeout in milliseconds.
///
/// reqwest sends `Accept-Encoding` with every request from a client that decompresses, whether
/// or not the request already removed it. So a request that removes it goes through a second
/// client that does not decompress, made the first time one does, with connections of its own.
#[derive(Clone)]
struct Client {
    client: reqwest::Client,
    plain: Arc<OnceLock<reqwest::Client>>,
    connect_timeout: Option<f64>,
}

impl Client {
    fn new(connect_timeout: Option<f64>) -> mlua::Result<Client> {
        Ok(Client {
            client: Client::build(connect_timeout, true)?,
            plain: Arc::default(),
            connect_timeout,
        })
    }

    fn build(connect_timeout: Option<f64>, decompress: bool) -> mlua::Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder();
        if let Some(ms) = connect_timeout {
            builder = builder.connect_timeout(milliseconds(ms));
        }
        if !decompress {
            builder = builder.no_gzip().no_brotli().no_zstd();
        }
        builder
            .build()
            .map_err(|e| mlua::Error::runtime(format!("http: could not make a Client: {e}")))
    }

    /// The reqwest client for a request whose headers are `headers`.
    fn for_headers(&self, headers: &Table) -> mlua::Result<&reqwest::Client> {
        if headers.get::<Value>("accept-encoding")? != Value::Boolean(false) {
            return Ok(&self.client);
        }
        if let Some(plain) = self.plain.get() {
            return Ok(plain);
        }
        let plain = Client::build(self.connect_timeout, false)?;
        Ok(self.plain.get_or_init(|| plain))
    }
}

/// The longest any timeout or delay can be. `http.lua` refuses one that is not finite, and this
/// keeps one that is merely enormous from overflowing an `Instant`.
const LONGEST: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);

fn milliseconds(ms: f64) -> Duration {
    Duration::try_from_secs_f64(ms / 1000.0)
        .unwrap_or(LONGEST)
        .min(LONGEST)
}

impl UserData for Client {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("send", |lua, this, request: Table| async move {
            exchange(&lua, &this, &request).await
        });
    }
}

/// Sends `request` through `client`, trying again as its `retry` says, and gives back what the
/// last attempt came to: the response's parts, or a failure. Its total timeout covers every
/// attempt and every wait between them.
async fn exchange(lua: &Lua, client: &Client, request: &Table) -> mlua::Result<MultiValue> {
    let builder = prepare(lua, client.for_headers(&request.get("headers")?)?, request)?;
    let url = redacted_str(&request.get::<String>("url")?);
    let retry = Retry::from_lua(request.get("retry")?)?;
    let deadlines = Deadlines::new(request.get("idle_timeout")?, request.get("timeout")?);
    let mut attempt = 1;
    loop {
        // The retry, unless this is the last attempt it allows.
        let retrying = retry.as_ref().filter(|r| attempt < r.attempts);
        let builder = builder
            .try_clone()
            .expect("a request's body is bytes, so it can be sent again");
        let sent = match deadlines.next() {
            None => Ok(send(builder).await),
            Some((at, phase)) => tokio::time::timeout_at(at, send(builder))
                .await
                .map_err(|_| phase),
        };
        // How long to wait before trying again, unless this attempt's outcome is the answer.
        let delay = match sent {
            Ok(Ok(response)) => {
                let status = response.status().as_u16();
                match retrying.filter(|r| r.statuses.contains(&status)) {
                    Some(retry) => retry.delay(attempt, retry_after(response.headers())),
                    None => return respond(lua, request, response, deadlines).await,
                }
            }
            Ok(Err(e)) => {
                let failure = failed_request(e, url.clone(), client.connect_timeout);
                match retrying {
                    Some(retry) if matches!(failure.kind, Kind::Connect | Kind::Timeout) => {
                        retry.delay(attempt, None)
                    }
                    _ => return failure.into_lua(lua),
                }
            }
            Err(phase) => match retrying {
                Some(retry) => retry.delay(attempt, None),
                None => return timed_out(phase, &deadlines, &url, Waiting::Response).into_lua(lua),
            },
        };
        if deadlines.ends_within(delay) {
            return timed_out(Phase::Total, &deadlines, &url, Waiting::Response).into_lua(lua);
        }
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
}

/// What a response that came to stay gives back: its parts, or, under `check` and with a 4xx or
/// 5xx status, a `"status"` failure.
async fn respond(
    lua: &Lua,
    request: &Table,
    response: reqwest::Response,
    deadlines: Deadlines,
) -> mlua::Result<MultiValue> {
    let status = response.status().as_u16();
    let headers = response_headers(lua, response.headers())?;
    let url = response.url().to_string();
    let redacted = redacted(response.url());
    let body = ResponseReader::new(response, redacted.clone(), deadlines);
    if (400..600).contains(&status) && request.get::<bool>("check")? {
        return failed_status(lua, body, status, headers).await;
    }
    (true, body, status, url, headers, redacted).into_lua_multi(lua)
}

/// How to try a request again: up to `attempts` in all, after a failure to connect, a timeout
/// before a response, or a response with one of `statuses`, waiting longer each time.
struct Retry {
    attempts: u32,
    statuses: Vec<u16>,
    /// In milliseconds: the first wait, before jitter, and the longest.
    base_delay: f64,
    max_delay: f64,
}

impl Retry {
    /// A `retry` as `http.lua` resolves it, with every field given; nil for none.
    fn from_lua(retry: Option<Table>) -> mlua::Result<Option<Retry>> {
        let Some(retry) = retry else { return Ok(None) };
        Ok(Some(Retry {
            attempts: retry.get("attempts")?,
            statuses: retry.get("statuses")?,
            base_delay: retry.get("base_delay")?,
            max_delay: retry.get("max_delay")?,
        }))
    }

    /// How long to wait after attempt `attempt`, counting from 1: what a `Retry-After` asked
    /// for, or else the base delay doubled for each attempt before, with up to half of it taken
    /// off at random so that clients turned away together do not all come back together. Never
    /// longer than the maximum.
    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let max = milliseconds(self.max_delay);
        if let Some(asked) = retry_after {
            return asked.min(max);
        }
        let backoff = milliseconds(self.base_delay * 2f64.powi(attempt as i32 - 1)).min(max);
        backoff.mul_f64(1.0 - random() / 2.0)
    }
}

/// A number from 0 up to 1, different each time: enough for jitter, and no more.
fn random() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.finish() as f64 / u64::MAX as f64
}

/// How long a response's `Retry-After` asks to wait, given as seconds or as a date.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(
        at.duration_since(std::time::SystemTime::now())
            .unwrap_or_default(),
    )
}

/// The runtime reqwest's connections live on: one thread of its own, for every runtime in the
/// process. reqwest runs each connection as a task on the runtime that opened it, and keeps it
/// alive while the connection waits in the pool to be reused. On a runtime's own executor that
/// would make a pooled connection count as a task, so `avarice` would wait for it before exiting,
/// and a connection Lua had let go of would close only when something next drove the executor.
/// Here, neither: a request is not a task (ADR 0025).
fn connections() -> &'static tokio::runtime::Handle {
    static CONNECTIONS: OnceLock<tokio::runtime::Handle> = OnceLock::new();
    CONNECTIONS.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("http: could not start the runtime for connections");
        let handle = runtime.handle().clone();
        std::thread::Builder::new()
            .name("avarice-http".into())
            .spawn(move || runtime.block_on(std::future::pending::<()>()))
            .expect("http: could not start the thread for connections");
        handle
    })
}

/// Sends a request from the connections' runtime, so that the connection it opens lives there.
/// Dropping the future, as an aborted task or a cancel does, stops the request.
async fn send(builder: reqwest::RequestBuilder) -> reqwest::Result<reqwest::Response> {
    struct AbortOnDrop(tokio::task::AbortHandle);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let task = connections().spawn(builder.send());
    let _abort = AbortOnDrop(task.abort_handle());
    match task.await {
        Ok(result) => result,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Which timeout ran out.
#[derive(Clone, Copy, Debug)]
enum Phase {
    Connect,
    Idle,
    Total,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Phase::Connect => "connect",
            Phase::Idle => "idle",
            Phase::Total => "total",
        }
    }
}

/// A request's idle and total timeouts, in milliseconds, and the moment the total one runs out.
#[derive(Clone, Copy)]
struct Deadlines {
    idle: Option<f64>,
    total: Option<f64>,
    end: Option<Instant>,
}

impl Deadlines {
    /// Deadlines starting now.
    fn new(idle: Option<f64>, total: Option<f64>) -> Deadlines {
        Deadlines {
            idle,
            total,
            end: total.map(|ms| Instant::now() + milliseconds(ms)),
        }
    }

    /// The moment to give up waiting for what comes next, if there is one, and which timeout
    /// that would be: the sooner of the idle timeout from now and the end.
    fn next(&self) -> Option<(Instant, Phase)> {
        let idle = self
            .idle
            .map(|ms| (Instant::now() + milliseconds(ms), Phase::Idle));
        let end = self.end.map(|end| (end, Phase::Total));
        match (idle, end) {
            (Some(idle), Some(end)) => Some(if end.0 <= idle.0 { end } else { idle }),
            (idle, end) => idle.or(end),
        }
    }

    /// Whether the total timeout runs out within `delay` from now.
    fn ends_within(&self, delay: Duration) -> bool {
        self.end.is_some_and(|end| Instant::now() + delay >= end)
    }
}

/// The error a body's stream gives when a timeout runs out while reading it.
#[derive(Debug)]
struct TimedOut(Phase);

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("timed out")
    }
}

impl std::error::Error for TimedOut {}

/// What a request was waiting for when a timeout ran out.
enum Waiting {
    /// The response's headers.
    Response,
    /// More of its body.
    More,
}

fn timed_out(phase: Phase, deadlines: &Deadlines, url: &str, waiting: Waiting) -> Failure {
    let waiting = match waiting {
        Waiting::Response => "a response from",
        Waiting::More => "more of the response from",
    };
    let message = match phase {
        Phase::Total => format!(
            "the request to {url} took longer than its timeout of {} ms",
            deadlines.total.unwrap_or_default()
        ),
        Phase::Idle | Phase::Connect => format!(
            "waited longer than the idle_timeout of {} ms for {waiting} {url}",
            deadlines.idle.unwrap_or_default()
        ),
    };
    Failure {
        kind: Kind::Timeout,
        phase: Some(phase),
        url: url.to_owned(),
        message,
    }
}

/// What a request says it is, unless it says otherwise.
const USER_AGENT: &str = concat!("avarice/", env!("CARGO_PKG_VERSION"));

/// A request as `http.lua` hands it over, already checked and merged: `url`, `method` in upper
/// case, `headers` keyed by lower-case name, each a string or `false` for one removed, and at most
/// one of `body`, a string, and `json`, any value; `check`, `retry`, and the timeouts, which
/// [`Deadlines`] take. Only what reqwest can still refuse is checked here.
fn prepare(
    lua: &Lua,
    client: &reqwest::Client,
    request: &Table,
) -> mlua::Result<reqwest::RequestBuilder> {
    let url: String = request.get("url")?;
    let method: String = request.get("method")?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| mlua::Error::runtime(format!("http: {method:?} is not a method")))?;
    let mut builder = client.request(method, &url);
    let headers: Table = request.get("headers")?;
    if headers.get::<Value>("user-agent")?.is_nil() {
        builder = builder.header(reqwest::header::USER_AGENT, USER_AGENT);
    }
    for pair in headers.pairs::<LuaString, Value>() {
        let (name, value) = pair?;
        // `false` is a header removed, which is only not sent.
        let Value::String(value) = value else {
            continue;
        };
        let name = reqwest::header::HeaderName::from_bytes(&name.as_bytes()).map_err(|_| {
            mlua::Error::runtime(format!("http: {} is not a header name", name.display()))
        })?;
        let value = reqwest::header::HeaderValue::from_bytes(&value.as_bytes()).map_err(|_| {
            mlua::Error::runtime(format!(
                "http: the value of the header {name} cannot be sent"
            ))
        })?;
        builder = builder.header(name, value);
    }
    if let Some(body) = request.get::<Option<LuaString>>("body")? {
        builder = builder.body(body.as_bytes().to_vec());
    }
    let json: Value = request.get("json")?;
    if !json.is_nil() {
        builder = builder.body(encode_json(lua, json)?);
    }
    Ok(builder)
}

/// A response's headers for Lua: keyed by lower-case name, each value its exact bytes, and the
/// values of a header sent more than once joined with `", "`, as RFC 9110 allows.
fn response_headers(lua: &Lua, headers: &reqwest::header::HeaderMap) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for name in headers.keys() {
        let mut joined = Vec::new();
        for value in headers.get_all(name) {
            if !joined.is_empty() {
                joined.extend_from_slice(b", ");
            }
            joined.extend_from_slice(value.as_bytes());
        }
        table.set(name.as_str(), lua.create_string(joined)?)?;
    }
    Ok(table)
}

/// A URL as an error or `tostring` shows it: without its query, its fragment or its userinfo,
/// where some APIs put keys.
fn redacted(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.to_string()
}

fn redacted_str(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(url) => redacted(&url),
        Err(_) => "a URL that does not parse".to_owned(),
    }
}

/// What kind of failure an error is, as its `kind` field names it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Connect,
    Tls,
    Timeout,
    Redirect,
    Status,
    Body,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Connect => "connect",
            Kind::Tls => "tls",
            Kind::Timeout => "timeout",
            Kind::Redirect => "redirect",
            Kind::Status => "status",
            Kind::Body => "body",
        }
    }
}

/// A failure that `http.lua` raises as a table.
struct Failure {
    kind: Kind,
    /// Which timeout ran out, for a timeout.
    phase: Option<Phase>,
    /// The URL, [`redacted`].
    url: String,
    /// What went wrong, after `http: `.
    message: String,
}

impl Failure {
    /// What a primitive returns for it: `false`, the table's fields, and its message.
    fn into_lua(self, lua: &Lua) -> mlua::Result<MultiValue> {
        let (fields, message) = self.fields(lua)?;
        (false, fields, message).into_lua_multi(lua)
    }

    /// The table's fields, and its message.
    fn fields(self, lua: &Lua) -> mlua::Result<(Table, String)> {
        let fields = lua.create_table()?;
        fields.set("kind", self.kind.name())?;
        fields.set("phase", self.phase.map(Phase::name))?;
        fields.set("url", self.url)?;
        let message = format!("http: {}", self.message);
        fields.set("message", message.as_str())?;
        Ok((fields, message))
    }
}

/// The most of an error status's body a `"status"` error holds.
const STATUS_BODY: usize = 64 * 1024;

/// The most of an error status's body its message shows.
const STATUS_MESSAGE_BODY: usize = 200;

/// The `"status"` error for a response with an error status, under `check`: its status, its
/// headers and the start of its body, up to [`STATUS_BODY`], which is read and then closed.
/// Whatever stops the body being read stops it there, and the error holds what was read.
async fn failed_status(
    lua: &Lua,
    body: ResponseReader,
    status: u16,
    headers: Table,
) -> mlua::Result<MultiValue> {
    let mut read = Vec::new();
    while read.len() < STATUS_BODY {
        match body.reader.read().await? {
            Ok(Some(chunk)) => read.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    body.reader.close().await;
    read.truncate(STATUS_BODY);

    let reason = reqwest::StatusCode::from_u16(status)
        .ok()
        .and_then(|s| s.canonical_reason())
        .map(|r| format!(" {r}"))
        .unwrap_or_default();
    let mut message = format!("{status}{reason} from {}", body.url);
    if !read.is_empty() {
        let start = &read[..read.len().min(STATUS_MESSAGE_BODY)];
        let more = if start.len() < read.len() { "..." } else { "" };
        // A message is for reading, so the start of the body is shown as text, invalid UTF-8
        // replaced. Its exact bytes are the error's `body` (ADR 0015).
        message += &format!(": {}{more}", String::from_utf8_lossy(start));
    }
    let (fields, message) = Failure {
        kind: Kind::Status,
        phase: None,
        url: body.url.clone(),
        message,
    }
    .fields(lua)?;
    fields.set("status", status)?;
    fields.set("headers", headers)?;
    fields.set("body", AstraBuffer::new(read.into()))?;
    (false, fields, message).into_lua_multi(lua)
}

/// `error` and each error that caused it, in turn. An `io::Error` wrapping another is followed
/// into the one it wraps, which its own `source` skips.
fn causes<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> impl Iterator<Item = &'a (dyn std::error::Error + 'static)> {
    std::iter::successors(Some(error), |error| {
        match error
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
        {
            Some(inner) => Some(inner as &(dyn std::error::Error + 'static)),
            None => error.source(),
        }
    })
}

/// `error` and each error that caused it, as one line, leaving out any whose words the line
/// already has.
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut parts: Vec<String> = Vec::new();
    for error in causes(error) {
        let part = error.to_string();
        if !parts.iter().any(|p| p.contains(&part)) {
            parts.push(part);
        }
    }
    parts.join(": ")
}

/// Whether TLS is what failed: whether rustls made any error in the chain.
fn is_tls(error: &(dyn std::error::Error + 'static)) -> bool {
    causes(error).any(|error| error.is::<rustls::Error>())
}

/// What a request that got no response failed with, through a Client whose connect timeout is
/// `connect_timeout` milliseconds.
fn failed_request(error: reqwest::Error, url: String, connect_timeout: Option<f64>) -> Failure {
    let error = error.without_url();
    if error.is_timeout() {
        return Failure {
            kind: Kind::Timeout,
            phase: Some(Phase::Connect),
            message: format!(
                "could not connect to {url} within the connect_timeout of {} ms",
                connect_timeout.unwrap_or_default()
            ),
            url,
        };
    }
    let (kind, what) = if error.is_redirect() {
        (Kind::Redirect, "too many redirects from")
    } else if is_tls(&error) {
        (Kind::Tls, "could not make a secure connection to")
    } else if error.is_body() || error.is_decode() {
        (Kind::Body, "could not read the response from")
    } else {
        (Kind::Connect, "could not connect to")
    };
    Failure {
        kind,
        phase: None,
        message: format!("{what} {url}: {}", chain(&error)),
        url,
    }
}

/// A response's body, as Rust holds it: a Reader over what reqwest receives, the URL it came
/// from, [`redacted`], and the request's deadlines, for errors.
struct ResponseReader {
    reader: Reader,
    url: String,
    deadlines: Deadlines,
}

impl ResponseReader {
    /// A Reader that gives up waiting for more when `deadlines` say to.
    fn new(response: reqwest::Response, url: String, deadlines: Deadlines) -> ResponseReader {
        use futures_util::StreamExt;
        let chunks =
            futures_util::stream::unfold(Some(response.bytes_stream()), move |chunks| async move {
                let mut chunks = chunks?;
                let next = match deadlines.next() {
                    None => Ok(chunks.next().await),
                    Some((at, phase)) => tokio::time::timeout_at(at, chunks.next())
                        .await
                        .map_err(|_| phase),
                };
                match next {
                    Ok(None) => None,
                    Ok(Some(Ok(chunk))) => Some((Ok(chunk), Some(chunks))),
                    // Nothing more is read after a failure.
                    Ok(Some(Err(e))) => Some((Err(std::io::Error::other(e.without_url())), None)),
                    Err(phase) => Some((Err(std::io::Error::other(TimedOut(phase))), None)),
                }
            });
        let stream = buffered(Box::new(tokio_util::io::StreamReader::new(chunks.boxed())));
        ResponseReader {
            reader: Reader::new(stream, "http", "the response's body"),
            url,
            deadlines,
        }
    }

    /// What a `read` or a `line` gives back: the bytes, nil at the end, or a failure.
    async fn piece(
        &self,
        lua: &Lua,
        read: std::io::Result<Option<Vec<u8>>>,
    ) -> mlua::Result<MultiValue> {
        match read {
            Ok(piece) => {
                (true, piece.map(|p| lua.create_string(p)).transpose()?).into_lua_multi(lua)
            }
            Err(e) => self.failed(e).await.into_lua(lua),
        }
    }

    /// What reading it failed with. It is closed, since nothing more can be read from it.
    async fn failed(&self, error: std::io::Error) -> Failure {
        self.reader.close().await;
        if let Some(TimedOut(phase)) = causes(&error).find_map(|e| e.downcast_ref::<TimedOut>()) {
            return timed_out(*phase, &self.deadlines, &self.url, Waiting::More);
        }
        Failure {
            kind: Kind::Body,
            phase: None,
            url: self.url.clone(),
            message: format!(
                "could not read the response from {}: {}",
                self.url,
                chain(&error)
            ),
        }
    }
}

impl UserData for ResponseReader {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("read", |lua, this, ()| async move {
            let read = this.reader.read().await?;
            this.piece(&lua, read).await
        });
        methods.add_async_method("line", |lua, this, ()| async move {
            let line = this.reader.line().await?;
            this.piece(&lua, line).await
        });
        methods.add_async_method("rest", |lua, this, ()| async move {
            match this.reader.rest().await? {
                Ok(rest) => (true, AstraBuffer::new(rest.into())).into_lua_multi(&lua),
                Err(e) => this.failed(e).await.into_lua(&lua),
            }
        });
        methods.add_async_method("close", |_, this, ()| async move {
            this.reader.close().await;
            Ok(())
        });
    }
}
