//! The bundled `openai` module (ADR 0026), driven the way a script does, against a server on a
//! port of its own that answers one request with what each test gives it.
//!
//! The environment is never touched: each test that is about it replaces `os.getenv` in its own
//! runtime, which the module reads through.

#![cfg(feature = "bundled-openai")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use avarice::bundled::OpenAi;
use avarice::{Profile, Runtime, StdModules};

/// A request as the server received it.
#[derive(Debug)]
struct Request {
    /// The request line, such as `POST /v1/chat/completions HTTP/1.1`.
    line: String,
    /// The head, every header name lower-cased.
    head: String,
    body: serde_json::Value,
}

impl Request {
    fn header(&self, name: &str) -> Option<String> {
        self.head
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}: ")))
            .map(str::to_owned)
    }
}

/// Reads one request from `stream`, head and body.
fn read_request(stream: &mut TcpStream) -> Request {
    let mut seen = Vec::new();
    let mut chunk = [0; 4096];
    let head_end = loop {
        if let Some(i) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "the client closed before its request ended");
        seen.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&seen[..head_end]).into_owned();
    let head = head
        .lines()
        .map(|l| match l.split_once(": ") {
            Some((name, value)) => format!("{}: {value}", name.to_ascii_lowercase()),
            None => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length: "))
        .map_or(0, |v| v.trim().parse().unwrap());
    while seen.len() < head_end + length {
        let n = stream.read(&mut chunk).unwrap();
        seen.extend_from_slice(&chunk[..n]);
    }
    let body = serde_json::from_slice(&seen[head_end..head_end + length]).unwrap();
    let line = head.lines().next().unwrap().to_owned();
    Request { line, head, body }
}

/// A server that answers its first request with `response`, raw bytes after the status line's
/// headers, and hands that request back on the receiver. Its URL is the first value.
fn server(response: String) -> (String, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_request(&mut stream);
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
        tx.send(request).unwrap();
    });
    (url, rx)
}

/// A whole JSON response.
fn json_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A stream of server-sent events, one per item, then the end of the body.
fn event_stream(events: &[&str]) -> String {
    let body: String = events.iter().map(|e| format!("{e}\n\n")).collect();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A trusted runtime with `openai` added, `openai` required, and `os.getenv` answering from
/// `env` alone.
fn runtime(env: &[(&str, &str)]) -> Runtime {
    let rt = Runtime::builder(Profile::Trusted)
        .module(OpenAi)
        .build()
        .unwrap();
    let pairs: String = env
        .iter()
        .map(|(name, value)| format!("[{name:?}] = {value:?},"))
        .collect();
    rt.block_on(rt.exec(
        &format!(
            "local env = {{ {pairs} }}
             os.getenv = function(name) return env[name] end
             openai = require('openai')"
        ),
        "=prelude",
    ))
    .unwrap();
    rt
}

fn eval<R: avarice::mlua::FromLuaMulti>(rt: &Runtime, source: &str) -> R {
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{e}"))
}

fn error_of(rt: &Runtime, source: &str) -> String {
    match rt.block_on(rt.exec(source, "=test")) {
        Ok(()) => panic!("expected an error from {source}"),
        Err(e) => e.to_string(),
    }
}

const CHUNK_HE: &str =
    r#"data: {"choices":[{"index":0,"delta":{"role":"assistant","content":"He"}}]}"#;
const CHUNK_LLO: &str = r#"data: {"choices":[{"index":0,"delta":{"content":"llo"}}]}"#;

// -- Streaming ---------------------------------------------------------------------------------

#[test]
fn stream_yields_each_delta_until_done() {
    let (url, rx) = server(event_stream(&[
        CHUNK_HE,
        ": a comment",
        CHUNK_LLO,
        r#"data: {"choices":[],"usage":{"total_tokens":7}}"#,
        "data: [DONE]",
    ]));
    let rt = runtime(&[]);
    let (text, tokens): (String, i64) = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{
                 base_url = "{url}/v1", api_key = "sk-test", model = "m",
               }}
               local parts, tokens = {{}}, nil
               local request = {{ messages = {{ {{ role = "user", content = "Hi" }} }} }}
               for delta, chunk in ai:stream(request) do
                 parts[#parts + 1] = delta.content or ""
                 if chunk.usage then tokens = chunk.usage.total_tokens end
               end
               return table.concat(parts), tokens"#
        ),
    );
    assert_eq!((text.as_str(), tokens), ("Hello", 7));

    let request = rx.recv().unwrap();
    assert_eq!(request.line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer sk-test")
    );
    assert_eq!(
        request.header("accept").as_deref(),
        Some("text/event-stream")
    );
    assert_eq!(request.body["stream"], true);
    assert_eq!(request.body["model"], "m");
    assert_eq!(request.body["messages"][0]["content"], "Hi");
}

