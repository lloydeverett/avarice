---
status: ready-for-agent
adrs: [0025, 0004, 0015, 0016, 0017]
---

# `http`: a Client, and responses read as they arrive

## Problem Statement

A script author wants to call HTTP APIs from Lua, and above all to stream tokens from an
OpenAI-compatible inference endpoint as they are generated. The `http` **stdlib module** today is
Astra's client, and it cannot do that well:

- Its only way to stream, `execute_streaming`, runs a callback on a detached task. Errors reach a
  log and nowhere else, nothing says when the stream has ended or whether it was cut short, the
  callback cannot wait on anything, and the script can neither stop the stream nor wait for it.
- Chunks arrive as the network delivers them, so every script must buffer them into lines and
  server-sent events itself.
- Every request opens new connections, with a fresh TLS handshake, so an agent loop calling one
  endpoint over and over pays for it every time.
- There are no timeouts, and a runtime's time limit does not end a wait, so a stalled server hangs
  the script until someone presses Ctrl-C.
- A body that fails to arrive is handed over as an empty one. A table body is sent as JSON or as
  bytes by guessing. A `content-type` set in lower case is sent beside the `text/plain` the module
  adds.
- An error status gives no help: the script must dig the server's explanation out of the body
  itself, and failures it might retry cannot be told apart from ones it should not.

## Solution

`http` is rewritten, original to avarice (ADR 0025), and Astra's client is removed with no
compatibility layer.

A script makes a **Client** with `http.client{...}`, holding a base URL, default headers and other
defaults, and keeping its connections to reuse. It sends requests through it, or through the
module's own functions, which use a default Client. A request is one table: a URL, a method,
headers, query parameters, and at most one of a byte body, a JSON value or a form.

A response comes back as soon as its headers arrive. Its status, headers and URL are fields, and its
body is a **Reader**, the same one a **Child**'s output is: the script asks for the next chunk, the
next line or the rest, and waits for it. `res:events()` parses server-sent events from it, so a
token loop is a generic `for`. Breaking out of the loop, or closing the response, drops the
connection.

Three timeouts bound connecting, each silence, and the whole request. Failures raise tables with a
`kind`, a 4xx or 5xx raises only when `check` is set and then carries the server's explanation, and
retrying is an opt-in option that never repeats a response the script has already started reading.

## User Stories

### Sending a request

1. As a script author, I want `http.get(url)` to return a response, so that a one-off request is one
   line.
2. As a script author, I want `http.request{ url, method, ... }` for any method, so that I am not
   limited to the methods with shortcuts.
3. As a script author, I want shortcuts for `get`, `post`, `put`, `patch`, `delete` and `head` that
   take a URL and an options table, so that common calls read naturally.
4. As a script author, I want to give a method in any case, so that `"post"` and `"POST"` both work.
5. As a script author, I want to pass `headers` as a table, so that I can authenticate and negotiate
   content.
6. As a script author, I want header names to match whatever their case, so that my `content-type`
   replaces a default `Content-Type` rather than being sent beside it.
7. As a script author, I want to set a header to `false` to stop it being sent, so that I can remove
   a default, a Client's header or the `User-Agent`.
8. As a script author, I want to set my own `User-Agent`, so that an API that identifies callers
   sees mine.
9. As a script author, I want a `User-Agent` sent by default, so that APIs that reject requests
   without one accept mine.
10. As a script author, I want to pass `query` as a table, so that parameters are encoded for me and
    appended to the URL.
11. As a script author, I want `body` to take a string or a **Buffer** and send it byte for byte, so
    that binary data arrives intact.
12. As a script author, I want `body` to add no `Content-Type` of its own, so that I am not told my
    bytes are `text/plain`.
13. As a script author, I want `json` to encode any Lua value and set `Content-Type:
    application/json`, so that calling a JSON API needs no extra step.
14. As a script author, I want `json` to encode exactly as `serde.json.encode` does, so that I never
    meet two JSON dialects in one runtime.
15. As a script author, I want `form` to send a table of strings url-encoded with its content type,
    so that form endpoints work.
16. As a script author, I want giving two of `body`, `json` and `form` to raise, so that a mistake
    is not resolved by a guess.
17. As a script author, I want a malformed URL, a non-string header value or an unknown option type
    to raise an ordinary error naming the problem, so that I fix my call.
18. As a script author, I want redirects followed, up to ten, so that moved resources still answer.
19. As a script author, I want compressed responses decompressed for me, gzip, brotli and zstd, so
    that I read the body the server meant.
