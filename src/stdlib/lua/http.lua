-- Original to avarice: not derived from Astra. See ADR 0025.

---@meta

local http = {}

---@param fields table
---@param message string
local function raise(fields, message)
  error(setmetatable(fields, { __tostring = function() return message end }), 0)
end

-- What a primitive gave back, past its leading `true`; or, given `false`, the error it describes,
-- raised.
local function checked(ok, ...)
  if ok then return ... end
  raise(...)
end

-- The Rust half of each Client and response, out of Lua's reach.
local raw = setmetatable({}, { __mode = "k" })

-- A Reader over `body`, the Rust half of a response's body. Closing it closes the response, so
-- the generic `for` that `lines` starts closes it when left early, by `break` or an error.
local function reader(body)
  local r = setmetatable({}, { __close = function() body:close() end })
  function r:read() return checked(body:read()) end
  function r:line() return checked(body:line()) end
  function r:lines() return function() return checked(body:line()) end, nil, nil, r end
  function r:rest() return checked(body:rest()) end
  return r
end

local Response = {}
Response.__index = Response

-- Closing a response drops its connection, and reading its body after is an error.
function Response:close()
  raw[self]:close()
end
Response.__close = Response.close

local function response(body, status, url, headers)
  local res = setmetatable({ status = status, url = url, headers = headers, body = reader(body) }, Response)
  raw[res] = body
  return res
end

function Response:bytes()
  return self.body:rest():bytes()
end

function Response:json()
  return self.body:rest():json()
end

