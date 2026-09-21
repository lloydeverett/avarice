-- The runtime's `print`, written in Lua over one Rust function (ADR 0005).
--
-- It is Lua so that its limits are the ones a Lua program already has: recursion into a nested
-- table is Lua recursion, which raises a catchable stack overflow rather than aborting the
-- process, and every string built here is charged to the memory cap.
--
-- Loaded once as a chunk when a runtime is built. It receives `write`, which appends a string to
-- the runtime's write sink, and returns the `print` function. Nothing here is a global, so a
-- script can replace `print` but cannot reach the sink except through it.

local write = ...

-- Kept in locals so that a script redefining a global later does not change how `print` works,
-- which is also true of stock `print`.
local tostring, type, select, getmetatable, rawget, next =
  tostring, type, select, getmetatable, rawget, next
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

local rep, format, gsub, find = string.rep, string.format, string.gsub, string.find
local concat, sort = table.concat, table.sort

local INDENT = "  "

local KEYWORDS = {}
for word in ([[and break do else elseif end false for function goto if in local nil not or
repeat return then true until while]]):gmatch("%a+") do
  KEYWORDS[word] = true
end

-- A table is printed structurally unless it says how to print itself, which is what stock `print`
-- honours too.
local function is_structural(value)
  if type(value) ~= "table" then
    return false
  end
  local metatable = getmetatable(value)
  return not (type(metatable) == "table" and rawget(metatable, "__tostring") ~= nil)
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
-- each time. Nothing here compares across types.
local RANK = { number = 1, string = 2, boolean = 3 }

local function key_less(a, b)
  local rank_a, rank_b = RANK[type(a)] or 4, RANK[type(b)] or 4
  if rank_a ~= rank_b then
    return rank_a < rank_b
  end
  if rank_a <= 2 then
    return a < b
  end
  if rank_a == 3 then
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
  if not is_structural(value) then
    out[#out + 1] = tostring(value)
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
    if is_structural(value) then
      local out = {}
      render(value, 0, {}, out)
      parts[i] = concat(out)
    else
      parts[i] = tostring(value)
    end
  end
  write(concat(parts, "\t") .. "\n")
end
