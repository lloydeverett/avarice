//! `http` is original to avarice, not derived from Astra: ADR 0025 records why. A **Client** sends
//! requests and keeps its connections to reuse; a response comes back once its headers have
//! arrived, and its body is read as it arrives.
//!
//! This is the half of the module that touches the network. `http.lua` is the rest: it checks
//! and merges options, joins URLs, and gives Lua the Client, the response and its **Reader**.
//! What fails here is not raised: the primitives return `false`, the error's fields and its
//! message, and `http.lua` raises them as a table, as `process` does.

use std::sync::OnceLock;

use mlua::{IntoLuaMulti, Lua, LuaString, MultiValue, Table, UserData, UserDataMethods, Value};

use super::AstraBuffer;
use super::astra_serde::encode_json;
use super::reader::{Reader, buffered};

pub fn register_to_lua(lua: &Lua) -> mlua::Result<()> {
    lua.globals().set(
        "astra_internal__http_client",
        lua.create_function(|_, ()| Client::new())?,
    )
}

/// A Client, as Rust holds it: one reqwest client, whose connections every request through it
/// shares.
#[derive(Clone)]
struct Client(reqwest::Client);

impl Client {
    fn new() -> mlua::Result<Client> {
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| mlua::Error::runtime(format!("http: could not make a Client: {e}")))?;
        Ok(Client(client))
    }
}

impl UserData for Client {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("send", |lua, this, request: Table| async move {
            let builder = prepare(&lua, &this.0, &request)?;
            let response = match send(builder).await {
                Ok(response) => response,
                Err(e) => return failure(&lua, "connect", &e.to_string()),
            };
            let headers = response_headers(&lua, response.headers())?;
            let status = response.status().as_u16();
            let url = response.url().to_string();
            let body = Body::new(response);
            (true, body, status, url, headers).into_lua_multi(&lua)
        });
    }
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

/// What a request says it is, unless it says otherwise.
const USER_AGENT: &str = concat!("avarice/", env!("CARGO_PKG_VERSION"));

/// A request as `http.lua` hands it over, already checked and merged: `url`, `method` in upper
/// case, `headers` keyed by lower-case name, each a string or `false` for one removed, and at most one of `body`, a string, and `json`, any
/// value. Only what reqwest can still refuse is checked here.
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

/// What a primitive returns for a failure that `http.lua` raises as a table: `false`, the
/// table's fields, and its message.
fn failure(lua: &Lua, kind: &str, message: &str) -> mlua::Result<MultiValue> {
    let fields = lua.create_table()?;
    fields.set("kind", kind)?;
    let message = format!("http: {message}");
    fields.set("message", message.as_str())?;
    (false, fields, message).into_lua_multi(lua)
}

/// A response's body, as Rust holds it: a Reader over what reqwest receives.
struct Body(Reader);

impl Body {
    fn new(response: reqwest::Response) -> Body {
        use futures_util::TryStreamExt;
        let stream = response.bytes_stream().map_err(std::io::Error::other);
        let stream = buffered(Box::new(tokio_util::io::StreamReader::new(stream)));
        Body(Reader::new(stream, "http", "the response's body"))
    }
}

impl UserData for Body {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("read", |lua, this, ()| async move {
            match this.0.read().await? {
                Ok(chunk) => {
                    (true, chunk.map(|c| lua.create_string(c)).transpose()?).into_lua_multi(&lua)
                }
                Err(e) => failure(&lua, "body", &e.to_string()),
            }
        });
        methods.add_async_method("line", |lua, this, ()| async move {
            match this.0.line().await? {
                Ok(line) => {
                    (true, line.map(|l| lua.create_string(l)).transpose()?).into_lua_multi(&lua)
                }
                Err(e) => failure(&lua, "body", &e.to_string()),
            }
        });
        methods.add_async_method("rest", |lua, this, ()| async move {
            match this.0.rest().await? {
                Ok(rest) => (true, AstraBuffer::new(rest.into())).into_lua_multi(&lua),
                Err(e) => failure(&lua, "body", &e.to_string()),
            }
        });
        methods.add_async_method("close", |_, this, ()| async move {
            this.0.close().await;
            Ok(())
        });
    }
}
