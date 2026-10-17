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
