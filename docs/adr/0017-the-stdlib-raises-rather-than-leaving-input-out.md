---
status: accepted
---

# The stdlib raises rather than leaving input out

When a stdlib function is given something it cannot use, it raises an error. It does not carry on
without it. A request that goes out missing what the script asked it to carry has succeeded at
something the script did not ask for, and nothing tells the script so.

This changes how Astra's code works, which [ADR 0006](0006-stdlib-derived-from-astra.md) otherwise
forbids, so, like [ADR 0015](0015-lua-strings-carry-bytes.md), it is an exception recorded as its
own ADR.

## What Astra did

`http`'s files to upload, given to `set_file` or as `file` in `http.request`'s table, could be
one `{ name, path }` table or a list of them. Astra read a table as one entry first, and if that
failed, as a list, skipping every value that was not a table and every entry that failed to read.
So a `{ name }` with no `path`, a list with a string in it, or `set_file(5)`, sent the request
without the file, and the request succeeded.

## The decision

- **An entry that cannot be read is an error when the request is sent**, naming what was wrong: a
  missing or non-string `name` or `path`, or a list holding something other than a table.
- **Something that is not a path, a table or `nil` is an error.** `nil` still means no file.
- **A table with a `name` or `path` key is one entry, and any other table is a list.** Astra
  decided by whether reading it as one entry failed, which is what let a broken entry fall
  through to the list reading and vanish. So a list that also has a `name` or `path` key is now
  read as one entry, and raises for its missing field, where Astra uploaded the list's entries.

`http/client/request.rs` records the change in its header, as ADR 0006 requires.

## Considered options

**Keeping Astra's order, one entry and then a list, but raising from the list reading** was
rejected. A broken single entry would then be reported as a list holding something other than a
table, an error about a mistake the script did not make.

## Consequences

A script that relied on a bad entry being skipped now fails where it sends the request. A future
module is held to the same rule: input it cannot use is an error, never dropped.
