-- Original to avarice: not derived from Astra. See ADR 0021.
--
-- Stores of files that outlast the program, on tidings <https://github.com/lloydeverett/tidings>.
-- A Store is one directory, its Location, held by one Backend: the filesystem, SQLite or memory.
-- Files are read directly, and written only by committing a Staging, all or nothing. Every Change
-- is reported on the Store's feed. tidings' README and glossary are the reference for what each
-- does. The differences are Lua's:
--   - A Conflict is returned, as `nil` and a table naming its Paths. Everything else raises.
--   - A Pending Commit has happened, so it is returned like a finished one, with `pending` set.
--   - A Revision is a string. A Prefix Revision is a value that prints as text but cannot be read
--     back from it.
--   - Times are `datetime` Timestamps.
--   - A Store, its feed and a Snapshot can each be closed, by `close` or `<close>`.
--   - An argument past the last one a function takes raises, where Lua would drop it.
-- An open Store on the filesystem or SQLite counts as a task: `avarice` waits for it to be closed
-- before exiting, and so does each line at the REPL.

---@meta

local tidings = {}

---A Store. Every method raises once it is closed, or if `abort_tasks` ended its tasks.
---@class tidings.Store
local Store = {}

---The File at `path`, or `nil` if there is none.
---@param path string
---@return tidings.File?
function Store:read(path) end

---The File at `path`'s Revision and last-modified time, without its contents, or `nil`.
---@param path string
---@return { revision: string, modified: datetime.Timestamp }?
function Store:stat(path) end

---The Paths under a Prefix, such as `"themes/"`, in order. With none, every Path in the Store.
---@param prefix string?
---@return string[]
function Store:list(prefix) end

---The state of everything under a Prefix, for `Staging:require_prefix`.
---@param prefix string? The whole Store if left out.
---@return tidings.PrefixRevision
function Store:stat_prefix(prefix) end

---Whether `snapshot` works: SQLite and memory, not the filesystem.
---@return boolean
function Store:supports_snapshots() end

---A view of the Store as it stands now, whatever is committed later.
---@return tidings.Snapshot
function Store:snapshot() end

---Commits a Staging, all or nothing. A Conflict gives `nil` and the Paths whose Preconditions
---failed, and writes nothing. A Staging can be committed once.
---@param staging tidings.Staging
---@return tidings.Committed?
---@return { paths: string[] }?
function Store:commit(staging) end

---Closes the Store. Its feed gives what it has, then ends. An operation in progress finishes.
function Store:close() end

---What a Commit gives back.
---@class tidings.Committed
---@field timestamp datetime.Timestamp Every File it wrote has this as its last-modified time.
---@field revisions table<string, string> The new Revision of each Path it wrote.
---Whether a File could not be replaced yet: the Commit has happened, and reads show it, but every
---Commit to the Store fails until it is finished.
---@field pending boolean

---A File that was read.
---@class tidings.File
---@field path string
---@field revision string
---@field modified datetime.Timestamp
local File = {}

---Its contents, exactly.
---@return string
function File:bytes() end

---Its contents, which must be valid UTF-8.
---@return string
function File:text() end

---What a write or a delete requires. Give one, or neither.
---@class tidings.Precondition
---@field if_absent boolean? There is no File at the Path.
---@field if_revision string? The File is unchanged since this Revision.

---Writes and deletes to commit together. Each method gives the Staging back, so calls chain.
---@class tidings.Staging
local Staging = {}

---@param path string
---@param contents string
---@param options tidings.Precondition?
---@return tidings.Staging
function Staging:write(path, contents, options) end

---@param path string
---@param options tidings.Precondition?
---@return tidings.Staging
function Staging:delete(path, options) end

---Deletes every File under a Prefix, such as `"themes/"`.
---@param prefix string
---@return tidings.Staging
function Staging:delete_prefix(prefix) end

---Requires, of a File it may not write, that it is absent or unchanged since a Revision.
---@param path string
---@param condition "absent"|string
---@return tidings.Staging
function Staging:require(path, condition) end

---Requires everything under a Prefix to be unchanged since `stat_prefix` gave `revision`.
---@param prefix string
---@param revision tidings.PrefixRevision
---@return tidings.Staging
function Staging:require_prefix(prefix, revision) end

---The state of everything under a Prefix, from `stat_prefix`. It prints as text, and compares
---with `==`, but cannot be made from text.
---@class tidings.PrefixRevision

---A Store as it stood when the Snapshot was taken, read as the Store is. It stays readable after
---the Store is closed.
---@class tidings.Snapshot
local Snapshot = {}

---The File at `path` as it stood, or `nil` if there was none.
---@param path string
---@return tidings.File?
function Snapshot:read(path) end

---The File at `path`'s Revision and last-modified time as they stood, or `nil`.
---@param path string
---@return { revision: string, modified: datetime.Timestamp }?
function Snapshot:stat(path) end

---The Paths under a Prefix as they stood, in order. With none, every Path.
---@param prefix string?
---@return string[]
function Snapshot:list(prefix) end

---Closes the Snapshot. A long-held SQLite Snapshot makes the database's log grow, so close it
---when done.
function Snapshot:close() end

---That one Path was changed or removed, and whether this Store did it or something else did.
---@class tidings.Change
---@field path string
---@field kind "changed"|"removed"
---@field origin "local"|"external"

---A Store's Change feed. One task reads it at a time; a second raises.
---@class tidings.Feed
local Feed = {}

---Waits for the next item: the Changes since the last, merged per Path, or a Resync, which says
---Changes may have been missed, so read again what you rely on. `nil` once the feed is closed, or
---once its Store is closed and everything recorded has been read.
---@return { changes: tidings.Change[] }|{ resync: true }|nil
function Feed:next() end

---Stops listening: Changes are no longer recorded, and a `next` waiting gives `nil`.
function Feed:close() end

---Opens the Store at a directory on the filesystem, making it if it is missing.
---@param location string
---@return tidings.Store
---@return tidings.Feed
function tidings.open_fs(location) end

---Opens the Store at a directory in SQLite, making it if it is missing.
---@param location string
---@return tidings.Store
---@return tidings.Feed
function tidings.open_sqlite(location) end

---A Store in memory, gone when it is closed.
---@return tidings.Store
---@return tidings.Feed
function tidings.open_memory() end

---Which Backend holds the Store at a directory, or `nil` if none does.
---@param location string
---@return "fs"|"sqlite"|nil
function tidings.detect(location) end

---A new, empty Staging: writes and deletes to commit to a Store together.
---@return tidings.Staging
function tidings.staging() end

-- Everything above is for LuaLS. What runs is this: the Rust half hands the module over as a value
-- (see `components/tidings.rs`), and this file returns it.
return ...
