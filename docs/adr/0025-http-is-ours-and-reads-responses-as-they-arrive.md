---
status: accepted
---

# `http` is ours, and Lua reads a response as it arrives

`http` is rewritten as a **stdlib module** original to avarice, like `process`
([ADR 0016](0016-process-runs-programs-without-a-shell.md)), replacing the client derived from Astra
under [ADR 0006](0006-stdlib-derived-from-astra.md). Nothing of Astra's API is kept: not
`http.request(...):execute()`, not the `set_*` builder, not `execute_streaming`, not
`http.status_codes`. There is no compatibility layer.

The use it is shaped around is streaming tokens from an OpenAI-compatible inference endpoint over
server-sent events. Astra's client could not do that well, and the reasons were in its shape, not
in reqwest, which stays underneath:

- `execute_streaming` ran its callback on a detached task. Its errors reached only a log, nothing
  said when the stream had ended, the callback could not await, and Lua could neither stop it nor
  wait for it.
- Every request built a new `reqwest::Client`, so no connection was ever reused.
- There were no timeouts, and a runtime's time limit does not end a wait (ADR 0004), so a stalled
  server hung the script.
- A body that failed to arrive was handed over as an empty one, against
  [ADR 0017](0017-the-stdlib-raises-rather-than-leaving-input-out.md); a table body was sent as
  bytes or as JSON by guessing; and a `Content-Type` the caller set in lower case was sent beside
  the one Astra added.

A reader would otherwise wonder why one stdlib module of Astra's was replaced outright rather than
amended, and why `http` looks like `process` rather than like the rest.

## The decision

- **Requests go through a Client.** `http.client{...}` makes a **Client**, which keeps its
  connections to reuse and holds defaults for every request through it: `base_url`, `headers`,
  `query`, `check`, `retry` and the timeouts. `http.request`, `http.get`, `http.post` and the rest
  send through a default Client, one per runtime. A Client's settings are fixed once it is made.
- **A request is one table.** `{ url, method, headers, query, body | json | form, check, retry,
  timeouts }`. At most one of `body` (a string or Buffer, sent byte for byte), `json` (encoded as
  `serde.json` encodes) and `form` is given, and two is an error. Only `json` and `form` imply a
  `Content-Type`.
- **Headers merge, and `false` removes one.** A request's headers win over its Client's, which win
  over avarice's own (`User-Agent: avarice/<version>`, `Accept-Encoding`). Names match whatever
  their case. A header set to `false` is not sent, whoever would have set it.
- **A URL without a scheme is appended to `base_url`**, with one `/` between them, rather than
  resolved against it as RFC 3986 would, which turns `https://host/v1` and `/chat/completions` into
  `https://host/chat/completions`.
- **A response arrives with its headers, and its body is a Reader.** `res.status`, `res.headers`
  (lower-case names, repeated values joined with `", "`, values as exact bytes) and `res.url` are
  fields. `res.body` is the same **Reader** as a **Child**'s output: `read`, `line`, `lines`,
  `rest`. `res:bytes()` and `res:json()` read all of it, as a Buffer's methods of those names do.
  Nothing transcodes a body by its `charset`
  ([ADR 0015](0015-lua-strings-carry-bytes.md)), so there is no `text()`.
- **Server-sent events are parsed for Lua.** `res:events()` iterates `{ event, data, id }` tables,
  parsed as WHATWG specifies, in Lua over `line`. It does not check `Content-Type`, does not
  reconnect, and leaves `[DONE]` and JSON to the caller.
- **Nothing takes a callback.** Lua asks for each piece and waits for it, as it does of a Child.
  Errors raise where Lua is reading, and a stream cut short raises rather than ending.
- **A response is to-be-closed.** `local res <close> = ...`, `res:close()`, and the closing value
  `lines` and `events` give a generic `for` all drop the connection, so breaking out of a token loop
  stops the generation. A request is not a task: when the coroutine waiting on it is aborted or
  cancelled, the request is dropped.
- **Three timeouts, in milliseconds.** `connect_timeout` (Client only, default 30 000),
  `idle_timeout` (longest wait for the next byte, headers included, default 300 000) and `timeout`
  (the whole request, body included, default none). `false` disables one. The idle default is
  generous because reasoning models can be silent for minutes before their first token, and finite
  because nothing else ends a stalled wait.
- **An error status is not an error unless `check` is set**, as with `process.run`. With it, a 4xx
  or 5xx raises a `"status"` error holding `status`, `headers` and up to 64 KiB of the body, whose
  start is in the message, since an API's explanation is always in the body.
- **What is raised is a table**, with a `kind` of `"connect"`, `"tls"`, `"timeout"` (with a
  `phase`), `"redirect"`, `"status"` or `"body"`, and a `__tostring`. A mistake in the call itself is
  an ordinary error. No message, and no `url` in an error, holds a URL's query or userinfo, where
  some APIs put keys.
- **Retrying is opt-in, and stops once Lua has the response.** `retry = { attempts, statuses,
  base_delay, max_delay }`, or `true` for defaults, retries connect failures, timeouts before a
  response and the listed statuses, with jittered exponential backoff that `Retry-After` replaces.
  A stream that breaks after Lua has its response raises, and is never silently started again.
- **Out of scope for now:** multipart and file upload, cookies, WebSockets, a request body that
  streams, proxies beyond reqwest's environment defaults, and policy an embedder imposes on hosts or
  keys. Each can come later without changing what is here.

## Considered options

**Amending Astra's client**, as ADR 0006 does for the other modules, was rejected: every fix above
changes its API, and a pull-based Reader replaces `execute_streaming` rather than repairing it.

**Keeping `execute_streaming` beside the Reader** was rejected for the reason ADR 0016 gives for
`process`: a callback on a detached task has nowhere to put its errors.

## Consequences

The Reader moves out of `process` to be shared. A Reader is told its errors by its source, since
`http`'s are tables with a `kind` and `process`'s are not.

`json` shares `serde.json`'s encoder, gap included: Astra's `sanetize_lua_input` rebuilds each
table and drops mlua's array marker, so an empty table is always sent as `{}`, never `[]`, and
there is no way to send `null`. Fixing that is a change to the shared encoder, not to `http`.
