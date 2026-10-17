//! The `http` module (ADR 0025), driven the way a script does: Lua through a trusted runtime,
//! against a server on a port of its own that each test scripts byte by byte.
//!
//! The server is raw TCP rather than an HTTP server, because what matters most here is what an
//! HTTP server would not do on purpose: stall, cut a body short, or answer with something that is
//! not HTTP at all.

#![cfg(feature = "stdlib-http")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use avarice::{Profile, Runtime};

/// A trusted runtime with `http` required.
fn runtime() -> Runtime {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    rt.block_on(rt.exec(r#"http = require("http")"#, "=prelude"))
        .unwrap();
    rt
}

fn eval<R: avarice::mlua::FromLuaMulti>(rt: &Runtime, source: &str) -> R {
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{e}"))
}

/// The message of the error `source` raises, which must be one.
fn error_of(rt: &Runtime, source: &str) -> String {
    match rt.block_on(rt.exec(source, "=test")) {
        Ok(()) => panic!("expected an error from {source}"),
        Err(e) => e.to_string(),
    }
}

/// A request as the server received it.
#[derive(Clone, Debug)]
struct Request {
    /// The request line, such as `GET /x HTTP/1.1`.
    line: String,
    /// Each header as sent, its name lower-cased.
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

impl Request {
    /// The value of every header named `name`, which is lower case.
    fn all(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| String::from_utf8_lossy(v).into_owned())
            .collect()
    }

    /// The one value of the header named `name`, or `None` if it was not sent. Panics if it was
    /// sent more than once.
    fn header(&self, name: &str) -> Option<String> {
        let all = self.all(name);
        assert!(
            all.len() <= 1,
            "{name} was sent {} times: {all:?}",
            all.len()
        );
        all.into_iter().next()
    }

    /// The target of the request line, such as `/x?y=1`.
    fn target(&self) -> &str {
        self.line.split(' ').nth(1).unwrap()
    }
}

/// One accepted connection, as a test's script sees it.
struct Connection {
    stream: TcpStream,
    seen: Vec<u8>,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Connection {
    /// Reads the next request, head and body, or `None` if the client closed the connection
    /// first.
    fn request(&mut self) -> Option<Request> {
        let head_end = loop {
            if let Some(i) = self.seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if !self.fill() {
                return None;
            }
        };
        let head = String::from_utf8_lossy(&self.seen[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let line = lines.next().unwrap().to_owned();
        let headers: Vec<(String, Vec<u8>)> = self.seen[..head_end]
            .split(|&b| b == b'\n')
            .skip(1)
            .filter_map(|raw| {
                let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
                let colon = raw.iter().position(|&b| b == b':')?;
                let name = String::from_utf8_lossy(&raw[..colon]).to_ascii_lowercase();
                let value = raw[colon + 1..].trim_ascii().to_vec();
                Some((name, value))
            })
            .collect();
        let length: usize = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .map_or(0, |(_, v)| String::from_utf8_lossy(v).parse().unwrap());
        while self.seen.len() < head_end + length {
            assert!(
                self.fill(),
                "the connection closed before the request's body ended"
            );
        }
        let body = self.seen[head_end..head_end + length].to_vec();
        self.seen.drain(..head_end + length);
        let request = Request {
            line,
            headers,
            body,
        };
        self.requests.lock().unwrap().push(request.clone());
        Some(request)
    }

    /// Reads more of what the client sent. False once it has closed the connection.
    fn fill(&mut self) -> bool {
        let mut chunk = [0; 4096];
        match self.stream.read(&mut chunk) {
            Ok(0) | Err(_) => false,
            Ok(n) => {
                self.seen.extend_from_slice(&chunk[..n]);
                true
            }
        }
    }

    /// Writes `bytes` to the client, ignoring a client that has gone.
    fn send(&mut self, bytes: impl AsRef<[u8]>) {
        let _ = self.stream.write_all(bytes.as_ref());
        let _ = self.stream.flush();
    }

    /// Answers with `status` and `body`, keeping the connection open for another request.
    fn respond(&mut self, status: &str, headers: &[(&str, &str)], body: impl AsRef<[u8]>) {
        let body = body.as_ref();
        let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
        for (name, value) in headers {
            head += &format!("{name}: {value}\r\n");
        }
        head += "\r\n";
        self.send(head);
        self.send(body);
    }

    /// Starts a `200 OK` response whose body is sent in chunks, with `headers`.
    fn start_chunked(&mut self, headers: &[(&str, &str)]) {
        let mut head = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n".to_owned();
        for (name, value) in headers {
            head += &format!("{name}: {value}\r\n");
        }
        self.send(head + "\r\n");
    }

    /// Sends `bytes` as one chunk of a chunked body.
    fn chunk(&mut self, bytes: impl AsRef<[u8]>) {
        let bytes = bytes.as_ref();
        self.send(format!("{:x}\r\n", bytes.len()));
        self.send(bytes);
        self.send("\r\n");
    }

    /// Whether the client closes the connection within `within`, sending nothing more.
    fn closed_within(&mut self, within: Duration) -> bool {
        self.stream.set_read_timeout(Some(within)).unwrap();
        let mut chunk = [0; 1];
        let closed = matches!(self.stream.read(&mut chunk), Ok(0))
            || matches!(self.stream.read(&mut chunk), Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset);
        self.stream.set_read_timeout(None).unwrap();
        closed
    }
}

/// A server on a port of its own that runs `script` for each connection it accepts, each on a
/// thread of its own, for as long as the test runs.
struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Request>>>,
    connections: Arc<AtomicUsize>,
}

impl Server {
    fn new(script: impl Fn(Connection) + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let script = Arc::new(script);
        {
            let requests = requests.clone();
            let connections = connections.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { return };
                    connections.fetch_add(1, Ordering::SeqCst);
                    let script = script.clone();
                    let requests = requests.clone();
                    std::thread::spawn(move || {
                        script(Connection {
                            stream,
                            seen: Vec::new(),
                            requests,
                        })
                    });
                }
            });
        }
        Server {
            url,
            requests,
            connections,
        }
    }

    /// A server that answers every request on every connection with `status`, `headers` and
    /// `body`.
    fn answering(
        status: &'static str,
        headers: &'static [(&str, &str)],
        body: &'static [u8],
    ) -> Server {
        Server::new(move |mut c| {
            while c.request().is_some() {
                c.respond(status, headers, body);
            }
        })
    }

    /// Every request received so far.
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The only request received.
    fn request(&self) -> Request {
        let requests = self.requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.into_iter().next().unwrap()
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

// Sending a request and reading its response.

#[test]
fn a_get_gives_the_status_headers_url_and_body() {
    let server = Server::answering("200 OK", &[("X-Thing", "one")], b"hello");
    let rt = runtime();
    let (status, thing, url, body): (u16, String, String, String) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{}/where")
               return res.status, res.headers["x-thing"], res.url, res:bytes()"#,
            server.url
        ),
    );
    assert_eq!(
        (status, thing.as_str(), body.as_str()),
        (200, "one", "hello")
    );
    assert_eq!(url, format!("{}/where", server.url));
    let request = server.request();
    assert_eq!(request.line, "GET /where HTTP/1.1");
}

/// A server whose `/stream` sends `first`, then waits for a request to `/go` before sending
/// `rest`, or for five seconds if none comes. Gives the server and whether `/go` came in time,
/// which it only knows once `rest` is sent.
fn held_back(first: &'static str, rest: &'static str) -> (Server, std::sync::mpsc::Receiver<bool>) {
    let (go, released) = std::sync::mpsc::channel::<()>();
    let released = Arc::new(Mutex::new(released));
    let (sent, in_time) = std::sync::mpsc::channel();
    let server = Server::new(move |mut c| {
        while let Some(request) = c.request() {
            if request.target() == "/go" {
                let _ = go.send(());
                c.respond("200 OK", &[], "");
                continue;
            }
            let length = first.len() + rest.len();
            c.send(format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n{first}"
            ));
            let came = released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .is_ok();
            c.send(rest);
            let _ = sent.send(came);
        }
    });
    (server, in_time)
}

