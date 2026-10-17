-- Original to avarice. A bundled module (ADR 0026): plain Lua, built on what the runtime registers.
-- It needs `http` and `serde`, and the `os` library for `os.getenv`, and says so when one is
-- missing.

---@meta

local openai = {}

local function refuse(message)
  error("openai: " .. message, 0)
end

-- `name`, required. A module the runtime does not register is refused with a message saying so;
-- any other error from loading it is raised as it is.
local function needed(name)
  local ok, module = pcall(require, name)
  if ok then return module end
  if tostring(module):find(("module '%s' not found"):format(name), 1, true) then
    refuse(("needs the %s module, which this runtime does not register"):format(name))
  end
  error(module, 0)
end

local http = needed("http")
local json = needed("serde").json
if type(os) ~= "table" or type(os.getenv) ~= "function" then
  refuse("needs the os library, for os.getenv, which this runtime does not open")
end

local DEFAULT_BASE_URL = "https://api.openai.com/v1"

---What a Client is made with. A field left out is read from the environment variable named
---beside it, when the Client is made; one set to the empty string counts as unset.
---@class OpenAIOptions
---Default: `OPENAI_BASE_URL`, else `https://api.openai.com/v1`.
---@field base_url string?
---Sent as `Authorization: Bearer <api_key>`. Default: `OPENAI_API_KEY`, else none, as a server
---on the local machine may want. `false` sends none whatever the environment holds.
---@field api_key string|false?
---The model of a request that names none. Default: `OPENAI_MODEL`.
---@field model string?
---Sent with every request, laid over the `Authorization` header. `false` removes one.
---@field headers table<string, string|false>?
---As `http.client` takes it. Default: `true`, so a 429 or a 5xx it lists is tried again.
---@field retry boolean|RetryOptions?
---@field connect_timeout number|false?
---Milliseconds to wait for each next part of a response, as `http.client` takes it: what bounds
---a stream that stalls.
---@field idle_timeout number|false?
---Milliseconds for a whole request, as `http.client` takes it. A stream's request lasts until its
---last chunk, so this cuts off a long stream: `idle_timeout` is usually what a stream wants.
---@field timeout number|false?

---What a stream raises when the server sends an error, or something that is not a chunk, rather
---than a chunk. A 4xx or 5xx status raises `http`'s `"status"` error, with the body in the
---message.
---@class OpenAIError
---@field kind "api"
---@field message string
---@field error any The `error` the server sent, decoded, or what it sent instead of a chunk.

---Sends to an OpenAI-compatible server. Its settings are fixed once it is made.
---@class OpenAIClient
---@field chat fun(self: OpenAIClient, request: table): table
---@field stream fun(self: OpenAIClient, request: table): fun(): table?, table?
---@field post fun(self: OpenAIClient, path: string, body: table): table
local Client = {}
Client.__index = Client

local OPTIONS = {
  base_url = true, api_key = true, model = true, headers = true, retry = true,
  connect_timeout = true, idle_timeout = true, timeout = true,
}

-- The environment variable `name`, or nil if it is unset or empty.
local function env(name)
  local value = os.getenv(name)
  if value == "" then return nil end
  return value
end

-- `value` checked to be a string that is not empty, or nil. `what` names it, for the message.
local function nonempty(value, what)
  if value ~= nil and (type(value) ~= "string" or value == "") then
    refuse(("%s is a string that is not empty"):format(what))
  end
  return value
end

-- Each Client's `http` Client, default model and `tostring`, out of Lua's reach.
local state = setmetatable({}, { __mode = "k" })

---Makes a Client for an OpenAI-compatible server.
---@param options OpenAIOptions?
---@return OpenAIClient
function openai.client(options)
  options = options or {}
  if type(options) ~= "table" then refuse("a Client's options are a table") end
  for key in pairs(options) do
    if not OPTIONS[key] then refuse(("a Client has no option %s"):format(tostring(key))) end
  end

  local base_url = nonempty(options.base_url, "a Client's base_url")
    or env("OPENAI_BASE_URL") or DEFAULT_BASE_URL
  local model = nonempty(options.model, "a Client's model") or env("OPENAI_MODEL")
  local api_key = options.api_key
  if api_key ~= false then
    api_key = nonempty(api_key, "a Client's api_key, or false for none") or env("OPENAI_API_KEY")
  end

  local headers = {}
  if api_key then headers.authorization = "Bearer " .. api_key end
  if options.headers ~= nil then
    if type(options.headers) ~= "table" then refuse("a Client's headers is a table") end
    for name, value in pairs(options.headers) do
      if type(name) ~= "string" then refuse("a Client's headers is keyed by strings") end
      headers[name:lower()] = value
    end
  end

  local retry = options.retry
  if retry == nil then retry = true end

  local client = setmetatable({}, Client)
  state[client] = {
    http = http.client {
      base_url = base_url, headers = headers, check = true, retry = retry,
      connect_timeout = options.connect_timeout, idle_timeout = options.idle_timeout,
      timeout = options.timeout,
    },
    model = model,
    shown = ("openai.Client(%s)"):format(base_url),
  }
  return client
