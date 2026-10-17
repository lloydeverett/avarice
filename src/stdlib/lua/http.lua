-- Original to avarice: not derived from Astra. See ADR 0025.

---@meta

local http = {}

---Bytes the host holds on Lua's behalf. `bytes` gives them as a string.
---@class Buffer
---@operator len: integer
---@field bytes fun(self: Buffer): string
---@field json fun(self: Buffer): any The bytes parsed as JSON.

---What a request sends, and how. Every field but `url` may be left out.
---@class Request
---A URL with a scheme, or one appended to the Client's `base_url`.
---@field url string
---Any method, in any case. Default: `"GET"`.
---@field method string?
---Laid over the Client's, name by name, whatever the case of the name. `false` removes a header,
---the `User-Agent` and `Accept-Encoding` avarice sends among them, but not `Accept`, which is
---always sent: give it a value instead.
---@field headers table<string, string|false>?
---Encoded and appended to the URL, after any query it has. Laid over the Client's; `false`
---removes a name.
---@field query table<string, string|false>?
---Sent byte for byte, with no `Content-Type` unless `headers` gives one.
---@field body string|Buffer?
---Encoded as `serde.json.encode` does, and sent as `application/json`.
---@field json any?
---Sent as `application/x-www-form-urlencoded`. Only one of `body`, `json` and `form` is given.
---@field form table<string, string>?
---Raise a `"status"` error for a 4xx or 5xx response, rather than return it. Default: `false`.
---@field check boolean?
---Try again after a failure to connect, a timeout before a response, or a status it lists:
---`true` for the defaults, a table to change them, `false` for none. Default: none. Never once
---the response has been returned, so a stream that breaks raises rather than starting again.
---@field retry boolean|RetryOptions?
---Milliseconds to wait for the response, and then for each next part of its body; `false` for
---no limit. Default: 300 000.
---@field idle_timeout number|false?
---Milliseconds for the whole request, from sending it to reading the last of its body, every
---retry and the waits between them included; `false` for no limit. Default: none.
---@field timeout number|false?

---@class RetryOptions
---@field attempts integer? Attempts in all, the first among them. Default: 3.
---@field statuses integer[]? Default: 429, 502, 503 and 504.
---The first wait, in milliseconds, doubled for each attempt after, with up to half of it taken
---off at random. A `Retry-After` replaces it. Default: 500.
---@field base_delay number?
---The longest wait, in milliseconds, whatever a `Retry-After` asks for. Default: 30 000.
---@field max_delay number?

---What a Client holds for every request through it.
---@class ClientOptions
---What a URL without a scheme is appended to, with one `/` between them.
---@field base_url string?
---@field headers table<string, string|false>?
---@field query table<string, string|false>?
---@field check boolean?
---@field retry boolean|RetryOptions?
---Milliseconds to connect, for every request through the Client. Default: 30 000.
---@field connect_timeout number|false?
---@field idle_timeout number|false?
---@field timeout number|false?

---Sends requests, and keeps its connections to reuse. Its settings are fixed once it is made.
---@class Client
---@field request fun(self: Client, request: Request): Response
---@field get fun(self: Client, url: string, options: Request?): Response
---@field post fun(self: Client, url: string, options: Request?): Response
---@field put fun(self: Client, url: string, options: Request?): Response
---@field patch fun(self: Client, url: string, options: Request?): Response
---@field delete fun(self: Client, url: string, options: Request?): Response
---@field head fun(self: Client, url: string, options: Request?): Response

---A response, as soon as its headers have arrived. Its body arrives as it is read. Closing it,
---or leaving the scope of a `<close>` variable holding it, drops its connection.
---@class Response
---@field status integer
---Keyed by lower-case name. A header sent more than once has its values joined with `", "`.
---@field headers table<string, string>
---The URL that answered, after any redirects.
---@field url string
---Its body, read as it arrives. Leaving a `lines` loop early closes the response.
---@field body Reader
---All that is left of the body.
---@field bytes fun(self: Response): string
---All that is left of the body, parsed as JSON.
---@field json fun(self: Response): any
---The body's server-sent events. Leaving the loop early closes the response.
---@field events fun(self: Response): fun(): Event?
---@field close fun(self: Response)

---A server-sent event.
---@class Event
---@field event string Its type: `"message"` unless it named one.
---@field data string Its data lines, joined with `\n`.
---@field id string? The last id the stream gave, in this event or an earlier one.

---What a request raises. Always a table: `tostring` gives its message. Neither holds the URL's
---query or userinfo.
---@class HTTPError
---@field kind "connect"|"tls"|"timeout"|"redirect"|"status"|"body"
---@field url string
---@field message string
---@field phase "connect"|"idle"|"total"|nil `"timeout"` only: which timeout ran out.
---@field status integer? `"status"` only.
---@field headers table<string, string>? `"status"` only.
---@field body Buffer? `"status"` only: the start of the body, up to 64 KiB.

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