#[test]
fn a_stream_that_ends_without_done_ends_the_loop() {
    let (url, _rx) = server(event_stream(&[CHUNK_HE]));
    let rt = runtime(&[]);
    let count: i64 = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}", model = "m" }}
               local n = 0
               for _ in ai:stream {{ messages = {{}} }} do n = n + 1 end
               return n"#
        ),
    );
    assert_eq!(count, 1);
}

#[test]
fn an_error_sent_mid_stream_raises() {
    let (url, _rx) = server(event_stream(&[
        CHUNK_HE,
        r#"data: {"error":{"message":"overloaded","type":"server_error"}}"#,
    ]));
    let rt = runtime(&[]);
    let (kind, message, error_type, seen): (String, String, String, String) = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}", model = "m" }}
               local seen = ""
               local ok, err = pcall(function()
                 for delta in ai:stream {{ messages = {{}} }} do seen = seen .. delta.content end
               end)
               assert(not ok)
               return err.kind, tostring(err), err.error.type, seen"#
        ),
    );
    assert_eq!(kind, "api");
    assert_eq!(message, "openai: overloaded");
    assert_eq!(error_type, "server_error");
    assert_eq!(seen, "He");
}

#[test]
fn leaving_a_stream_early_closes_the_connection() {
    // The server sends one chunk and then holds the stream open, so the connection closes only if
    // the script's `break` closes it.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        let event = format!("{CHUNK_HE}\n\n");
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                 Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{event}\r\n",
                event.len()
            )
            .as_bytes(),
        );
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let closed = match stream.read(&mut [0; 1]) {
            Ok(0) => true,
            Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
            Ok(_) => false,
        };
        tx.send(closed).unwrap();
    });
    let rt = runtime(&[]);
    let first: String = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}", model = "m" }}
               for delta in ai:stream {{ messages = {{}} }} do return delta.content end"#
        ),
    );
    assert_eq!(first, "He");
    assert!(
        rx.recv().unwrap(),
        "the connection was still open 5s after the loop was left"
    );
}

#[test]
fn an_error_event_that_is_not_json_raises_with_what_was_sent() {
    let (url, _rx) = server(event_stream(&["event: error\ndata: upstream went away"]));
    let rt = runtime(&[]);
    let (kind, message): (String, String) = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}", model = "m" }}
               local ok, err = pcall(function() for _ in ai:stream {{ messages = {{}} }} do end end)
               assert(not ok)
               return err.kind, tostring(err)"#
        ),
    );
    assert_eq!(kind, "api");
    assert_eq!(message, "openai: upstream went away");
}

#[test]
fn data_that_is_not_a_chunk_raises() {
    for data in ["not json", "42"] {
        let (url, _rx) = server(event_stream(&[&format!("data: {data}")]));
        let rt = runtime(&[]);
        let (kind, message): (String, String) = eval(
            &rt,
            &format!(
                r#"local ai = openai.client {{ base_url = "{url}", model = "m" }}
                   local ok, err = pcall(function()
                     for _ in ai:stream {{ messages = {{}} }} do end
                   end)
                   assert(not ok)
                   return err.kind, tostring(err)"#
            ),
        );
        assert_eq!(kind, "api", "{data}");
        assert!(
            message.contains("which is not a chunk"),
            "{data}: {message}"
        );
        assert!(message.contains(data), "{data}: {message}");
    }
}

// -- Whole completions and other endpoints -----------------------------------------------------

#[test]
fn chat_gives_the_decoded_response_and_a_requests_model_wins() {
    let (url, rx) = server(json_response(
        r#"{"choices":[{"message":{"role":"assistant","content":"Hi there"}}]}"#,
    ));
    let rt = runtime(&[]);
    let content: String = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}", model = "client-model" }}
               local request = {{ model = "request-model", messages = {{}} }}
               local res = ai:chat(request)
               assert(request.stream == nil, "the caller's table is left as it was")
               return res.choices[1].message.content"#
        ),
    );
    assert_eq!(content, "Hi there");
    let request = rx.recv().unwrap();
    assert_eq!(request.body["model"], "request-model");
    assert!(request.body.get("stream").is_none());
}