-- The events in `source`, anything with a `line` method that gives a line without its `\n` or
-- `\r\n` and nil at the end, as an iterator. Parsed as the WHATWG HTML standard's event stream
-- format says: https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation
-- `retry` is ignored, since nothing reconnects, and so are comments and unknown fields.
local function event_iterator(source)
  -- What is left of the last line `source` gave, split where a bare `\r` ends a line too.
  local pending, first = {}, true
  local function next_line()
    if pending[1] == nil then
      local line = source:line()
      if line == nil then return nil end
      if first then
        first = false
        line = line:gsub("^\239\187\191", "")
      end
      local start = 1
      while true do
        local cr = line:find("\r", start, true)
        pending[#pending + 1] = line:sub(start, cr and cr - 1)
        if not cr then break end
        start = cr + 1
      end
    end
    return table.remove(pending, 1)
  end

  -- The event being read: its data lines, nil until there is one, and its type. The last id
  -- seen carries over from one event to the next.
  local data, event_type, last_id = nil, "", nil
  return function()
    while true do
      local line = next_line()
      if line == nil then return nil end
      if line == "" then
        if data ~= nil then
          local event = { event = event_type ~= "" and event_type or "message", data = table.concat(data, "\n"), id = last_id }
          data, event_type = nil, ""
          return event
        end
        event_type = ""
      elseif line:sub(1, 1) ~= ":" then
        local field, value = line, ""
        local colon = line:find(":", 1, true)
        if colon then
          field, value = line:sub(1, colon - 1), line:sub(colon + 1)
          if value:sub(1, 1) == " " then value = value:sub(2) end
        end
        if field == "data" then
          data = data or {}
          data[#data + 1] = value
        elseif field == "event" then
          event_type = value
        elseif field == "id" and not value:find("\0", 1, true) then
          last_id = value
        end
      end
    end
  end
end

function http.events(source)
  return event_iterator(source)
end

function Response:events()
  return event_iterator(self.body), nil, nil, self
end

local Client = {}
Client.__index = Client

-- What may be in a request, and what a Client may hold for every request through it.
local REQUEST = { url = true, method = true, headers = true, query = true, body = true, json = true, form = true }
local CLIENT = { base_url = true, headers = true, query = true }

-- Each Client's settings, copied when it was made, out of Lua's reach.
local settings = setmetatable({}, { __mode = "k" })

local function refuse(message)
  error("http: " .. message, 0)
end

local function refuse_unknown(options, allowed, what)
  if type(options) ~= "table" then refuse(("%s is a table"):format(what)) end
  for key in pairs(options) do
    if not allowed[key] then refuse(("%s has no option %s"):format(what, tostring(key))) end
  end
end

-- `text` encoded as `application/x-www-form-urlencoded` encodes it, which leaves only ASCII
-- letters and digits and `*-._` as they are, and gives a space as `+`.
local function form_component(text)
  return (text:gsub("[^%w%*%-%._ ]", function(c) return ("%%%02X"):format(c:byte()) end):gsub(" ", "+"))
end

-- `fields`, a table of strings, encoded as a form or a query, in order of name so that it is the
-- same every time.
local function form_encoded(fields)
  local names = {}
  for name in pairs(fields) do names[#names + 1] = name end
  table.sort(names)
  for i, name in ipairs(names) do
    names[i] = form_component(name) .. "=" .. form_component(fields[name])
  end
  return table.concat(names, "&")
end

-- `fields` checked to be strings, keyed by strings, or with `allow_false`, `false` too; with
-- `lower`, keyed by the lower case of their names. A copy, so that changing `fields` afterwards
-- changes nothing.
local function strings(fields, what, allow_false, lower)
  if type(fields) ~= "table" then refuse(("%s is a table of strings"):format(what)) end
  local copy = {}
  for name, value in pairs(fields) do
    if type(name) ~= "string" then
      refuse(("%s is keyed by strings, not by %s"):format(what, tostring(name)))
    end
    if type(value) ~= "string" and not (allow_false and value == false) then
      refuse(("%s holds %s as a %s, not a string%s"):format(what, name, type(value), allow_false and " or false" or ""))
    end
    copy[lower and name:lower() or name] = value
  end
  return copy
end

-- `over` laid over `under`: a key in `over` replaces the same key in `under`.
local function merged(under, over)
  local result = {}
  for name, value in pairs(under or {}) do result[name] = value end
  for name, value in pairs(over or {}) do result[name] = value end
  return result
end

-- `fields` without the keys set to `false`, which remove what they would have replaced.
local function without_false(fields)
  local result = {}
  for name, value in pairs(fields) do
    if value ~= false then result[name] = value end
  end
  return result
end

-- `url` joined to `base_url`: appended to it, with one `/` between them, unless it has a scheme
-- of its own. Not RFC 3986's resolution, which would drop the last segment of a base such as
-- `https://host/v1`.
local function joined(base_url, url)
  if url:find("^%a[%w+.-]*:") then return url end
  if base_url == nil then
    refuse(("%s has no scheme, and there is no base_url to join it to"):format(url))
  end
  return (base_url:gsub("/+$", "")) .. "/" .. (url:gsub("^/+", ""))
end

function http.client(options)
  options = options or {}
  refuse_unknown(options, CLIENT, "a Client")
  if options.base_url ~= nil and type(options.base_url) ~= "string" then
    refuse("a Client's base_url is a string")
  end
  local client = setmetatable({}, Client)
  settings[client] = {
    base_url = options.base_url,
    headers = options.headers and strings(options.headers, "a Client's headers", true, true),
    query = options.query and strings(options.query, "a Client's query", true),
  }
  ---@diagnostic disable-next-line: undefined-global
  raw[client] = astra_internal__http_client()
  return client
end

-- `request`, checked and merged with `defaults`, in the form the Rust half sends.
local function prepared(request, defaults)
  refuse_unknown(request, REQUEST, "a request")
  if type(request.url) ~= "string" then refuse("a request needs a string url") end
  local method = request.method or "GET"
  if type(method) ~= "string" then refuse("a request's method is a string") end

  local headers = merged(defaults.headers, request.headers and strings(request.headers, "a request's headers", true, true))
  local query = without_false(merged(defaults.query, request.query and strings(request.query, "a request's query", true)))

  local url = joined(defaults.base_url, request.url)
  if next(query) ~= nil then
    url = url .. (url:find("?", 1, true) and "&" or "?") .. form_encoded(query)
  end

  local given = {}
  for _, key in ipairs({ "body", "json", "form" }) do
    if request[key] ~= nil then given[#given + 1] = key end
  end
  if #given > 1 then
    refuse(("a request has one body, but was given %s"):format(table.concat(given, " and ")))
  end

  local body, content_type = request.body, nil
  if type(body) == "userdata" then
    body = body:bytes()
  elseif body ~= nil and type(body) ~= "string" then
    refuse(("a request's body is a string or a Buffer, not a %s"):format(type(body)))
  end
  if request.form ~= nil then
    body = form_encoded(strings(request.form, "a request's form"))
    content_type = "application/x-www-form-urlencoded"
  elseif request.json ~= nil then
    content_type = "application/json"
  end
  if content_type and headers["content-type"] == nil then headers["content-type"] = content_type end

  return { url = url, method = method:upper(), headers = headers, body = body, json = request.json }
end

function Client:request(request)
  return response(checked(raw[self]:send(prepared(request, settings[self]))))
end

-- The default Client, made the first time it is needed. Each runtime loads this file once, so
-- each runtime has its own.
local default
local function default_client()
  default = default or http.client()
  return default
end

for _, method in ipairs({ "get", "post", "put", "patch", "delete", "head" }) do
  local upper = method:upper()
  Client[method] = function(self, url, options)
    local request = {}
    for key, value in pairs(options or {}) do request[key] = value end
    request.url, request.method = url, upper
    return self:request(request)
  end
  http[method] = function(url, options)
    return Client[method](default_client(), url, options)
  end
end

function http.request(request)
  return default_client():request(request)
end

return http
