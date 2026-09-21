-- The runtime's `print`, written in Lua over two Rust functions (ADR 0005).
--
-- It is Lua so that its limits are the ones a Lua program already has: recursion into a nested
-- table is Lua recursion, which raises a catchable stack overflow rather than aborting the
-- process, and every string built here is charged to the memory cap.
--
-- Loaded once as a chunk when a runtime is built. It receives `write`, which appends a string to
-- the runtime's write sink, `parameters`, which says what parameters a function written in Lua
-- takes (`"a, b, ..."`) and returns nil for one that is not written in Lua, and `ansi`, the core
-- `ansi` module evaluated for this chunk alone. It returns the `print` function.
-- Nothing here is a global, so a script can replace `print` but cannot reach the sink except
-- through it.
--
-- What `print` shows inside a table is highlighted (ADR 0011): always, whether or not anyone can
-- see colour. Removing it where it cannot be shown is the destination's business, which `avrt`
-- does and an embedder's sink may not.
--
-- `string` and `table` are assumed. The runtime refuses to be built without them.

local write, parameters, ansi = ...

-- Kept in locals so that a script redefining a global later does not change how `print` works,
-- which is also true of stock `print`.
local tostring, type, select, rawget, next = tostring, type, select, rawget, next
local string, table = string, table

local rep, gsub, find = string.rep, string.gsub, string.find
local concat, sort = table.concat, table.sort

local format = string.format

-- The codes, copied out of `ansi` now so that nothing a script does later can change them. A
-- token is a code, its text, and a full reset.
local RESET = ansi.reset
local DIM = ansi.dim
local GREEN, CYAN = ansi.fg.green, ansi.fg.cyan
local YELLOW, RED, MAGENTA = ansi.fg.yellow, ansi.fg.red, ansi.fg.magenta

local function paint(code, text)
  return code .. text .. RESET
end

local COMMA = paint(CYAN, ",")
local FUNCTION = paint(RED, "function")

local INDENT = "  "

-- A key that is one of these has to be bracketed to be valid Lua.
local KEYWORDS = {
  ["and"] = true, ["break"] = true, ["do"] = true, ["else"] = true, ["elseif"] = true,
  ["end"] = true, ["false"] = true, ["for"] = true, ["function"] = true, ["goto"] = true,
  ["if"] = true, ["in"] = true, ["local"] = true, ["nil"] = true, ["not"] = true,
  ["or"] = true, ["repeat"] = true, ["return"] = true, ["then"] = true, ["true"] = true,
  ["until"] = true, ["while"] = true,
}

-- The plain string forms of `nil` and the booleans, which are the ones `line_text` highlights.
local LITERAL = { ["nil"] = true, ["true"] = true, ["false"] = true }

-- What `print` shows for `value` on one line, or nil if it is a table that has no string form of
-- its own, which is the case this file exists to print better. A table has one if its metatable
-- has `__tostring` or `__name`, and asking `tostring` is the only way to find out that respects a
-- protected metatable, which `getmetatable` would hide. The check is that the answer is not just
-- the address, and it runs `__tostring` once, so the caller uses the text rather than asking again.
--
-- That is what stock `print` shows, except for two things. A function is laid out as
-- `function (a, b, ...) [0x55d0]`, with its parameters as its definition spells them, and
-- one not written in Lua, such as `string.format`, has no parameters to show and is
-- `function [0x55d0]` rather than claim it takes none. And `true`, `false` and `nil` are
-- highlighted. A function with a string form of its own keeps it.
local function line_text(value)
  local text = tostring(value)
  local kind = type(value)
  if kind == "table" then
    if text == "table: " .. format("%p", value) then
      return nil
    end
  elseif kind == "function" then
    local address = format("%p", value)
    if text == "function: " .. address then
      local names = parameters(value)
      local params = ""
      if names then
        params = " (" .. gsub(names, ", ", COMMA .. " ") .. ")"
      end
      return FUNCTION .. params .. " " .. paint(DIM, "[" .. address .. "]")
    end
  elseif (kind == "boolean" or kind == "nil") and LITERAL[text] then
    return paint(MAGENTA, text)
  end
  return text
