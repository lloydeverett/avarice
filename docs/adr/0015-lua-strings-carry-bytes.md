---
status: accepted
---

# Lua strings carry bytes, and the stdlib passes them through exactly

A Lua string is a sequence of bytes, not of characters. Wherever bytes cross between Lua and a
**stdlib module**, in either direction, they cross as a Lua string holding exactly those bytes.
Nothing is replaced, and nothing is refused for not being UTF-8. An operation that is textual by
nature, such as parsing JSON, fails on invalid UTF-8 with an error; it never quietly replaces what
it cannot read.

This changes how Astra's code works, which [ADR 0006](0006-stdlib-derived-from-astra.md) otherwise
forbids. It is recorded here, and not as a sixth amendment to 0006, because it is a rule every
future module must also follow, not a note about Astra's files.

## What Astra did

Astra treats bytes as text or as tables of numbers, and both lose. Before this decision:

- `Buffer:bytes()` returned a table with one integer per byte. Every slot is a full Lua value, so
  2 million bytes took 32 MiB, and turning the table back into a string with
  `string.char(table.unpack(t))` fails beyond about a million entries.
- `Buffer:text()` and `Buffer:json()` replaced invalid UTF-8 with U+FFFD before returning or
  parsing.
- `fs.read_file_bytes` returned the same kind of table.
- `fs.write_file` and `HTTPClientRequest:set_body` took a string through `to_string_lossy`, so
  writing or sending binary data given as a string corrupted it.
- `crypto.hash` and `crypto.base64.encode` refused any input that was not UTF-8, and
  `crypto.base64.decode` replaced invalid UTF-8 in its output, so `/wAB` came back as
  `EF BF BD 00 01` and not as `FF 00 01`.
- An HTTP response's header values replaced bytes that are not UTF-8, such as Latin-1 text, which
  HTTP allows. Through `execute_streaming`, a value became `""` altogether if it held anything but
  visible ASCII, even valid UTF-8 such as `café`.
- `utils.env.get` gave `nil` for a variable whose value was not UTF-8, as if it were unset.

None of this could be fixed by adding: a second, correct method beside `bytes()` would leave the
wrong one where every reader looks first.

## The decision

- **`Buffer:bytes()` returns a Lua string** holding the buffer's exact bytes.
- **`Buffer:text()` is removed.** With `bytes()` returning a string, `text()` could only differ by
  replacing invalid UTF-8, which is the defect.
- **`Buffer:json()` stays, and fails on invalid UTF-8** rather than replacing it.
- **A Buffer has a length**, through `__len`, so `#buffer` does not copy the bytes into Lua.
- **`fs.read_file_bytes` returns a string.** It differs from `fs.read_file` only in that
  `read_file` fails on a file that is not UTF-8.
- **`fs.write_file`, `set_body`, `crypto.hash` and `crypto.base64` take and give exact bytes.**
- **What arrives from outside gives exact bytes: an HTTP response's header values, and the value
  `utils.env.get` reads.** A script does not choose these, so nothing about them can be assumed,
  and nearly all are ASCII or UTF-8 anyway; `utf8.len(value) ~= nil` checks. Outside Unix, where
  the environment is UTF-16 and a value has no bytes of its own, a value that is not valid Unicode
  is an error naming the variable.
- **What a script writes itself stays text: the header names and values it gives a request, and
  the name it gives `utils.env.get`.** Each must be UTF-8, and one that is not is an error, before
  anything is sent. [RFC 9110](https://www.rfc-editor.org/rfc/rfc9110#section-5.5) asks new header
  fields to keep to ASCII, and a script has no reason to send anything else. A response's header
  names stay text too: they are ASCII by the protocol.
- **A table of byte values is still accepted as input** where Astra accepted one, since refusing it
  would break callers and gain nothing.

Each file changed records what changed under `Changes from the original:` in its header, as
ADR 0006 requires.

## Considered options

**Adding `Buffer:raw()` beside Astra's methods** kept to ADR 0006's rule and was rejected. It fixed
one method of six, and left `bytes()` returning a table under the name anyone would reach for.

**Response header values that must be UTF-8**, so a script could treat every header as text,
was rejected, because nothing good can be done with a value that is not. Failing the request loses
its status and body over one odd cookie. Dropping the header, or replacing the bytes, is the silent
loss this decision exists to end. Decoding as Latin-1, as Python's `http.client` and Node do,
always gives valid UTF-8 and can be undone, but turns a raw UTF-8 value, the commonest non-ASCII
one, into mojibake (`café` as `cafÃ©`), and a script cannot tell which values were decoded.

**Fixing `Buffer` alone**, which the `process` module needs, was rejected. The same defect is in
`fs`, `http` and `crypto`, and a stdlib where some functions keep bytes and others do not is harder
to learn than one where none do.

## Consequences

Scripts written against Astra that call `:text()` or iterate the table from `:bytes()` break.
Taking a later Astra version means reapplying these changes by hand, file by file, from the headers.

A future module that returns or accepts bytes is held to the same rule: a Lua string, exactly the
bytes, and an error rather than a replacement where the operation needs text.