#[test]
fn post_sends_to_any_path() {
    let (url, rx) = server(json_response(r#"{"data":[{"embedding":[0.5]}]}"#));
    let rt = runtime(&[]);
    let value: f64 = eval(
        &rt,
        &format!(
            r#"local ai = openai.client {{ base_url = "{url}/v1" }}
               return ai:post("/embeddings", {{ model = "e", input = "x" }}).data[1].embedding[1]"#
        ),
    );
    assert_eq!(value, 0.5);
    assert_eq!(rx.recv().unwrap().line, "POST /v1/embeddings HTTP/1.1");
}

// -- The environment ---------------------------------------------------------------------------

#[test]
fn what_a_client_is_not_given_comes_from_the_environment() {
    let (url, rx) = server(json_response(r#"{"choices":[]}"#));
    let rt = runtime(&[
        ("OPENAI_BASE_URL", &format!("{url}/env")),
        ("OPENAI_API_KEY", "sk-env"),
        ("OPENAI_MODEL", "env-model"),
    ]);
    eval::<()>(&rt, "openai.chat { messages = {} }");
    let request = rx.recv().unwrap();
    assert_eq!(request.line, "POST /env/chat/completions HTTP/1.1");
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer sk-env")
    );
    assert_eq!(request.body["model"], "env-model");
}

#[test]
fn what_a_client_is_given_wins_over_the_environment() {
    let (url, rx) = server(json_response(r#"{"choices":[]}"#));
    let rt = runtime(&[
        ("OPENAI_BASE_URL", "http://127.0.0.1:1/unused"),
        ("OPENAI_API_KEY", "sk-env"),
        ("OPENAI_MODEL", "env-model"),
    ]);
    eval::<()>(
        &rt,
        &format!(
            r#"openai.client {{ base_url = "{url}", api_key = "sk-given", model = "given" }}
                 :chat {{ messages = {{}} }}"#
        ),
    );
    let request = rx.recv().unwrap();
    assert_eq!(request.line, "POST /chat/completions HTTP/1.1");
    assert_eq!(
        request.header("authorization").as_deref(),
        Some("Bearer sk-given")
    );
    assert_eq!(request.body["model"], "given");
}

#[test]
fn api_key_false_sends_no_authorization_whatever_the_environment() {
    let (url, rx) = server(json_response(r#"{"choices":[]}"#));
    let rt = runtime(&[("OPENAI_API_KEY", "sk-env")]);
    eval::<()>(
        &rt,
        &format!(
            r#"openai.client {{ base_url = "{url}", api_key = false, model = "m" }}
                 :chat {{ messages = {{}} }}"#
        ),
    );
    assert_eq!(rx.recv().unwrap().header("authorization"), None);
}

#[test]
fn an_empty_variable_counts_as_unset() {
    let rt = runtime(&[("OPENAI_BASE_URL", ""), ("OPENAI_MODEL", "")]);
    let shown: String = eval(&rt, "return tostring(openai.client())");
    assert_eq!(shown, "openai.Client(https://api.openai.com/v1)");
    let message = error_of(&rt, "openai.client():chat { messages = {} }");
    assert!(message.contains("chat needs a model"), "{message}");
}

// -- What it refuses ---------------------------------------------------------------------------

#[test]
fn an_unknown_option_is_refused() {
    let rt = runtime(&[]);
    let message = error_of(&rt, "openai.client { api_token = 'x' }");
    assert!(
        message.contains("a Client has no option api_token"),
        "{message}"
    );
}

#[test]
fn a_request_that_gives_stream_is_refused() {
    let rt = runtime(&[]);
    let message = error_of(
        &rt,
        "openai.client { model = 'm' }:chat { stream = true, messages = {} }",
    );
    assert!(
        message.contains("chat sets the request's stream itself"),
        "{message}"
    );
    let message = error_of(
        &rt,
        "openai.client { model = 'm' }:stream { stream = false, messages = {} }",
    );
    assert!(
        message.contains("stream sets the request's stream itself"),
        "{message}"
    );
}

#[test]
fn a_model_must_be_a_string_that_is_not_empty() {
    let rt = runtime(&[]);
    let message = error_of(&rt, "openai.client():chat { model = 4, messages = {} }");
    assert!(
        message.contains("a request's model is a string"),
        "{message}"
    );
    let message = error_of(&rt, "openai.client { model = '' }");
    assert!(
        message.contains("a Client's model is a string that is not empty"),
        "{message}"
    );
}

#[test]
fn an_empty_option_is_refused_rather_than_sent() {
    let rt = runtime(&[]);
    for option in ["base_url", "api_key"] {
        let message = error_of(&rt, &format!("openai.client {{ {option} = '' }}"));
        assert!(
            message.contains(&format!("a Client's {option}")),
            "{option}: {message}"
        );
    }
}

#[test]
fn requiring_it_without_http_says_so() {
    let rt = Runtime::builder(Profile::Trusted)
        .without_std_modules(StdModules::HTTP)
        .module(OpenAi)
        .build()
        .unwrap();
    let message = error_of(&rt, "require('openai')");
    assert!(
        message.contains("openai: needs the http module"),
        "{message}"
    );
}

#[test]
fn requiring_it_without_os_says_so() {
    // A sandbox with the two modules it needs, but no `os`.
    let rt = Runtime::builder(Profile::Sandbox)
        .with_std_modules(StdModules::HTTP | StdModules::SERDE)
        .module(OpenAi)
        .build()
        .unwrap();
    let message = error_of(&rt, "require('openai')");
    assert!(
        message.contains("openai: needs the os library"),
        "{message}"
    );
}

#[test]
fn no_profile_registers_it() {
    for profile in [Profile::Trusted, Profile::Sandbox] {
        let rt = Runtime::new(profile).unwrap();
        let message = error_of(&rt, "require('openai')");
        assert!(message.contains("module 'openai' not found"), "{message}");
    }
}