end

function Client:__tostring()
  return state[self].shown
end

-- `request`, the body of a chat completion, checked and copied with `client`'s model if it
-- names none, so that the caller's table is never changed. `what` is the method, for messages.
local function completion(client, request, what)
  if type(request) ~= "table" then refuse(("%s takes a request table"):format(what)) end
  if request.stream ~= nil then
    refuse(("%s sets the request's stream itself: leave it out, and call %s"):format(
      what, what == "chat" and "stream for a streamed completion" or "chat for a whole one"))
  end
  local body = {}
  for key, value in pairs(request) do body[key] = value end
  body.model = nonempty(body.model, "a request's model") or state[client].model
  if body.model == nil then
    refuse(("%s needs a model: give the request one, or the Client, or set OPENAI_MODEL")
      :format(what))
  end
  return body
end

local function raise_api(message, err)
  error(setmetatable({ kind = "api", message = message, error = err }, {
    __tostring = function() return "openai: " .. message end,
  }), 0)
end

-- The message of `err`, an error the server sent: its `message`, or the whole of it.
local function api_message(err)
  if type(err) == "table" and type(err.message) == "string" then return err.message end
  if type(err) == "string" then return err end
  return json.encode(err)
end

---Sends `body` to `path` under the base URL, as JSON, and gives back the response, decoded. For
---the endpoints this module has no method for, such as `/embeddings`.
---@param path string
---@param body table
---@return table
function Client:post(path, body)
  if type(path) ~= "string" then refuse("post takes a path, such as /embeddings") end
  if type(body) ~= "table" then refuse("post takes a request table") end
  return state[self].http:post(path, { json = body }):json()
end

---A chat completion, whole: `request` is the body of `/chat/completions`, but for `stream`, which
---it refuses, and `model`, which the Client gives if it leaves it out. Gives back the response,
---decoded.
---@param request table
---@return table
function Client:chat(request)
  return self:post("/chat/completions", completion(self, request, "chat"))
end

---A chat completion, streamed: `request` as `chat` takes it, sent with `stream = true`. Iterates
---each chunk's first `delta`, `{}` for a chunk with no choices, and the whole chunk beside it,
---until the server sends `[DONE]` or ends the stream. Leaving the loop early closes the
---connection.
---@param request table
---@return fun(): table?, table?
function Client:stream(request)
  local body = completion(self, request, "stream")
  body.stream = true
  local res = state[self].http:post("/chat/completions", {
    json = body, headers = { accept = "text/event-stream" },
  })
  local events, done = res:events(), false
  return function()
    if done then return nil end
    for ev in events do
      if ev.data == "[DONE]" then break end
      local decoded, chunk = pcall(json.decode, ev.data)
      if not decoded or type(chunk) ~= "table" then
        if ev.event == "error" then raise_api(ev.data, ev.data) end
        raise_api(("the server sent %q, which is not a chunk"):format(ev.data), ev.data)
      end
      if ev.event == "error" or chunk.error ~= nil then
        local err = chunk.error
        if err == nil then err = chunk end
        raise_api(api_message(err), err)
      end
      local choice = chunk.choices and chunk.choices[1]
      return choice and choice.delta or {}, chunk
    end
    done = true
    return nil
  end, nil, nil, res
end

-- The default Client, made from the environment alone the first time it is needed, and kept, so
-- that its connections are reused: a change to the environment after that is not seen.
local default
local function default_client()
  default = default or openai.client()
  return default
end

---`chat` through a Client made from the environment alone, when first used.
---@param request table
---@return table
function openai.chat(request)
  return default_client():chat(request)
end

---`stream` through a Client made from the environment alone, when first used.
---@param request table
---@return fun(): table?, table?
function openai.stream(request)
  return default_client():stream(request)
end

return openai