#[test]
fn a_line_is_read_as_soon_as_it_arrives() {
    let (server, in_time) = held_back("one\r\n", "two\nthree");
    let rt = runtime();
    let lines: Vec<String> = eval(
        &rt,
        &format!(
            r#"local res = http.get("{0}/stream")
               local first = res.body:line()
               http.get("{0}/go")
               local lines = {{ first }}
               for line in res.body:lines() do lines[#lines + 1] = line end
               return lines"#,
            server.url
        ),
    );
    assert!(
        in_time.recv().unwrap(),
        "the first line waited for the rest of the body"
    );
    assert_eq!(lines, ["one", "two", "three"]);
}

#[test]
fn read_gives_chunks_as_they_arrive_and_then_nil() {
    let (server, in_time) = held_back("abc", "def");
    let rt = runtime();
    let (first, rest, end): (String, String, Option<String>) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{0}/stream")
               local first = res.body:read()
               http.get("{0}/go")
               local rest = ""
               while #rest < 3 do rest = rest .. res.body:read() end
               return first, rest, res.body:read()"#,
            server.url
        ),
    );
    assert!(
        in_time.recv().unwrap(),
        "the first chunk waited for the rest of the body"
    );
    assert_eq!((first.as_str(), rest.as_str(), end), ("abc", "def", None));
}

#[test]
fn rest_gives_what_is_left_as_a_buffer() {
    let server = Server::answering("200 OK", &[], b"skip\n{\"a\": [1, 2]}");
    let rt = runtime();
    let (kind, length, sum): (String, i64, i64) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{0}")
               res.body:line()
               local rest = res.body:rest()
               local doc = rest:json()
               return type(rest), #rest, doc.a[1] + doc.a[2]"#,
            server.url
        ),
    );
    assert_eq!((kind.as_str(), length, sum), ("userdata", 13, 3));
}

#[test]
fn bytes_and_json_read_the_whole_body() {
    let server = Server::answering("200 OK", &[], b"{\"a\": [1, 2]}");
    let rt = runtime();
    let (bytes, sum): (String, i64) = eval(
        &rt,
        &format!(
            r#"local doc = http.get("{0}"):json()
               return http.get("{0}"):bytes(), doc.a[1] + doc.a[2]"#,
            server.url
        ),
    );
    assert_eq!((bytes.as_str(), sum), ("{\"a\": [1, 2]}", 3));
}

// What a request sends.

/// A Lua expression for a string of every byte value once, in order.
const ALL_BYTES: &str = "(function() local t = {} for i = 0, 255 do t[#t + 1] = string.char(i) end \
                         return table.concat(t) end)()";

#[test]
fn a_string_body_is_sent_byte_for_byte_with_no_content_type() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(r#"http.post("{}", {{ body = {ALL_BYTES} }})"#, server.url),
    );
    let request = server.request();
    assert_eq!(request.line, "POST / HTTP/1.1");
    assert_eq!(request.body, (0..=255u8).collect::<Vec<_>>());
    assert_eq!(request.header("content-type"), None);
}

#[test]
fn a_buffer_body_is_sent_byte_for_byte() {
    let echo = Server::new(|mut c| {
        while let Some(request) = c.request() {
            c.respond("200 OK", &[], request.body);
        }
    });
    let rt = runtime();
    let same: bool = eval(
        &rt,
        &format!(
            r#"local buffer = http.post("{0}", {{ body = {ALL_BYTES} }}).body:rest()
               return http.post("{0}", {{ body = buffer }}):bytes() == {ALL_BYTES}"#,
            echo.url
        ),
    );
    assert!(same);
}

#[test]
fn a_json_body_is_encoded_and_labelled() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.post("{}", {{ json = {{ model = "m", stream = true, n = 2 }} }})"#,
            server.url
        ),
    );
    let request = server.request();
    assert_eq!(
        request.header("content-type").as_deref(),
        Some("application/json")
    );
    let mut body = String::from_utf8(request.body).unwrap();
    body.retain(|c| !c.is_whitespace());
    let mut fields: Vec<&str> = body
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .collect();
    fields.sort();
    assert_eq!(fields, [r#""model":"m""#, r#""n":2"#, r#""stream":true"#]);
}

#[test]
fn a_form_body_is_url_encoded_and_labelled() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.post("{}", {{ form = {{ b = "x y&z=é", a = "1" }} }})"#,
            server.url
        ),
    );
    let request = server.request();
    assert_eq!(
        request.header("content-type").as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(request.body, b"a=1&b=x+y%26z%3D%C3%A9");
}