20. As a script author, I want decompression to happen as the body arrives, so that a compressed
    stream still streams.

### Clients

21. As a script author, I want to make a Client with `http.client{...}`, so that settings shared by
    many requests are written once.
22. As a script author, I want a Client to keep its connections and reuse them, so that repeated
    calls to one API skip the connect and the TLS handshake.
23. As a script author, I want a Client's `base_url` to be prefixed to a URL that has no scheme,
    with one `/` between them, so that `/chat/completions` against `https://host/v1` goes to
    `https://host/v1/chat/completions`.
24. As a script author, I want a URL that has a scheme to ignore `base_url`, so that one Client can
    still reach somewhere else.
25. As a script author, I want a relative URL with no `base_url` to raise, so that the mistake is
    plain.
26. As a script author, I want a Client to hold default `headers`, `query`, `check`, `retry` and
    timeouts, so that every call site is one line.
27. As a script author, I want a request's options to override its Client's, and its `headers` and
    `query` to merge key by key, so that I can vary one thing for one call.
28. As a script author, I want a Client's settings fixed once it is made, so that one part of a
    script cannot change another's requests underneath it.
29. As a script author, I want the module's own functions to go through a default Client, so that
    even ad-hoc calls reuse connections.
30. As an embedder, I want each runtime to have its own default Client, so that two runtimes in my
    program never share connections or state.

### Reading a response

31. As a script author, I want the call to return as soon as the headers arrive, so that I can
    decide what to do before the body comes.
32. As a script author, I want `res.status` as a number, so that I compare it directly.
33. As a script author, I want `res.headers` keyed by lower-case name, so that I look up any header
    without guessing its case.
34. As a script author, I want a repeated header's values joined with `", "`, so that none of them
    is lost.
35. As a script author, I want header values as their exact bytes, so that nothing is replaced or
    dropped when a value is not UTF-8.
36. As a script author, I want `res.url` to be the URL that finally answered, so that I know where
    redirects led.
37. As a script author, I want `res.body` to be a Reader, so that I read it piece by piece as it
    arrives.
38. As a script author, I want `res.body:read()` to give the next chunk and `nil` at the end, so
    that I can process arbitrary bytes as they come.
39. As a script author, I want `res.body:line()` and `res.body:lines()`, so that line-delimited
    formats such as NDJSON need no buffering of my own.
40. As a script author, I want `res.body:rest()` to give the rest as a Buffer, so that I can take a
    large body without making a Lua string of it.
41. As a script author, I want `res:bytes()` to give the whole body as a string, so that the common
    case is one call.
42. As a script author, I want `res:json()` to decode the whole body, so that calling a JSON API is
    one call.
43. As a script author, I want the body's bytes untouched by any `charset`, so that what I get is
    what was sent.
44. As a script author, I want a Reader shared between two tasks at once to raise, so that neither
    silently gets half of the other's data.
45. As a script author, I want a body cut short to raise a `"body"` error where I am reading, so
    that a truncated response never looks complete.
46. As a script author, I want reading a body to the end to give `nil` repeatedly after that, so
    that loops end cleanly.
47. As a script author, I want `print(res)` to show the status and URL but never the headers or the
    query, so that printing a response does not leak a key.

### Streaming server-sent events

48. As a script author, I want `for ev in res:events() do ... end` to give one table per event, so
    that a token stream is a plain loop.
49. As a script author, I want each event to have `event`, `data` and `id`, so that I can tell event
    types apart.
50. As a script author, I want `event` to default to `"message"`, so that untyped events have a
    type.
51. As a script author, I want several `data:` lines in one event joined with `\n`, so that
    multi-line payloads come through whole.
52. As a script author, I want comment lines and unknown fields ignored, so that keep-alive pings do
    not reach my loop.
53. As a script author, I want an event with no data not delivered, so that I do not handle empty
    events.
54. As a script author, I want `\n`, `\r\n` and `\r` line endings all handled, so that any server's
    framing works.
55. As a script author, I want an event split across network chunks delivered whole, so that I never
    see half an event.
56. As a script author, I want a final event without a trailing blank line discarded as the WHATWG
    rules require, so that a cut stream does not hand me a partial event.
57. As a script author, I want `events()` to work whatever the `Content-Type`, so that self-hosted
    servers that mislabel their streams still work.
58. As a script author, I want `[DONE]` and JSON decoding left to me, so that the parser stays true
    to SSE and works with non-OpenAI streams.
59. As a script author, I want `events()` not to reconnect, so that a stream never silently starts
    again.
60. As a script author, I want to break out of an `events()` or `lines()` loop and have the
    connection dropped at once, so that "stop generating" is just `break`.

