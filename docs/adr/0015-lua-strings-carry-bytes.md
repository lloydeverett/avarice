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
  HTTP allows; through `execute_streaming`, such a value became `""` altogether.
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
- **An HTTP response's header values, and `utils.env.get`, give exact bytes.** On Windows, where
  the environment is UTF-16 and has no bytes of its own, a value that is not valid Unicode is an
  error. Names, of headers and of variables, stay text: header names are ASCII by the protocol.
- **A table of byte values is still accepted as input** where Astra accepted one, since refusing it
  would break callers and gain nothing.

Each file changed records what changed under `Changes from the original:` in its header, as
ADR 0006 requires.

## Considered options

**Adding `Buffer:raw()` beside Astra's methods** kept to ADR 0006's rule and was rejected. It fixed
one method of six, and left `bytes()` returning a table under the name anyone would reach for.

**Fixing `Buffer` alone**, which the `process` module needs, was rejected. The same defect is in
`fs`, `http` and `crypto`, and a stdlib where some functions keep bytes and others do not is harder
to learn than one where none do.

## Consequences

Scripts written against Astra that call `:text()` or iterate the table from `:bytes()` break.
Taking a later Astra version means reapplying these changes by hand, file by file, from the headers.

A future module that returns or accepts bytes is held to the same rule: a Lua string, exactly the
bytes, and an error rather than a replacement where the operation needs text.