#[test]
fn a_content_type_the_caller_sets_wins_in_any_case() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.post("{}", {{ json = {{}}, headers = {{ ["Content-TYPE"] = "application/vnd+json" }} }})"#,
            server.url
        ),
    );
    assert_eq!(
        server.request().header("content-type").as_deref(),
        Some("application/vnd+json")
    );
}

#[test]
fn a_method_is_sent_in_upper_case_whatever_its_case() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.request({{ url = "{}/x", method = "propfind" }})"#,
            server.url
        ),
    );
    assert_eq!(server.request().line, "PROPFIND /x HTTP/1.1");
}

#[test]
fn two_bodies_are_refused() {
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"http.post("http://127.0.0.1:9/", { body = "x", json = {} })"#,
    );
    assert!(
        message.contains("body") && message.contains("json"),
        "{message}"
    );
}

#[test]
fn an_option_a_request_does_not_have_is_refused() {
    // A misspelt option is input the module cannot use, so it raises rather than being ignored
    // (ADR 0017).
    let rt = runtime();
    let message = error_of(&rt, r#"http.get("http://127.0.0.1:9/", { heders = {} })"#);
    assert!(message.contains("heders"), "{message}");
}

#[test]
fn a_header_value_that_is_not_a_string_is_refused() {
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"http.get("http://127.0.0.1:9/", { headers = { ["x-n"] = 5 } })"#,
    );
    assert!(message.contains("x-n"), "{message}");
}

// Headers, both ways.

#[test]
fn the_user_agent_is_avarice_unless_replaced_or_removed() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.get("{0}/default")
               http.get("{0}/mine", {{ headers = {{ ["User-Agent"] = "mine/1" }} }})
               http.get("{0}/none", {{ headers = {{ ["user-agent"] = false }} }})"#,
            server.url
        ),
    );
    let agents: Vec<Option<String>> = server
        .requests()
        .iter()
        .map(|r| r.header("user-agent"))
        .collect();
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        agents,
        [
            Some(format!("avarice/{version}")),
            Some("mine/1".into()),
            None
        ]
    );
}

#[test]
fn a_client_sends_its_headers_which_a_request_overrides_or_removes() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"local client = http.client({{ headers = {{ Authorization = "Bearer k", ["x-a"] = "client" }} }})
               client:get("{0}/1")
               client:get("{0}/2", {{ headers = {{ ["X-A"] = "request", ["x-b"] = "b" }} }})
               client:get("{0}/3", {{ headers = {{ authorization = false }} }})"#,
            server.url
        ),
    );
    let requests = server.requests();
    let seen: Vec<(Option<String>, Option<String>, Option<String>)> = requests
        .iter()
        .map(|r| (r.header("authorization"), r.header("x-a"), r.header("x-b")))
        .collect();
    let s = |v: &str| Some(v.to_owned());
    assert_eq!(
        seen,
        [
            (s("Bearer k"), s("client"), None),
            (s("Bearer k"), s("request"), s("b")),
            (None, s("client"), None),
        ]
    );
}

#[test]
fn a_content_type_can_be_removed() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.post("{}", {{ json = {{ a = 1 }}, headers = {{ ["content-type"] = false }} }})"#,
            server.url
        ),
    );
    assert_eq!(server.request().header("content-type"), None);
}

#[test]
fn response_header_names_are_lower_case_and_repeated_values_are_joined() {
    let server = Server::new(|mut c| {
        while c.request().is_some() {
            c.send(
                b"HTTP/1.1 200 OK\r\nX-Many: one\r\nx-many: two\r\nX-Latin: caf\xe9\r\n\
                  Content-Length: 0\r\n\r\n",
            );
        }
    });
    let rt = runtime();
    let (many, latin): (String, avarice::mlua::BString) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{}")
               return res.headers["x-many"], res.headers["x-latin"]"#,
            server.url
        ),
    );
    assert_eq!(many, "one, two");
    assert_eq!(latin.as_slice(), b"caf\xe9");
}

// Clients and URLs.

#[test]
fn a_url_without_a_scheme_is_appended_to_the_base_url() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"http.client({{ base_url = "{0}/v1" }}):get("/chat/completions")
               http.client({{ base_url = "{0}/v1/" }}):get("models")
               http.client({{ base_url = "{0}/v1//" }}):get("//a")
               http.client({{ base_url = "{0}/v1" }}):get("{0}/elsewhere")"#,
            server.url
        ),
    );
    let targets: Vec<String> = server
        .requests()
        .iter()
        .map(|r| r.target().to_owned())
        .collect();
    assert_eq!(
        targets,
        ["/v1/chat/completions", "/v1/models", "/v1/a", "/elsewhere"]
    );
}

#[test]
fn a_url_without_a_scheme_and_no_base_url_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"http.get("/chat/completions")"#);
    assert!(message.contains("base_url"), "{message}");
}

#[test]
fn query_is_encoded_after_any_query_in_the_url_and_merges_with_the_clients() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"local client = http.client({{ base_url = "{0}", query = {{ key = "k", v = "1" }} }})
               client:get("/a?x=1", {{ query = {{ q = "a b&c", v = false }} }})"#,
            server.url
        ),
    );
    assert_eq!(server.request().target(), "/a?x=1&key=k&q=a+b%26c");
}

#[test]
fn a_client_reuses_its_connections() {
    let server = Server::answering("200 OK", &[], b"ok");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"local client = http.client()
               for _ = 1, 3 do client:get("{0}"):bytes() end
               for _ = 1, 3 do http.get("{0}"):bytes() end"#,
            server.url
        ),
    );
    assert_eq!(server.requests().len(), 6);
    assert_eq!(server.connections(), 2);
}