### Closing and cancelling

61. As a script author, I want `local res <close> = ...` to close the response when the scope ends,
    so that an error part-way still drops the connection.
62. As a script author, I want `res:close()`, so that I can drop a connection explicitly.
63. As a script author, I want reading after close to raise, so that a closed response is not
    mistaken for an empty one.
64. As a script author, I want a response I lose hold of to be cleaned up when Lua collects it, so
    that a forgotten response does not hold a connection forever.
65. As a script author, I want aborting or cancelling the task that waits on a request to drop the
    request, so that cancelled work stops reaching the network.
66. As an `avarice` user, I want Ctrl-C to end a script waiting on a request, so that a hung request
    never holds my terminal.
67. As an embedder, I want an in-flight request not to count as a task, so that `wait_for_tasks`
    does not wait on a response nobody will read.

### Timeouts

68. As a script author, I want a `connect_timeout` on a Client, defaulting to 30 seconds, so that an
    unreachable host fails promptly.
69. As a script author, I want an `idle_timeout`, defaulting to five minutes, bounding the wait for
    the headers and for each next byte, so that a stalled stream ends without capping a long one.
70. As a script author, I want to set `idle_timeout` per request, so that a slow reasoning call can
    wait longer than the rest.
71. As a script author, I want a `timeout` on the whole request, body included, off by default, so
    that I can cap a call when I need to.
72. As a script author, I want `false` to disable any timeout, so that I can wait as long as it
    takes.
73. As a script author, I want timeouts in milliseconds, as `process` takes them, so that one
    runtime has one unit.
74. As a script author, I want a timeout to raise `kind = "timeout"` with a `phase` of `"connect"`,
    `"idle"` or `"total"`, so that I know which one fired.

### Errors

75. As a script author, I want network failures raised as tables with a `kind`, so that I can handle
    each kind differently.
76. As a script author, I want an error table to print as a readable message, so that an unhandled
    failure is still clear.
77. As a script author, I want `"connect"` for DNS failures, refusals and resets before a response,
    so that I know the server never answered.
78. As a script author, I want `"tls"` for a failed handshake or certificate, so that I can tell a
    security problem from an outage.
79. As a script author, I want `"redirect"` for too many redirects or a loop, so that a
    misconfigured server is named as such.
80. As a script author, I want `"body"` for a connection lost mid-body or a failed decompression,
    so that truncation is never silent.
81. As a script author, I want an error's message and its `url` to leave out the query string and
    any userinfo, so that an API key in a URL never reaches my logs.
82. As a script author, I want a 4xx or 5xx returned normally by default, so that I can inspect a
    429's `Retry-After` without a `pcall`.
83. As a script author, I want `check = true` to raise a `"status"` error for a 4xx or 5xx, so that
    I need not check every status myself.
84. As a script author, I want a `"status"` error to carry `status`, `headers` and the body as a
    Buffer, so that I can act on the server's answer.
85. As a script author, I want the start of the body in a `"status"` error's message, so that
    "model not found" appears where I first look.
86. As a script author, I want the body in a `"status"` error capped at 64 KiB, so that a
    misbehaving server cannot make an enormous error.
87. As a script author, I want the response closed once a `"status"` error is raised, so that the
    connection is freed.

### Retrying

88. As a script author, I want no retrying unless I ask, so that a POST that bills per token is
    never sent twice behind my back.
89. As a script author, I want `retry = true` to retry with sensible defaults, so that turning it on
    is one word.
90. As a script author, I want `retry = { attempts, statuses, base_delay, max_delay }`, so that I
    can tune it.
91. As a script author, I want retries on connect failures, timeouts before a response and the
    listed statuses (429, 502, 503 and 504 by default), so that transient failures recover.
92. As a script author, I want exponential backoff with jitter between attempts, so that many
    clients do not retry in lockstep.
93. As a script author, I want `Retry-After`, in seconds or as an HTTP date, to replace the backoff,
    capped at `max_delay`, so that I wait as long as the server asks and no longer than I allow.
94. As a script author, I want no retry once I have the response, so that a stream broken mid-way
    raises instead of replaying tokens I have already seen.
95. As a script author, I want the last attempt's failure raised when retries run out, so that I
    see what finally went wrong.
96. As a script author, I want `retry` on a Client, so that every call to a flaky API retries.

### Embedding

97. As an embedder, I want `http` registered only by the trusted profile, as before, so that
    sandboxed scripts cannot reach the network.