local function refuse(message)
  error("http: " .. message, 0)
end

---@diagnostic disable: undefined-global
local new_client, redacted = astra_internal__http_client, astra_internal__http_redacted
---@diagnostic enable: undefined-global

-- The Rust half of each Client and response, and what `tostring` gives for each, out of Lua's
-- reach.
local raw = setmetatable({}, { __mode = "k" })
local shown = setmetatable({}, { __mode = "k" })

-- A Reader over `body`, the Rust half of a response's body. Closing it closes the response, so
-- the generic `for` that `lines` starts closes it when left early, by `break` or an error.
local function reader(body)
  local r = setmetatable({}, {
    __close = function() body:close() end,
    __tostring = function() return "Reader(body)" end,
  })
  function r:read() return checked(body:read()) end
  function r:line() return checked(body:line()) end
  function r:lines() return function() return checked(body:line()) end, nil, nil, r end
  function r:rest() return checked(body:rest()) end
  return r
end

local Response = {}
Response.__index = Response

local function response(body, status, url, headers, url_shown)
  local res = setmetatable({ status = status, url = url, headers = headers, body = reader(body) }, Response)
  raw[res] = body
  shown[res] = ("Response(%d %s)"):format(status, url_shown)
  return res
end

function Response:__tostring()
  return shown[self]
end

function Response:close()
  raw[self]:close()
end
Response.__close = Response.close

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

---The server-sent events in `source`, anything with a `line` method, such as a Reader: a Child's
---output, or a response's body. `res:events()` is this over `res.body`.
---@param source { line: fun(self): string? }
---@return fun(): Event?
function http.events(source)
  return event_iterator(source)
end

function Response:events()
  return event_iterator(self.body), nil, nil, self
end

-- Checking and merging options.

-- What may be in a request, and what a Client may hold for every request through it.
local REQUEST = {
  url = true, method = true, headers = true, query = true, body = true, json = true, form = true,
  check = true, retry = true, idle_timeout = true, timeout = true,
}
local CLIENT = {
  base_url = true, headers = true, query = true, check = true, retry = true, connect_timeout = true,
  idle_timeout = true, timeout = true,
}

-- Each timeout's default, in milliseconds; `false` for none.
local DEFAULT = { connect_timeout = 30000, idle_timeout = 300000, timeout = false }

-- What a `retry` table may hold, and what `retry = true` means.
local RETRY = { attempts = true, statuses = true, base_delay = true, max_delay = true }
local RETRY_DEFAULT = { attempts = 3, statuses = { 429, 502, 503, 504 }, base_delay = 500, max_delay = 30000 }

local function refuse_unknown(options, allowed, what)
  if type(options) ~= "table" then refuse(("%s is a table"):format(what)) end
  for key in pairs(options) do
    if not allowed[key] then refuse(("%s has no option %s"):format(what, tostring(key))) end
  end
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

local function boolean(value, what)
  if value ~= nil and type(value) ~= "boolean" then refuse(("%s is true or false"):format(what)) end
  return value
end

-- Whether `value` is a finite number of milliseconds, at least `least`.
local function milliseconds(value, least)
  return type(value) == "number" and value >= least and value < math.huge
end

-- `value` checked to be a timeout: a positive number of milliseconds, `false` for none, or nil
-- for the default.
local function timeout(value, what)
  if value ~= nil and value ~= false and not (milliseconds(value, 0) and value > 0) then
    refuse(("%s is a positive number of milliseconds, or false for none"):format(what))
  end
  return value
end

-- `value` checked as a `retry` option: true, false, nil, or a table of what RETRY names, copied.
local function retry_option(value, what)
  if value == nil or type(value) == "boolean" then return value end
  refuse_unknown(value, RETRY, what)
  local copy = {}
  if value.attempts ~= nil then
    if math.type(value.attempts) ~= "integer" or value.attempts < 1 then
      refuse(("%s's attempts is a whole number, at least 1"):format(what))
    end
    copy.attempts = value.attempts
  end
  if value.statuses ~= nil then
    if type(value.statuses) ~= "table" then refuse(("%s's statuses is a list of statuses"):format(what)) end
    copy.statuses = {}
    for key, status in pairs(value.statuses) do
      if math.type(key) ~= "integer" or key < 1 or key > #value.statuses then
        refuse(("%s's statuses is a list of statuses, not a table keyed by %s"):format(what, tostring(key)))
      end
      if math.type(status) ~= "integer" then
        refuse(("%s's statuses holds %s, not a status"):format(what, tostring(status)))
      end
      copy.statuses[key] = status
    end
  end
  for _, key in ipairs({ "base_delay", "max_delay" }) do
    if value[key] ~= nil then
      if not milliseconds(value[key], 0) then
        refuse(("%s's %s is a number of milliseconds"):format(what, key))
      end
      copy[key] = value[key]
    end
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