end

-- A string inside a table, quoted so that the structure stays unambiguous. `%q` writes a newline
-- as a backslash and a real newline, which would break the layout, so it is respelled `\n`.
local function quote(text)
  return (gsub(format("%q", text), "\\\n", "\\n"))
end

-- The whole key is one token, brackets and quotes with it, so a quoted key is not green.
local function key_text(key)
  if type(key) == "string" then
    if find(key, "^[%a_][%w_]*$") and not KEYWORDS[key] then
      return paint(YELLOW, key)
    end
    return paint(YELLOW, "[" .. quote(key) .. "]")
  end
  return paint(YELLOW, "[" .. tostring(key) .. "]")
end

-- Numbers, then strings, then booleans, then everything else, so that a table prints the same way
-- each time. Nothing here compares across types. The last group, keys that are themselves tables,
-- functions or the like, is ordered by address, and so is not the same from one run to the next.
local NUMBER, STRING, BOOLEAN, OTHER = 1, 2, 3, 4
local RANK = { number = NUMBER, string = STRING, boolean = BOOLEAN }

local function key_less(a, b)
  local rank_a, rank_b = RANK[type(a)] or OTHER, RANK[type(b)] or OTHER
  if rank_a ~= rank_b then
    return rank_a < rank_b
  end
  if rank_a == NUMBER or rank_a == STRING then
    return a < b
  end
  if rank_a == BOOLEAN then
    return (not a) and b
  end
  return tostring(a) < tostring(b)
end

-- Appends the text of a value found inside a table to `out`. `ancestors` holds the tables between
-- here and the top, so that a table that contains itself is marked rather than followed.
local function render(value, depth, ancestors, out)
  if type(value) == "string" then
    out[#out + 1] = paint(GREEN, quote(value))
    return
  end
  local text = line_text(value)
  if text then
    out[#out + 1] = text
    return
  end
  if ancestors[value] then
    out[#out + 1] = paint(DIM, "<cycle: " .. tostring(value) .. ">")
    return
  end

  -- Raw access throughout: printing must not run a script's `__index` or `__pairs`.
  local length = 0
  while rawget(value, length + 1) ~= nil do
    length = length + 1
  end
  local keys, count = {}, 0
  for key in next, value do
    if not (type(key) == "number" and key >= 1 and key <= length and key % 1 == 0) then
      count = count + 1
      keys[count] = key
    end
  end
  if length == 0 and count == 0 then
    out[#out + 1] = "{}"
    return
  end
  sort(keys, key_less)

  ancestors[value] = true
  local inner = rep(INDENT, depth + 1)
  out[#out + 1] = "{\n"
  for i = 1, length do
    out[#out + 1] = inner
    render(rawget(value, i), depth + 1, ancestors, out)
    out[#out + 1] = COMMA .. "\n"
  end
  for i = 1, count do
    local key = keys[i]
    out[#out + 1] = inner .. key_text(key) .. " = "
    render(rawget(value, key), depth + 1, ancestors, out)
    out[#out + 1] = COMMA .. "\n"
  end
  out[#out + 1] = rep(INDENT, depth) .. "}"
  ancestors[value] = nil
end

-- Everything is built before anything is written, so an error partway through leaves the sink
-- untouched rather than holding half a table.
return function(...)
  local n = select("#", ...)
  local parts = {}
  for i = 1, n do
    local value = (select(i, ...))
    local text = line_text(value)
    if text then
      parts[i] = text
    else
      local out = {}
      render(value, 0, {}, out)
      parts[i] = concat(out)
    end
  end
  write(concat(parts, "\t") .. "\n")
end