98. As an embedder, I want `http` behind the `stdlib-http` feature, as before, so that a build
    without it carries no HTTP dependencies.
99. As a maintainer, I want one Reader shared by `process` and `http`, so that a fix to reading
    applies to both.

## Implementation Decisions

- **Astra's client is removed entirely.** Its request type, its response type, the `set_*`
  builder, `execute`, `execute_streaming`, the deprecated stubs and `http.status_codes` go, with
  their Astra attribution headers. The module keeps its name, `http`, and its feature,
  `stdlib-http`. Dependencies only it used, such as `tracing` and `futures` if nothing else needs
  them, and reqwest's `charset` feature, are dropped; reqwest's gzip, brotli and zstd features and
  tokio-util (to adapt a byte stream into the Reader) are added.
- **The Reader moves out of `process`** to a component of its own that both modules use. It wraps
  any buffered async byte source and keeps its present Lua surface: `read`, `line`, `lines`,
  `rest`, and the one-task-at-a-time rule. What it raises when the source fails is supplied by its
  owner: `process` keeps its plain messages, and `http` raises `"body"` and `"timeout"` tables. A
  Reader can be closed, after which reads raise. `process`'s behaviour does not change.
- **The idle timeout is enforced in the Reader** for `http`, not only by reqwest, because reqwest
  sets it per Client and the design allows it per request. The connect timeout is set on reqwest's
  client and is therefore Client-only. The total timeout covers the body as well as the headers.