#[test]
fn an_option_a_client_does_not_have_is_refused() {
    let rt = runtime();
    let message = error_of(&rt, r#"http.client({ base_uri = "x" })"#);
    assert!(message.contains("base_uri"), "{message}");
}

#[test]
fn a_clients_settings_are_fixed_once_it_is_made() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"local options = {{ base_url = "{0}/one", headers = {{ ["x-a"] = "1" }} }}
               local client = http.client(options)
               options.base_url = "{0}/two"
               options.headers["x-a"] = "2"
               client:get("/x")"#,
            server.url
        ),
    );
    let request = server.request();
    assert_eq!(
        (request.target(), request.header("x-a").as_deref()),
        ("/one/x", Some("1"))
    );
}

// Server-sent events, parsed from a source of lines the test makes up.

/// The events `http.events` gives for `lines`, a Lua list of strings, each as
/// `event|data|id`, with an `id` of `nil` as `-`.
fn events_of(lines: &str) -> Vec<String> {
    let rt = runtime();
    eval(
        &rt,
        &format!(
            r#"local lines, i = {lines}, 0
               local source = {{ line = function() i = i + 1 return lines[i] end }}
               local seen = {{}}
               for ev in http.events(source) do
                 seen[#seen + 1] = ev.event .. "|" .. ev.data .. "|" .. (ev.id or "-")
               end
               return seen"#
        ),
    )
}

#[test]
fn an_event_is_its_data_once_a_blank_line_ends_it() {
    assert_eq!(events_of(r#"{ "data: hello", "" }"#), ["message|hello|-"]);
}

#[test]
fn data_lines_are_joined_with_newlines() {
    assert_eq!(
        events_of(r#"{ "data: a", "data: b", "data:", "" }"#),
        ["message|a\nb\n|-"]
    );
}

#[test]
fn an_event_has_its_type_and_the_last_id_seen() {
    assert_eq!(
        events_of(
            r#"{ "event: delta", "id: 7", "data: x", "", "data: y", "", "id: 8", "event: done", "data: z", "" }"#
        ),
        ["delta|x|7", "message|y|7", "done|z|8"]
    );
}

#[test]
fn comments_unknown_fields_and_retry_are_ignored() {
    assert_eq!(
        events_of(r#"{ ": ping", "retry: 10", "colour: red", "data: x", "" }"#),
        ["message|x|-"]
    );
}

#[test]
fn an_event_with_no_data_is_not_given() {
    assert_eq!(
        events_of(r#"{ "event: nothing", "", ": ping", "", "data: x", "" }"#),
        ["message|x|-"]
    );
}

#[test]
fn a_field_with_no_colon_has_an_empty_value() {
    assert_eq!(events_of(r#"{ "data", "data", "" }"#), ["message|\n|-"]);
}

#[test]
fn only_one_space_after_the_colon_is_dropped() {
    assert_eq!(
        events_of(r#"{ "data:x", "", "data:  y", "" }"#),
        ["message|x|-", "message| y|-"]
    );
}

#[test]
fn a_bare_carriage_return_ends_a_line_too() {
    assert_eq!(
        events_of(r#"{ "data: a\rdata: b\r", "data: c\r\rdata: d", "" }"#),
        ["message|a\nb|-", "message|c|-", "message|d|-"]
    );
}

#[test]
fn a_byte_order_mark_at_the_start_is_dropped() {
    assert_eq!(events_of(r#"{ "\u{FEFF}data: x", "" }"#), ["message|x|-"]);
}

#[test]
fn an_event_the_stream_ends_in_the_middle_of_is_not_given() {
    assert_eq!(
        events_of(r#"{ "data: x", "", "data: cut" }"#),
        ["message|x|-"]
    );
}

// Streaming a response, and closing it.

/// A server that answers with `chunks` as a chunked body and then leaves the body open, as a
/// stream that has more to send would, and says whether the client closed the connection within
/// two seconds of the last chunk.
fn streaming(chunks: &'static [&'static str]) -> (Server, std::sync::mpsc::Receiver<bool>) {
    let (sender, closed) = std::sync::mpsc::channel();
    let server = Server::new(move |mut c| {
        if c.request().is_none() {
            return;
        }
        c.start_chunked(&[("Content-Type", "text/event-stream")]);
        for chunk in chunks {
            c.chunk(chunk);
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = sender.send(c.closed_within(Duration::from_secs(2)));
    });
    (server, closed)
}

#[test]
fn events_arrive_whole_from_a_stream_split_anywhere_and_break_closes_it() {
    let (server, closed) = streaming(&[
        "data: {\"t\": \"Hel",
        "lo\"}\n\ndata: {\"t\": \", \"}\n",
        "\ndata: {\"t\": \"world\"}\r\n\r\n",
        "data: [DONE]\n\n",
    ]);
    let rt = runtime();
    let text: String = eval(
        &rt,
        &format!(
            r#"local text = ""
               for ev in http.post("{}/chat", {{ json = {{ stream = true }} }}):events() do
                 if ev.data == "[DONE]" then break end
                 text = text .. ev.data:match('"t": "(.-)"')
               end
               return text"#,
            server.url
        ),
    );
    assert_eq!(text, "Hello, world");
    assert!(
        closed.recv().unwrap(),
        "breaking out of events() left the connection open"
    );
}

#[test]
fn breaking_out_of_lines_closes_the_response() {
    let (server, closed) = streaming(&["one\ntwo\n"]);
    let rt = runtime();
    let first: String = eval(
        &rt,
        &format!(
            r#"for line in http.get("{}").body:lines() do return line end"#,
            server.url
        ),
    );
    assert_eq!(first, "one");
    assert!(
        closed.recv().unwrap(),
        "breaking out of lines() left the connection open"
    );
}

#[test]
fn a_to_be_closed_response_is_closed_at_the_end_of_its_scope() {
    let (server, closed) = streaming(&["one\n"]);
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"do
                 local res <close> = http.get("{}")
                 res.body:line()
               end"#,
            server.url
        ),
    );
    assert!(
        closed.recv().unwrap(),
        "the response was left open past its scope"
    );
}

#[test]
fn close_closes_a_response_and_reading_it_after_raises() {
    let (server, closed) = streaming(&["one\n"]);
    let rt = runtime();
    let message = error_of(
        &rt,
        &format!(
            r#"local res = http.get("{}")
               res:close()
               res.body:line()"#,
            server.url
        ),
    );
    assert!(message.contains("closed"), "{message}");
    assert!(closed.recv().unwrap(), "close left the connection open");
}

#[test]
fn a_request_is_not_a_task_even_while_its_connection_waits_to_be_reused() {
    // A pooled connection counting as a task would keep `avarice` from exiting until the pool
    // let it go (ADR 0025).
    let server = Server::answering("200 OK", &[], b"ok");
    let rt = runtime();
    eval::<()>(&rt, &format!(r#"http.get("{}"):bytes()"#, server.url));
    assert_eq!(rt.outstanding_tasks(), 0);
}

// Errors.

/// The `kind`, `url` and message of the error `source` raises, which must be a table.
fn failure_of(rt: &Runtime, source: &str) -> (String, String, String) {
    eval(
        rt,
        &format!(
            r#"local ok, e = pcall(function() {source} end)
               assert(not ok, "no error was raised")
               assert(type(e) == "table", "not a table: " .. tostring(e))
               return e.kind, e.url, tostring(e)"#
        ),
    )
}

/// A port nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[test]
fn nothing_listening_is_a_connect_error() {
    let rt = runtime();
    let port = closed_port();
    let (kind, url, message) =
        failure_of(&rt, &format!(r#"http.get("http://127.0.0.1:{port}/x")"#));
    assert_eq!(kind, "connect");
    assert_eq!(url, format!("http://127.0.0.1:{port}/x"));
    assert!(message.starts_with("http: "), "{message}");
    assert!(message.contains(&url), "{message}");
}

#[test]
fn an_error_never_shows_a_urls_query_or_userinfo() {
    let rt = runtime();
    let port = closed_port();
    let (_, url, message) = failure_of(
        &rt,
        &format!(r#"http.get("http://user:PASSWORD@127.0.0.1:{port}/x?key=SECRET#frag")"#),
    );
    assert_eq!(url, format!("http://127.0.0.1:{port}/x"));
    for secret in ["PASSWORD", "SECRET", "user"] {
        assert!(!message.contains(secret), "{message}");
    }
}

#[test]
fn a_body_cut_short_is_a_body_error_where_it_is_read() {
    let server = Server::new(|mut c| {
        if c.request().is_some() {
            c.send("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        }
    });
    let rt = runtime();
    let (kind, _, message) = failure_of(
        &rt,
        &format!(
            r#"local res = http.get("{}")
               res:bytes()"#,
            server.url
        ),
    );
    assert_eq!(kind, "body", "{message}");
}

#[test]
fn something_that_is_not_tls_where_tls_should_be_is_a_tls_error() {
    let server = Server::new(|mut c| {
        c.send("this is not a TLS handshake\r\n\r\n");
        c.closed_within(Duration::from_secs(2));
    });
    let rt = runtime();
    let url = server.url.replace("http://", "https://");
    let (kind, _, message) = failure_of(&rt, &format!(r#"http.get("{url}")"#));
    assert_eq!(kind, "tls", "{message}");
}

#[test]
fn a_redirect_loop_is_a_redirect_error() {
    let server = Server::new(|mut c| {
        while c.request().is_some() {
            c.respond("302 Found", &[("Location", "/again")], "");
        }
    });
    let rt = runtime();
    let (kind, _, message) = failure_of(&rt, &format!(r#"http.get("{}/start")"#, server.url));
    assert_eq!(kind, "redirect", "{message}");
}

#[test]
fn redirects_are_followed_and_the_url_is_where_they_led() {
    let server = Server::new(|mut c| {
        while let Some(request) = c.request() {
            if request.target() == "/start" {
                c.respond("302 Found", &[("Location", "/end")], "");
            } else {
                c.respond("200 OK", &[], "here");
            }
        }
    });
    let rt = runtime();
    let (url, body): (String, String) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{}/start")
               return res.url, res:bytes()"#,
            server.url
        ),
    );
    assert_eq!(
        (url, body.as_str()),
        (format!("{}/end", server.url), "here")
    );
}

#[test]
fn a_response_prints_its_status_and_url_without_its_query_or_headers() {
    let server = Server::answering("200 OK", &[("Set-Cookie", "session=COOKIE")], b"");
    let rt = runtime();
    let shown: String = eval(
        &rt,
        &format!(
            r#"return tostring(http.get("{}/x?key=SECRET"))"#,
            server.url
        ),
    );
    assert_eq!(shown, format!("Response(200 {}/x)", server.url));
}

// Timeouts.

/// The `kind` and `phase` of the error `source` raises, which must be a table.
fn timeout_of(rt: &Runtime, source: &str) -> (String, Option<String>, String) {
    eval(
        rt,
        &format!(
            r#"local ok, e = pcall(function() {source} end)
               assert(not ok, "no error was raised")
               assert(type(e) == "table", "not a table: " .. tostring(e))
               return e.kind, e.phase, tostring(e)"#
        ),
    )
}

/// A server that reads a request and then sends nothing, for as long as the client waits.
fn silent() -> Server {
    Server::new(|mut c| {
        if c.request().is_some() {
            c.closed_within(Duration::from_secs(10));
        }
    })
}

#[test]
fn waiting_too_long_for_the_headers_is_an_idle_timeout() {
    let server = silent();
    let rt = runtime();
    let (kind, phase, message) = timeout_of(
        &rt,
        &format!(r#"http.get("{}", {{ idle_timeout = 100 }})"#, server.url),
    );
    assert_eq!(
        (kind.as_str(), phase.as_deref()),
        ("timeout", Some("idle")),
        "{message}"
    );
    assert!(message.contains("100"), "{message}");
}

#[test]
fn waiting_too_long_for_more_of_the_body_is_an_idle_timeout_where_it_is_read() {
    let (server, _) = streaming(&["one\n"]);
    let rt = runtime();
    let (kind, phase, message) = timeout_of(
        &rt,
        &format!(
            r#"local res = http.get("{}", {{ idle_timeout = 100 }})
               assert(res.body:line() == "one")
               res.body:line()"#,
            server.url
        ),
    );
    assert_eq!(
        (kind.as_str(), phase.as_deref()),
        ("timeout", Some("idle")),
        "{message}"
    );
}

#[test]
fn a_request_sets_its_own_idle_timeout_over_its_clients() {
    let server = Server::new(|mut c| {
        while c.request().is_some() {
            std::thread::sleep(Duration::from_millis(300));
            c.respond("200 OK", &[], "late");
        }
    });
    let rt = runtime();
    let bodies: Vec<String> = eval(
        &rt,
        &format!(
            r#"local client = http.client({{ idle_timeout = 50 }})
               return {{
                 client:get("{0}", {{ idle_timeout = 2000 }}):bytes(),
                 client:get("{0}", {{ idle_timeout = false }}):bytes(),
               }}"#,
            server.url
        ),
    );
    assert_eq!(bodies, ["late", "late"]);
}

#[test]
fn a_body_that_trickles_past_the_total_timeout_is_a_total_timeout() {
    let server = Server::new(|mut c| {
        if c.request().is_none() {
            return;
        }
        c.start_chunked(&[]);
        for _ in 0..100 {
            c.chunk("x");
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let rt = runtime();
    let (kind, phase, message) = timeout_of(
        &rt,
        &format!(
            r#"http.get("{}", {{ timeout = 300, idle_timeout = 200 }}):bytes()"#,
            server.url
        ),
    );
    assert_eq!(
        (kind.as_str(), phase.as_deref()),
        ("timeout", Some("total")),
        "{message}"
    );
}

#[test]
fn connect_timeout_is_set_on_a_client_and_refused_on_a_request() {
    let rt = runtime();
    eval::<()>(&rt, r#"http.client({ connect_timeout = 1000 })"#);
    let message = error_of(
        &rt,
        r#"http.get("http://127.0.0.1:9/", { connect_timeout = 1000 })"#,
    );
    assert!(message.contains("connect_timeout"), "{message}");
}

#[test]
fn a_timeout_that_is_not_a_positive_number_or_false_is_refused() {
    let rt = runtime();
    for timeout in ["0", "-5", "'10'", "true"] {
        let message = error_of(
            &rt,
            &format!(r#"http.get("http://127.0.0.1:9/", {{ timeout = {timeout} }})"#),
        );
        assert!(message.contains("timeout"), "{timeout}: {message}");
    }
}

// Error statuses.

#[test]
fn an_error_status_is_returned_unless_check_is_set() {
    let server = Server::answering("404 Not Found", &[], b"nothing here");
    let rt = runtime();
    let (status, body): (u16, String) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{}") return res.status, res:bytes()"#,
            server.url
        ),
    );
    assert_eq!((status, body.as_str()), (404, "nothing here"));
}

#[test]
fn check_raises_a_status_error_carrying_the_response() {
    let server = Server::answering(
        "400 Bad Request",
        &[("Retry-After", "3")],
        br#"{"error": {"message": "model not found"}}"#,
    );
    let rt = runtime();
    let (kind, status, retry_after, body, message): (String, u16, String, String, String) = eval(
        &rt,
        &format!(
            r#"local ok, e = pcall(http.post, "{}/v1/chat?key=SECRET", {{ json = {{}}, check = true }})
               assert(not ok)
               return e.kind, e.status, e.headers["retry-after"], e.body:bytes(), tostring(e)"#,
            server.url
        ),
    );
    assert_eq!(
        (kind.as_str(), status, retry_after.as_str()),
        ("status", 400, "3")
    );
    assert_eq!(body, r#"{"error": {"message": "model not found"}}"#);
    assert!(message.contains("400"), "{message}");
    assert!(message.contains("model not found"), "{message}");
    assert!(
        message.contains(&format!("{}/v1/chat", server.url)),
        "{message}"
    );
    assert!(!message.contains("SECRET"), "{message}");
}

#[test]
fn check_on_a_client_applies_to_its_requests_and_is_quiet_about_success() {
    let server = Server::new(|mut c| {
        while let Some(request) = c.request() {
            match request.target() {
                "/ok" => c.respond("200 OK", &[], "fine"),
                _ => c.respond("503 Service Unavailable", &[], "busy"),
            }
        }
    });
    let rt = runtime();
    let (fine, kind): (String, String) = eval(
        &rt,
        &format!(
            r#"local client = http.client({{ base_url = "{}", check = true }})
               local fine = client:get("/ok"):bytes()
               local ok, e = pcall(client.get, client, "/busy")
               return fine, e.kind"#,
            server.url
        ),
    );
    assert_eq!((fine.as_str(), kind.as_str()), ("fine", "status"));
}

#[test]
fn a_status_errors_body_stops_at_64_kib() {
    let server = Server::answering("500 Internal Server Error", &[], &[b'x'; 100 * 1024]);
    let rt = runtime();
    let (length, message_length): (i64, i64) = eval(
        &rt,
        &format!(
            r#"local ok, e = pcall(http.get, "{}", {{ check = true }})
               return #e.body, #tostring(e)"#,
            server.url
        ),
    );
    assert_eq!(length, 64 * 1024);
    assert!(message_length < 1024, "a message of {message_length} bytes");
}

// Retrying.

/// A server whose `n`th request (from 1) is answered by `answer(n, connection)`.
fn counting(answer: impl Fn(usize, &mut Connection) + Send + Sync + 'static) -> Server {
    let count = Arc::new(AtomicUsize::new(0));
    Server::new(move |mut c| {
        while c.request().is_some() {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            answer(n, &mut c);
        }
    })
}

#[test]
fn nothing_is_retried_unless_asked() {
    let server = counting(|_, c| c.respond("503 Service Unavailable", &[], "busy"));
    let rt = runtime();
    let status: u16 = eval(&rt, &format!(r#"return http.get("{}").status"#, server.url));
    assert_eq!((status, server.requests().len()), (503, 1));
}

#[test]
fn a_listed_status_is_retried_until_it_is_not() {
    let server = counting(|n, c| match n {
        1 => c.respond("503 Service Unavailable", &[], "busy"),
        2 => c.respond("429 Too Many Requests", &[], "slow down"),
        _ => c.respond("200 OK", &[], "done"),
    });
    let rt = runtime();
    let body: String = eval(
        &rt,
        &format!(
            r#"return http.get("{}", {{ retry = {{ base_delay = 10 }} }}):bytes()"#,
            server.url
        ),
    );
    assert_eq!((body.as_str(), server.requests().len()), ("done", 3));
}

#[test]
fn when_attempts_run_out_the_last_response_is_what_comes_back() {
    let server = counting(|n, c| c.respond("502 Bad Gateway", &[], format!("attempt {n}")));
    let rt = runtime();
    let (status, body): (u16, String) = eval(
        &rt,
        &format!(
            r#"local res = http.get("{}", {{ retry = {{ attempts = 2, base_delay = 10 }} }})
               return res.status, res:bytes()"#,
            server.url
        ),
    );
    assert_eq!((status, body.as_str()), (502, "attempt 2"));
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn with_check_the_last_failure_is_raised_when_attempts_run_out() {
    let server = counting(|n, c| c.respond("502 Bad Gateway", &[], format!("attempt {n}")));
    let rt = runtime();
    let (kind, body): (String, String) = eval(
        &rt,
        &format!(
            r#"local ok, e = pcall(http.get, "{}", {{ check = true, retry = {{ attempts = 3, base_delay = 10 }} }})
               return e.kind, e.body:bytes()"#,
            server.url
        ),
    );
    assert_eq!((kind.as_str(), body.as_str()), ("status", "attempt 3"));
}

#[test]
fn a_connection_dropped_before_a_response_is_retried() {
    let count = Arc::new(AtomicUsize::new(0));
    let server = Server::new(move |mut c| {
        if c.request().is_none() {
            return;
        }
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            return; // drops the connection without answering
        }
        c.respond("200 OK", &[("Connection", "close")], "second time");
    });
    let rt = runtime();
    let body: String = eval(
        &rt,
        &format!(
            r#"return http.get("{}", {{ retry = {{ base_delay = 10 }} }}):bytes()"#,
            server.url
        ),
    );
    assert_eq!(body, "second time");
}

#[test]
fn retry_after_in_seconds_is_waited_for_up_to_max_delay() {
    let server = counting(|n, c| match n {
        1 => c.respond("429 Too Many Requests", &[("Retry-After", "1")], ""),
        _ => c.respond("200 OK", &[], "ok"),
    });
    let rt = runtime();
    let started = std::time::Instant::now();
    eval::<()>(
        &rt,
        &format!(
            r#"http.get("{}", {{ retry = {{ base_delay = 10 }} }})"#,
            server.url
        ),
    );
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "{:?}",
        started.elapsed()
    );

    let started = std::time::Instant::now();
    eval::<()>(
        &rt,
        &format!(
            r#"http.get("{}", {{ retry = {{ base_delay = 10, max_delay = 50 }} }})"#,
            server.url.clone() + "/again"
        ),
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn retry_after_as_a_date_is_waited_for() {
    let later = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(2));
    let server = counting(move |n, c| match n {
        1 => c.respond("503 Service Unavailable", &[("Retry-After", &later)], ""),
        _ => c.respond("200 OK", &[], "ok"),
    });
    let rt = runtime();
    let started = std::time::Instant::now();
    eval::<()>(
        &rt,
        &format!(
            r#"http.get("{}", {{ retry = {{ base_delay = 10 }} }})"#,
            server.url
        ),
    );
    // A date has whole seconds, so it can be anything from one second to two away.
    assert!(
        started.elapsed() >= Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_stream_cut_after_its_response_is_raised_not_retried() {
    let server = Server::new(|mut c| {
        if c.request().is_some() {
            c.send("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        }
    });
    let rt = runtime();
    let kind: String = eval(
        &rt,
        &format!(
            r#"local ok, e = pcall(function() return http.get("{}", {{ retry = true }}):bytes() end)
               return e.kind"#,
            server.url
        ),
    );
    assert_eq!((kind.as_str(), server.requests().len()), ("body", 1));
}

#[test]
fn a_client_retries_every_request_and_a_request_can_say_not_to() {
    let server = counting(|n, c| match n {
        1 | 3 => c.respond("503 Service Unavailable", &[], ""),
        _ => c.respond("200 OK", &[], ""),
    });
    let rt = runtime();
    let statuses: Vec<u16> = eval(
        &rt,
        &format!(
            r#"local client = http.client({{ retry = {{ base_delay = 10 }} }})
               return {{ client:get("{0}").status, client:get("{0}", {{ retry = false }}).status }}"#,
            server.url
        ),
    );
    assert_eq!(statuses, [200, 503]);
}

#[test]
fn a_retry_option_it_does_not_have_is_refused() {
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"http.get("http://127.0.0.1:9/", { retry = { tries = 2 } })"#,
    );
    assert!(message.contains("tries"), "{message}");
}

// Compression.

/// `hello, compressed world`, gzipped.
const GZIPPED: &[u8] = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xcb\x48\xcd\xc9\xc9\xd7\x51\x48\xce\xcf\x2d\x28\x4a\x2d\x2e\x4e\x4d\x51\x28\xcf\x2f\xca\x49\x01\x00\x08\x9e\x34\x35\x17\x00\x00\x00";

#[test]
fn a_compressed_body_is_decompressed_and_compression_is_asked_for() {
    let server = Server::answering("200 OK", &[("Content-Encoding", "gzip")], GZIPPED);
    let rt = runtime();
    let body: String = eval(
        &rt,
        &format!(r#"return http.get("{}"):bytes()"#, server.url),
    );
    assert_eq!(body, "hello, compressed world");
    let accepted = server
        .request()
        .header("accept-encoding")
        .unwrap_or_default();
    for encoding in ["gzip", "br", "zstd"] {
        assert!(accepted.contains(encoding), "{accepted}");
    }
}

#[test]
fn accept_encoding_can_be_removed_and_then_nothing_is_decompressed() {
    let server = Server::answering("200 OK", &[("Content-Encoding", "gzip")], GZIPPED);
    let rt = runtime();
    let length: i64 = eval(
        &rt,
        &format!(
            r#"return #http.get("{}", {{ headers = {{ ["accept-encoding"] = false }} }}):bytes()"#,
            server.url
        ),
    );
    assert_eq!(length, GZIPPED.len() as i64);
    assert_eq!(server.request().header("accept-encoding"), None);
}

// Cancelling, and reading from two tasks.

/// A trusted runtime with `http` required, which `cancel` cancels.
fn cancellable(cancel: &avarice::CancelHandle) -> Runtime {
    let rt = Runtime::builder(Profile::Trusted)
        .cancel_handle(cancel.clone())
        .build()
        .unwrap();
    rt.block_on(rt.exec(r#"http = require("http")"#, "=prelude"))
        .unwrap();
    rt
}

/// Runs `source` in a runtime that is cancelled once the server has a request, and checks that
/// it was the cancel that ended it.
fn cancelled_once_requested(server: &Server, source: &str) {
    let cancel = avarice::CancelHandle::new();
    let rt = cancellable(&cancel);
    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while server.requests().is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            }
            std::thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        rt.block_on(rt.exec(source, "=test"))
    });
    match result.unwrap_err() {
        avarice::Error::Lua(err) => assert!(avarice::was_cancelled(&err), "{err}"),
        other => panic!("expected a Lua error, got {other:?}"),
    }
}

#[test]
fn cancelling_a_wait_for_a_response_drops_the_request() {
    let (sender, closed) = std::sync::mpsc::channel();
    let server = Server::new(move |mut c| {
        if c.request().is_some() {
            let _ = sender.send(c.closed_within(Duration::from_secs(5)));
        }
    });
    cancelled_once_requested(&server, &format!(r#"http.get("{}")"#, server.url));
    assert!(
        closed.recv().unwrap(),
        "the cancelled request's connection stayed open"
    );
}

#[test]
fn cancelling_a_wait_for_more_of_a_body_drops_the_connection() {
    let (server, closed) = streaming(&["one\n"]);
    cancelled_once_requested(
        &server,
        &format!(
            r#"local res = http.get("{}")
               res.body:line()
               res.body:line()"#,
            server.url
        ),
    );
    assert!(
        closed.recv().unwrap(),
        "the cancelled body's connection stayed open"
    );
}

#[cfg(feature = "stdlib-utils")]
#[test]
fn a_body_read_by_two_tasks_at_once_is_an_error_for_the_second() {
    let (server, _) = streaming(&["one\n"]);
    let rt = runtime();
    let message = error_of(
        &rt,
        &format!(
            r#"local utils = require("utils")
               local res = http.get("{}")
               assert(res.body:line() == "one")
               utils.spawn_task(function() pcall(res.body.line, res.body) end)
               utils.spawn_timeout(function() end, 50):await()
               res.body:line()"#,
            server.url
        ),
    );
    assert!(message.contains("another task"), "{message}");
}

#[test]
fn a_body_and_a_client_print_as_what_they_are() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    let shown: Vec<String> = eval(
        &rt,
        &format!(
            r#"return {{
                 tostring(http.get("{0}").body),
                 tostring(http.client({{ base_url = "https://user:pw@example.com/v1?key=SECRET" }})),
                 tostring(http.client()),
               }}"#,
            server.url
        ),
    );
    assert_eq!(
        shown,
        ["Reader(body)", "Client(https://example.com/v1)", "Client()"]
    );
}

// What the review found.

#[test]
fn every_shortcut_sends_its_method() {
    let server = Server::answering("200 OK", &[], b"");
    let rt = runtime();
    eval::<()>(
        &rt,
        &format!(
            r#"local client = http.client({{ base_url = "{0}" }})
               for _, method in ipairs({{ "get", "post", "put", "patch", "delete", "head" }}) do
                 http[method]("{0}/" .. method)
                 client[method](client, "/" .. method)
               end"#,
            server.url
        ),
    );
    let lines: Vec<String> = server.requests().iter().map(|r| r.line.clone()).collect();
    let mut expected = Vec::new();
    for method in ["get", "post", "put", "patch", "delete", "head"] {
        let line = format!("{} /{method} HTTP/1.1", method.to_uppercase());
        expected.push(line.clone());
        expected.push(line);
    }
    assert_eq!(lines, expected);
}

#[test]
fn check_raises_only_for_4xx_and_5xx() {
    let server = Server::answering("600 Beyond", &[], b"");
    let rt = runtime();
    let status: u16 = eval(
        &rt,
        &format!(
            r#"return http.get("{}", {{ check = true }}).status"#,
            server.url
        ),
    );
    assert_eq!(status, 600);
}

#[test]
fn the_total_timeout_covers_every_attempt_and_the_waits_between() {
    let server = counting(|_, c| {
        std::thread::sleep(Duration::from_millis(150));
        c.respond("503 Service Unavailable", &[], "");
    });
    let rt = runtime();
    let started = std::time::Instant::now();
    let (kind, phase, message) = timeout_of(
        &rt,
        &format!(
            r#"http.get("{}", {{ timeout = 400, retry = {{ attempts = 10, base_delay = 10 }} }})"#,
            server.url
        ),
    );
    assert_eq!(
        (kind.as_str(), phase.as_deref()),
        ("timeout", Some("total")),
        "{message}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(1000),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn accept_cannot_be_removed_so_removing_it_is_refused() {
    // reqwest sends `Accept: */*` with every request that has none, so `false` could not be kept.
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"http.get("http://127.0.0.1:9/", { headers = { Accept = false } })"#,
    );
    assert!(message.contains("accept"), "{message}");
}

#[test]
fn a_number_that_is_not_finite_is_refused_as_a_timeout_or_a_delay() {
    let rt = runtime();
    for option in [
        "timeout = math.huge",
        "idle_timeout = 0/0",
        "retry = { max_delay = math.huge }",
    ] {
        let message = error_of(
            &rt,
            &format!(r#"http.get("http://127.0.0.1:9/", {{ {option} }})"#),
        );
        assert!(message.contains("milliseconds"), "{option}: {message}");
    }
}

#[test]
fn statuses_that_are_not_a_list_are_refused() {
    let rt = runtime();
    let message = error_of(
        &rt,
        r#"http.get("http://127.0.0.1:9/", { retry = { statuses = { 429, [5] = 503 } } })"#,
    );
    assert!(message.contains("statuses"), "{message}");
}
