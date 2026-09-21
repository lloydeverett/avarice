-- The runtime's `print`, written in Lua over two Rust functions (ADR 0005).
--
-- It is Lua so that its limits are the ones a Lua program already has: recursion into a nested
-- table is Lua recursion, which raises a catchable stack overflow rather than aborting the
-- process, and every string built here is charged to the memory cap.
--
-- Loaded once as a chunk when a runtime is built. It receives `write`, which appends a string to
-- the runtime's write sink, and `parameters`, which says what parameters a function written in Lua
-- takes (`"a, b, ..."`) and returns nil for one that is not written in Lua. It returns the `print`
-- function.
-- Nothing here is a global, so a script can replace `print` but cannot reach the sink except
-- through it.

local write, parameters = ...

-- Kept in locals so that a script redefining a global later does not change how `print` works,
-- which is also true of stock `print`.
local tostring, type, select, rawget, next = tostring, type, select, rawget, next
local string, table = string, table

-- A runtime built without the `string` or `table` library can still print scalars, and prints
-- tables the way stock `print` does.
if not (string and table) then
  return function(...)
    local out = ""
    for i = 1, select("#", ...) do
      if i > 1 then
        out = out .. "\t"
      end
      out = out .. tostring((select(i, ...)))
    end
    write(out .. "\n")
  end
end

local rep, gsub, find = string.rep, string.gsub, string.find
local concat, sort = table.concat, table.sort

local format = string.format

local INDENT = "  "

local KEYWORDS = {}
for word in ([[and break do else elseif end false for function goto if in local nil not or
repeat return then true until while]]):gmatch("%a+") do
  KEYWORDS[word] = true
end

-- What `print` shows for `value` on one line, or nil if it is a table that has no string form of
-- its own, which is the case this file exists to print better. A table has one if its metatable
-- has `__tostring` or `__name`, and asking `tostring` is the only way to find out that respects a
-- protected metatable, which `getmetatable` would hide. The check is that the answer is not just
-- the address, and it runs `__tostring` once, so the caller uses the text rather than asking again.
--
-- That is what stock `print` shows, except that a function is followed by its parameters, so
-- `function: 0x55d0(a, b, ...)`. A function not written in Lua, such as `string.format`, has no
-- parameters to show, and is left as `function: 0x55d0` rather than claim it takes none.
--
-- The same goes for the runtime built without `string` or `table`, below: it prints every function
-- as `tostring` does.
local function line_text(value)
  local text = tostring(value)
  local kind = type(value)
  if (kind == "table" or kind == "function") and text == kind .. ": " .. format("%p", value) then
    if kind == "table" then
      return nil
    end
    local names = parameters(value)
    if names then
      return text .. "(" .. names .. ")"
    end
  end
  return text
end

-- A string inside a table, quoted so that the structure stays unambiguous. `%q` writes a newline
-- as a backslash and a real newline, which would break the layout, so it is respelled `\n`.
local function quote(text)
  return (gsub(format("%q", text), "\\\n", "\\n"))
end

local function key_text(key)
  if type(key) == "string" then
    if find(key, "^[%a_][%w_]*$") and not KEYWORDS[key] then
      return key
    end
    return "[" .. quote(key) .. "]"
  end
  return "[" .. tostring(key) .. "]"
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
    out[#out + 1] = quote(value)
    return
  end
  local text = line_text(value)
  if text then
    out[#out + 1] = text
    return
  end
  if ancestors[value] then
    out[#out + 1] = "<cycle: " .. tostring(value) .. ">"
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
    out[#out + 1] = ",\n"
  end
  for i = 1, count do
    local key = keys[i]
    out[#out + 1] = inner .. key_text(key) .. " = "
    render(rawget(value, key), depth + 1, ancestors, out)
    out[#out + 1] = ",\n"
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
