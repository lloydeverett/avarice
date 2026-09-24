-- Original to avarice-rt: not derived from Astra. See ADR 0016.

---@meta

local process = {}

---A program to run, and how. The program and its arguments are the table's entries, passed one by
---one: nothing is split, quoted, expanded or globbed, since no shell is involved.
---
---```lua
---process.run({ "git", "log", "--oneline", cwd = "repo" })
---```
---@class Command
---The program. A bare name is searched for on the Child's `PATH`, and on Windows with each
---extension in `PATHEXT`; a path with a separator is relative to `cwd`.
---@field [1] string
---The Child's working directory. Default: the host's.
---@field cwd string?
---Variables to set, or with `false`, to remove.
---@field env table<string, string|false>?
---Start from an empty environment, before `env`. Default: `false`.
---@field clear_env boolean?
---A stream setting, or bytes to feed the Child and then close its input. Default: `"null"` for
---`run`, `"pipe"` for `spawn`. `run` refuses `"pipe"`, and closes an input `stdio` pipes at once.
---@field stdin "pipe"|"inherit"|"null"|string|Buffer?
---Default: `"pipe"`. `"inherit"` is the host's own stream, not `print`'s.
---@field stdout "pipe"|"inherit"|"null"?
---Default: `"pipe"`. `"stdout"` sends it wherever stdout goes, interleaved.
---@field stderr "pipe"|"inherit"|"null"|"stdout"?
---All three streams at once; a stream's own field wins over it.
---@field stdio "pipe"|"inherit"|"null"?
---`run` only: raise an `"exit"` error if the Child exits unsuccessfully.
---@field check boolean?
---`run` only: milliseconds before the Child is killed and a `"timeout"` error raised.
---@field timeout number?

---How a Child exited.
---@class ExitStatus
---@field ok boolean Whether it exited successfully.
---@field code integer? Its exit code; `nil` if a signal ended it.
---@field signal integer? On Unix, the signal that ended it.

---What a Child left behind. A stream that was not piped gives an empty Buffer, and so does
---`stderr` sent to stdout.
---@class Output: ExitStatus
---@field stdout Buffer
---@field stderr Buffer

---What `run` and `spawn` raise. Always a table: `tostring` gives its message, which names the
---program and never its arguments.
---@class ProcessError
---@field kind "start"|"exit"|"timeout"
---@field program string The program, as the Command gave it.
---@field reason "not_found"|"permission_denied"|"bad_cwd"|"other"|nil `"start"` only.
---@field message string? `"start"` only: the operating system's own words.
---@field output Output? `"exit"` and `"timeout"`: what was captured, up to the kill for a timeout.

---One of a Child's output streams. One task reads it at a time; a second is an error.
---@class Reader
---@field read fun(self: Reader): string? The next chunk, up to 8 KiB; `nil` at the end.
---The next line, without its `\n` or `\r\n`; `nil` at the end.
---@field line fun(self: Reader): string?
---@field lines fun(self: Reader): fun(): string? An iterator over `line`.
---@field rest fun(self: Reader): Buffer Everything left.

---A Child's input.
---@class Writer
---Raises once closed, or if the Child has stopped reading.
---@field write fun(self: Writer, bytes: string|Buffer)
---@field close fun(self: Writer)

---A running program.
---@class Child
---@field stdin Writer? `nil` unless piped.
---@field stdout Reader? `nil` unless piped.
---@field stderr Reader? `nil` unless piped, or when sent to stdout.
---Closes `stdin`, then waits for the Child to exit. It can wait forever if the Child fills a pipe
---nobody reads: use `output` to read them while waiting.
---@field wait fun(self: Child): ExitStatus
---Closes `stdin`, then reads `stdout` and `stderr` to their ends while waiting for the Child.
---@field output fun(self: Child): Output
---Ends it at once: `SIGKILL` on Unix. Only the Child, never programs it started.
---@field kill fun(self: Child)
---Asks it to end: `SIGTERM` on Unix, and the same as `kill` elsewhere.
---@field terminate fun(self: Child)
---@field pid fun(self: Child): integer

---@param fields table
---@param message string
local function raise(fields, message)
  error(setmetatable(fields, { __tostring = function() return message end }), 0)
end

---Runs a program to its end, and gives back what it wrote and how it exited. An unsuccessful exit
---is not an error unless `check` is set. A Child still running when its task or the script is
---aborted or cancelled is killed. A runtime's time limit does not interrupt the wait, so give the
---Command a `timeout` to bound it.
---@param command Command
---@return Output
function process.run(command)
  ---@diagnostic disable-next-line: undefined-global
  local output, fields, message = astra_internal__process_run(command)
  if output == nil then raise(fields, message) end
  return output
end

---Starts a program, and gives back the Child, whose streams are piped unless the Command says
---otherwise. The Child runs on even if Lua loses hold of it; `avrt` waits for it before exiting,
---and Ctrl-C kills it.
---@param command Command
---@return Child
function process.spawn(command)
  ---@diagnostic disable-next-line: undefined-global
  local child, fields, message = astra_internal__process_spawn(command)
  if child == nil then raise(fields, message) end
  return child
end

return process