-- `headers`, a request's or a Client's, refused if they remove `accept`: reqwest sends
-- `Accept: */*` with any request that has none, so its removal could not be kept.
local function removable(headers, what)
  if headers and headers.accept == false then
    refuse(("%s cannot remove accept, which is sent whatever; give it a value instead"):format(what))
  end
  return headers
end

-- `fields` without the keys set to `false`, which remove what they would have replaced.
local function without_false(fields)
  local result = {}
  for name, value in pairs(fields) do
    if value ~= false then result[name] = value end
  end
  return result
end

-- The setting `key` resolves to, from a request, its Client's settings, and the default.
local function resolved(key, request, defaults)
  local value = request[key]
  if value == nil then value = defaults[key] end
  if value == nil then value = DEFAULT[key] end
  return value
end

-- The `retry` a request resolves to, from it and its Client's settings: RETRY_DEFAULT with what it
-- gives laid over it, or nil for none.
local function resolved_retry(request, defaults)
  local value = request.retry
  if value == nil then value = defaults.retry end
  if not value then return nil end
  return merged(RETRY_DEFAULT, value ~= true and value or nil)
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

-- `request`, checked and merged with `defaults`, a Client's settings, in the form the Rust half
-- sends.
local function prepared(request, defaults)
  refuse_unknown(request, REQUEST, "a request")
  if type(request.url) ~= "string" then refuse("a request needs a string url") end
  local method = request.method or "GET"
  if type(method) ~= "string" then refuse("a request's method is a string") end
  boolean(request.check, "a request's check")
  retry_option(request.retry, "a request's retry")
  timeout(request.idle_timeout, "a request's idle_timeout")
  timeout(request.timeout, "a request's timeout")

  local headers = merged(defaults.headers, removable(request.headers and strings(request.headers, "a request's headers", true, true), "a request's headers"))
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

  return {
    url = url, method = method:upper(), headers = headers, body = body, json = request.json,
    check = resolved("check", request, defaults) == true,
    retry = resolved_retry(request, defaults),
    idle_timeout = resolved("idle_timeout", request, defaults) or nil,
    timeout = resolved("timeout", request, defaults) or nil,
  }
end

-- Clients.

local Client = {}
Client.__index = Client

-- Each Client's settings, copied when it was made, out of Lua's reach.
local settings = setmetatable({}, { __mode = "k" })

---Makes a Client, which holds `options` for every request through it.
---@param options ClientOptions?
---@return Client
function http.client(options)
  options = options or {}
  refuse_unknown(options, CLIENT, "a Client")
  if options.base_url ~= nil and type(options.base_url) ~= "string" then
    refuse("a Client's base_url is a string")
  end
  local connect_timeout = timeout(options.connect_timeout, "a Client's connect_timeout")
  if connect_timeout == nil then connect_timeout = DEFAULT.connect_timeout end

  local client = setmetatable({}, Client)
  settings[client] = {
    base_url = options.base_url,
    headers = removable(options.headers and strings(options.headers, "a Client's headers", true, true), "a Client's headers"),
    query = options.query and strings(options.query, "a Client's query", true),
    check = boolean(options.check, "a Client's check"),
    retry = retry_option(options.retry, "a Client's retry"),
    idle_timeout = timeout(options.idle_timeout, "a Client's idle_timeout"),
    timeout = timeout(options.timeout, "a Client's timeout"),
  }
  raw[client] = new_client(connect_timeout or nil)
  shown[client] = ("Client(%s)"):format(options.base_url and redacted(options.base_url) or "")
  return client
end

function Client:__tostring()
  return shown[self]
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

-- `client:get(url, options)` and the rest send through a Client, and `http.get(url, options)` and
-- the rest through the runtime's default one.
for _, method in ipairs({ "get", "post", "put", "patch", "delete", "head" }) do
  local upper = method:upper()
  Client[method] = function(self, url, options)
    local request = {}
    for key, value in pairs(options or {}) do
      if key == "url" or key == "method" then
        refuse(("%s takes its %s from its name and its first argument, not its options"):format(method, key))
      end
      request[key] = value
    end
    request.url, request.method = url, upper
    return self:request(request)
  end
  http[method] = function(url, options)
    return Client[method](default_client(), url, options)
  end
end

---Sends `request` through the runtime's default Client.
---@param request Request
---@return Response
function http.request(request)
  return default_client():request(request)
end

return http