- **A Client wraps one reqwest client.** `http.client(opts)` builds it, and the runtime builds its
  default Client the first time `http` needs it. Client defaults and request options are merged
  before sending: scalars by override, `headers` and `query` key by key, a header set to `false`
  removed at whichever level set it. Precedence is request, then Client, then avarice's defaults
  (`User-Agent: avarice/<version>`, and `Accept-Encoding` from reqwest's decompression).
- **URL joining** prefixes `base_url` to any URL without a scheme, collapsing the slashes at the
  join to one. `query` is appended after joining, after any query already in the URL.
- **Request table contract.** `url` (string, required), `method` (string, default `"GET"`, any
  case), `headers` (table of string or `false`), `query` (table of string), `body` (string or
  Buffer), `json` (any value), `form` (table of string), `check` (boolean), `retry` (`true` or a
  table), `idle_timeout` and `timeout` (milliseconds or `false`). The shortcuts take `(url,
  opts)`. A Client takes `base_url`, `headers`, `query`, `check`, `retry`, `connect_timeout`,
  `idle_timeout` and `timeout`. An unknown key is an error, as `process` treats one in a Command.
- **Response contract.** Fields `status`, `headers` and `url`, and `body`, a Reader. Methods
  `bytes()`, `json()`, `events()` and `close()`, plus `__close` and a `__tostring` of the form
  `Response(<status> <url without query>)`. `bytes()` and `json()` read what remains of the body.
- **`events()` is Lua**, written in the module's Lua half, implementing the WHATWG event-stream
  parsing rules over a line source: fields `event`, `data` and `id`; `retry` and unknown fields
  ignored; comments ignored; empty data not dispatched; a trailing partial event discarded at the
  end. It returns a generic-`for` iterator with a closing value, as `lines()` does, so `break`
  closes the response. The parser takes any value with a `line` method, which is the seam its unit
  tests use. It is public as `http.events(source)`, and `res:events()` is it over the body.
- **Line endings for SSE.** The Reader's `line` splits on `\n` and strips a trailing `\r`. Since SSE
  also allows a bare `\r`, the SSE parser splits a line on any `\r` left in it.
- **Errors** are raised as tables with a `__tostring`, built the way `process` builds its own:
  `kind` one of `"connect"`, `"tls"`, `"timeout"`, `"redirect"`, `"status"`, `"body"`; `url`
  without query or userinfo; `message`; `phase` for `"timeout"`; `status`, `headers` and `body` (a
  Buffer of at most 64 KiB) for `"status"`. reqwest's error is classified into a kind by what it
  reports (connect, timeout, redirect, body or decode), and a TLS failure by its source chain.
  Mistakes in the call are ordinary Lua errors.
- **Retrying** wraps sending, before the response reaches Lua. Defaults for `retry = true`: 3
  attempts in all, statuses 429, 502, 503 and 504, a base delay of 500 ms and a maximum of 30 000
  ms. A retried response's body is discarded and its connection dropped before waiting. `check`
  applies to the last response only.
- **Cancellation** needs no special path: a request is a future owned by the coroutine awaiting it,
  so aborting or cancelling that coroutine drops it, and dropping a response or its Reader drops the
  connection. Nothing is spawned onto the executor for a request: it is sent from a runtime of its
  own on one thread, where reqwest's connections live, so that a pooled connection is not a task
  (ADR 0025).
- **Annotations.** The module's Lua half carries LuaLS annotations for the Client, the request
  table, the response, the event and the error, as `process` does, and the Reader's annotations
  move with it.
- **README.** The `http` section is rewritten around a Client and a streamed completion.

## Testing Decisions

- **What a good test is here.** Each test runs Lua through a public `Runtime`, as an embedder does,
  and checks what the script observes and what the server received: values returned, errors raised
  and their fields, the bytes of each request, how many connections were opened. No test reaches
  into the Client, the Reader or reqwest.
- **Seam 1: Lua through a trusted `Runtime`, against a scripted local server.** A new `http`
  integration suite, gated on `stdlib-http`. Its server is a raw TCP listener on a thread, as the
  existing helpers in the stdlib suite are, extended so each test scripts the exchange: send the
  head, send body chunks with pauses between, stall, close mid-body, answer several requests on one
  connection, count connections, and hand back every request received. Raw TCP rather than an HTTP
  server crate, because stalls, cut bodies and garbage in place of TLS need byte-level control.
  Covered:
  - each method, the shortcuts, `query` encoding, and URL joining with and without `base_url`;
  - `body`, `json` and `form` on the wire, their content types, and two of them raising;
  - header merging across request, Client and defaults, case-insensitivity, and `false` removing
    a header, the `User-Agent` among them;
  - connection reuse through one Client, and through the default Client;
  - status, lower-cased and joined headers, non-UTF-8 header bytes, and the final URL after a
    redirect;
  - reading by chunk, line and rest; `bytes()` and `json()`; a Reader read from two tasks;
  - a gzip-compressed body decoded;
  - `events()` over a stream sent in pieces, ending at `[DONE]` by the script's `break`;
  - `break`, `<close>` and `close()` dropping the connection, seen by the server, and reads after
    close raising;
  - a body cut short raising `"body"`;
  - each timeout firing with its `phase`, the idle timeout per request, and `false` disabling one;
  - `check` off and on, the `"status"` error's fields, its 64 KiB cap and its message;
  - `"connect"` against a closed port, `"tls"` against a server that is not speaking TLS, and
    `"redirect"` on a loop;
  - the query and userinfo absent from messages and from `print(res)`;
  - retry on a status, on `Retry-After` in both forms, with its cap, giving up with the last
    failure, and never after the response was handed over;
  - cancelling a runtime waiting on a stalled request ends the wait.
- **Seam 2: the SSE parser on a fake line source.** The parser's unit tests, in the same suite,
  hand it a Lua table whose `line` method returns scripted lines, so the WHATWG cases are checked
  without a server: multi-line data, default and named events, `id`, comments, unknown fields,
  `retry` ignored, empty data not dispatched, bare `\r` endings, a field with no colon, a leading
  space after the colon stripped once, and a trailing partial event discarded.
- **`process`'s suite** stays as it is and must pass unchanged, so moving the Reader is proven not
  to alter it.
- **The compiled-in, features and sandbox suites** keep `http` where they have it; the feature
  matrix still builds `stdlib-http` alone. Tests in the stdlib suite that exercise Astra's API are
  removed or moved to the new suite in the new API.
- **Prior art.** The `process` suite for a stdlib module driven from Lua and its error tables; the
  stdlib suite's local-server helpers; the `tasks` and `limits` suites for cancelling a waiting
  runtime.

## Out of Scope

- Multipart bodies and file upload.
- Cookies and a cookie jar, and therefore a list form of `Set-Cookie`.
- WebSockets.
- A request body that streams from Lua.
- Proxy settings beyond reqwest's reading of the environment.
- Policy an embedder imposes, such as allowed hosts or keys injected into a Client from Rust.
- Reconnecting an event stream, and `Last-Event-ID`.
- Transcoding a body by its `charset`.
- Fixing the shared JSON encoder's empty-table and `null` gap; `json` inherits it.

## Further Notes

- The design was settled in an interview and is recorded in ADR 0025. **Client** and **Reader** are
  in the glossary, and **Buffer**'s entry was revised to match.
- The shared JSON encoder drops mlua's array marker, so `json = { tools = {} }` sends `"tools":{}`.
  It is a known gap, to be looked into after this lands.
- reqwest's own `read_timeout` and the Reader's idle timeout overlap. The Reader's is the one
  scripts see; whether reqwest's is also set, as a backstop, is left to the implementation.
