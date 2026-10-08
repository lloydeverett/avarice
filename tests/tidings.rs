//! The `tidings` module (ADR 0021), driven the way a script does: Lua through a trusted runtime.
//!
//! tidings has its own tests of what a Store does, so these check the binding: what reaches Lua,
//! what raises, and what the Lua-shaped parts (a Conflict returned rather than raised, `close`, a
//! Change feed with one reader, an open Store counting as a task) mean. Most use the memory
//! Backend; the filesystem and SQLite ones are checked to open, commit and count as tasks.

#![cfg(feature = "stdlib-tidings")]

mod common;

use avarice::{Profile, Runtime};
use common::TempDir;

/// Runs `body` with `tidings` bound to the module, and returns what it returns.
fn eval<R: mlua::FromLuaMulti>(body: &str) -> R {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    eval_in(&rt, body)
}

fn eval_in<R: mlua::FromLuaMulti>(rt: &Runtime, body: &str) -> R {
    let source = format!("local tidings = require('tidings')\n{body}");
    rt.block_on(rt.eval(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

/// Runs `body` with `tidings` bound to the module, expecting it to raise, and returns the message.
fn raises(body: &str) -> String {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    raises_in(&rt, body)
}

fn raises_in(rt: &Runtime, body: &str) -> String {
    let source = format!(
        "local tidings = require('tidings')\n\
         local ok, err = pcall(function() {body} end)\n\
         assert(not ok, 'did not raise')\n\
         return tostring(err)"
    );
    rt.block_on(rt.eval::<String>(source, "=test"))
        .unwrap_or_else(|e| panic!("{body}: {e}"))
}

/// A directory as a Lua long string, for a script to open a Store at.
fn lua_path(dir: &std::path::Path) -> String {
    format!("[==[{}]==]", dir.display())
}

// -- Reading and writing -----------------------------------------------------------------------

#[test]
fn a_committed_file_reads_back_as_text_and_bytes() {
    let got: (String, String, String) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('notes/a.md', '# A\\n')
         store:commit(s)
         local file = store:read('notes/a.md')
         return file.path, file:text(), file:bytes()",
    );
    assert_eq!(got, ("notes/a.md".into(), "# A\n".into(), "# A\n".into()));
}

#[test]
fn contents_that_are_not_text_pass_through_exactly_and_text_raises() {
    let got: (bool, bool) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('blob', '\\255\\0\\1')
         store:commit(s)
         local file = store:read('blob')
         return file:bytes() == '\\255\\0\\1', pcall(file.text, file)",
    );
    assert_eq!(got, (true, false));
    let message = raises(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('blob', '\\255')
         store:commit(s)
         store:read('blob'):text()",
    );
    assert!(message.contains("not valid UTF-8"), "{message}");
}

#[test]
fn a_missing_file_reads_and_stats_as_nil() {
    let got: (bool, bool) = eval(
        "local store = tidings.open_memory()
         return store:read('nope.txt') == nil, store:stat('nope.txt') == nil",
    );
    assert_eq!(got, (true, true));
}

#[test]
fn stat_and_read_agree_and_modified_is_a_timestamp() {
    let got: (bool, bool, bool, String) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'a')
         local committed = store:commit(s)
         local file, stat = store:read('a.txt'), store:stat('a.txt')
         return file.revision == stat.revision, file.modified == stat.modified,
                file.modified == committed.timestamp, type(file.revision)",
    );
    assert_eq!(got, (true, true, true, "string".into()));
    // A Timestamp from `datetime`: it has jiff's methods.
    let seconds: i64 = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'a')
         return store:commit(s).timestamp:as_second()",
    );
    assert!(seconds > 1_700_000_000, "{seconds}");
}

#[test]
fn list_gives_the_paths_under_a_prefix_or_the_whole_store() {
    let got: (String, String) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('b.txt', 'b'):write('themes/dark.toml', 'd'):write('a.txt', 'a')
         store:commit(s)
         return table.concat(store:list(), ','), table.concat(store:list('themes/'), ',')",
    );
    assert_eq!(
        got,
        (
            "a.txt,b.txt,themes/dark.toml".into(),
            "themes/dark.toml".into()
        )
    );
}

#[test]
fn delete_and_delete_prefix_remove_files() {
    let got: String = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'a'):write('t/1', '1'):write('t/2', '2'):write('keep', 'k')
         store:commit(s)
         s = tidings.staging()
         s:delete('a.txt'):delete_prefix('t/')
         store:commit(s)
         return table.concat(store:list(), ',')",
    );
    assert_eq!(got, "keep");
}

#[test]
fn an_invalid_path_raises_where_it_is_staged() {
    let message = raises("tidings.staging():write('CON', 'x')");
    assert!(message.contains("invalid path"), "{message}");
    let message = raises("tidings.open_memory():list('themes')");
    assert!(message.contains("invalid path"), "{message}");
}

#[test]
fn contents_must_be_a_string() {
    let message = raises("tidings.staging():write('a.txt', 5)");
    assert!(message.contains("string"), "{message}");
}

// -- Preconditions and Conflicts ---------------------------------------------------------------

#[test]
fn a_conflict_is_returned_naming_its_paths_and_writes_nothing() {
    let got: (bool, String, String) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'first')
         store:commit(s)
         s = tidings.staging()
         s:write('a.txt', 'second', { if_absent = true }):write('b.txt', 'b')
         local committed, conflict = store:commit(s)
         return committed == nil, table.concat(conflict.paths, ','),
                table.concat(store:list(), ',') .. '=' .. store:read('a.txt'):text()",
    );
    assert_eq!(got, (true, "a.txt".into(), "a.txt=first".into()));
}

#[test]
fn if_revision_holds_for_the_revision_read_and_not_for_a_stale_one() {
    let got: (bool, bool) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'one')
         store:commit(s)
         local stale = store:read('a.txt').revision
         s = tidings.staging()
         s:write('a.txt', 'two', { if_revision = stale })
         local ok = store:commit(s) ~= nil
         s = tidings.staging()
         s:delete('a.txt', { if_revision = stale })
         local again = store:commit(s) ~= nil
         return ok, again",
    );
    assert_eq!(got, (true, false));
}

#[test]
fn require_adds_a_precondition_on_a_file_not_written() {
    let got: (bool, String) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('lock', 'x')
         store:commit(s)
         s = tidings.staging()
         s:write('a.txt', 'a'):require('lock', 'absent')
         local committed, conflict = store:commit(s)
         return committed == nil, table.concat(conflict.paths, ',')",
    );
    assert_eq!(got, (true, "lock".into()));
}

#[test]
fn a_prefix_revision_compares_and_holds_until_the_prefix_changes() {
    let got: (bool, bool, bool, bool) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('t/1', '1')
         store:commit(s)
         local before = store:stat_prefix('t/')
         local same = before == store:stat_prefix('t/')
         s = tidings.staging()
         s:write('t/2', '2')
         store:commit(s)
         local changed = before ~= store:stat_prefix('t/')
         s = tidings.staging()
         s:write('other', 'o'):require_prefix('t/', before)
         local held = store:commit(s) ~= nil
         s = tidings.staging()
         s:write('other', 'o'):require_prefix('t/', store:stat_prefix('t/'))
         return same, changed, held, store:commit(s) ~= nil",
    );
    assert_eq!(got, (true, true, false, true));
}

#[test]
fn a_prefix_revision_prints_as_text() {
    let got: bool = eval(
        "local p = tidings.open_memory():stat_prefix('')
         return type(tostring(p)) == 'string' and #tostring(p) > 0",
    );
    assert!(got);
}

#[test]
fn an_unknown_or_contradictory_option_raises() {
    let message = raises("tidings.staging():write('a', 'a', { if_absnet = true })");
    assert!(message.contains("unknown key 'if_absnet'"), "{message}");
    let message =
        raises("tidings.staging():write('a', 'a', { if_absent = true, if_revision = 'x' })");
    assert!(
        message.contains("if_absent") && message.contains("if_revision"),
        "{message}"
    );
    let message = raises("tidings.staging():require('a', 'not a revision')");
    assert!(message.contains("Revision"), "{message}");
}

#[test]
fn a_staging_cannot_be_committed_twice() {
    let message = raises(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a', 'a')
         store:commit(s)
         s:write('b', 'b')",
    );
    assert!(message.contains("already committed"), "{message}");
}

// -- What a commit gives back ------------------------------------------------------------------

#[test]
fn a_commit_gives_the_revisions_of_exactly_the_paths_it_wrote() {
    let got: (bool, bool, bool, bool) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('gone', 'g')
         store:commit(s)
         s = tidings.staging()
         s:write('a.txt', 'a'):delete('gone')
         local committed = store:commit(s)
         return committed.revisions['a.txt'] == store:read('a.txt').revision,
                committed.revisions['gone'] == nil, committed.pending == false,
                next(committed.revisions, 'a.txt') == nil",
    );
    assert_eq!(got, (true, true, true, true));
}

// -- The Change feed ---------------------------------------------------------------------------

#[test]
fn a_commit_is_reported_on_the_feed_as_local() {
    let got: String = eval(
        "local store, feed = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'a'):write('b.txt', 'b')
         store:commit(s)
         local item = feed:next()
         local seen = {}
         for _, change in ipairs(item.changes) do
           seen[#seen + 1] = change.path .. ':' .. change.kind .. ':' .. change.origin
         end
         return table.concat(seen, ',')",
    );
    assert_eq!(got, "a.txt:changed:local,b.txt:changed:local");
}

#[test]
fn a_closed_feed_gives_nil_and_the_store_carries_on() {
    let got: (bool, bool, String) = eval(
        "local store, feed = tidings.open_memory()
         feed:close()
         feed:close()
         local s = tidings.staging()
         s:write('a.txt', 'a')
         local committed = store:commit(s)
         return feed:next() == nil, committed ~= nil, store:read('a.txt'):text()",
    );
    assert_eq!(got, (true, true, "a".into()));
}

#[test]
fn closing_the_store_ends_the_feed_after_what_was_recorded() {
    let got: (bool, bool) = eval(
        "local store, feed = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'a')
         store:commit(s)
         store:close()
         return feed:next() ~= nil, feed:next() == nil",
    );
    assert_eq!(got, (true, true));
}

#[cfg(all(feature = "stdlib-utils", feature = "stdlib-datetime"))]
#[test]
fn closing_a_feed_ends_a_wait_on_it_with_nil() {
    let got: bool = eval(
        "local utils = require('utils')
         local store, feed = tidings.open_memory()
         local got = 'unset'
         local task = utils.spawn_task(function() got = feed:next() end)
         require('datetime').sleep(10)
         feed:close()
         task:await()
         return got == nil",
    );
    assert!(got);
}

#[cfg(all(feature = "stdlib-utils", feature = "stdlib-datetime"))]
#[test]
fn a_second_reader_of_a_feed_raises() {
    let message = raises(
        "local utils = require('utils')
         local store, feed = tidings.open_memory()
         local task = utils.spawn_task(function() feed:next() end)
         require('datetime').sleep(10)
         local ok, err = pcall(feed.next, feed)
         feed:close()
         task:await()
         error(err)",
    );
    assert!(message.contains("already being read"), "{message}");
}

// -- Closing -----------------------------------------------------------------------------------

#[test]
fn a_closed_store_raises_and_closing_twice_is_fine() {
    let message = raises(
        "local store = tidings.open_memory()
         store:close()
         store:close()
         store:read('a.txt')",
    );
    assert!(message.contains("closed"), "{message}");
}

#[test]
fn a_to_be_closed_store_is_closed_at_the_end_of_its_scope() {
    let message = raises(
        "local outer
         do
           local store <close> = tidings.open_memory()
           outer = store
         end
         outer:list()",
    );
    assert!(message.contains("closed"), "{message}");
}

// -- Snapshots ---------------------------------------------------------------------------------

#[test]
fn a_snapshot_keeps_reading_the_state_it_was_taken_at() {
    let got: (bool, String, String, bool) = eval(
        "local store = tidings.open_memory()
         local s = tidings.staging()
         s:write('a.txt', 'old')
         store:commit(s)
         local snap = store:snapshot()
         s = tidings.staging()
         s:write('a.txt', 'new'):write('b.txt', 'b')
         store:commit(s)
         local old = snap:read('a.txt'):text()
         local listed = table.concat(snap:list(), ',')
         snap:close()
         return store:supports_snapshots(), old, listed, pcall(snap.list, snap)",
    );
    assert_eq!(got, (true, "old".into(), "a.txt".into(), false));
}

// -- The filesystem and SQLite -----------------------------------------------------------------

#[test]
fn a_filesystem_store_writes_ordinary_files_and_has_no_snapshots() {
    let dir = TempDir::new();
    let location = dir.path().join("store");
    let got: (String, bool, String) = eval(&format!(
        "local store <close> = tidings.open_fs({})
         local s = tidings.staging()
         s:write('notes/a.md', 'hello')
         store:commit(s)
         return tidings.detect({}), store:supports_snapshots(), store:read('notes/a.md'):text()",
        lua_path(&location),
        lua_path(&location),
    ));
    assert_eq!(got, ("fs".into(), false, "hello".into()));
    assert_eq!(
        std::fs::read_to_string(location.join("notes/a.md")).unwrap(),
        "hello"
    );
    let message = raises(&format!(
        "local store <close> = tidings.open_fs({}) store:snapshot()",
        lua_path(&location)
    ));
    assert!(message.contains("not supported"), "{message}");
}

#[test]
fn a_sqlite_store_commits_and_refuses_to_be_opened_as_the_filesystem() {
    let dir = TempDir::new();
    let location = dir.path().join("db");
    let got: (String, String) = eval(&format!(
        "local store <close> = tidings.open_sqlite({})
         local s = tidings.staging()
         s:write('a.txt', 'a')
         store:commit(s)
         return tidings.detect({}), store:read('a.txt'):text()",
        lua_path(&location),
        lua_path(&location),
    ));
    assert_eq!(got, ("sqlite".into(), "a".into()));
    let message = raises(&format!("tidings.open_fs({})", lua_path(&location)));
    assert!(message.contains("sqlite"), "{message}");
}

#[test]
fn detect_gives_nil_for_a_directory_with_no_store() {
    let dir = TempDir::new();
    let got: bool = eval(&format!(
        "return tidings.detect({}) == nil",
        lua_path(dir.path())
    ));
    assert!(got);
}

// -- Tasks -------------------------------------------------------------------------------------

#[test]
fn an_open_filesystem_store_counts_as_a_task_until_it_is_closed() {
    let dir = TempDir::new();
    let rt = Runtime::new(Profile::Trusted).unwrap();
    eval_in::<()>(
        &rt,
        &format!(
            "store = tidings.open_fs({})",
            lua_path(&dir.path().join("s"))
        ),
    );
    assert!(rt.outstanding_tasks() > 0);
    eval_in::<()>(&rt, "store:close()");
    rt.block_on(rt.wait_for_tasks()).unwrap();
    assert_eq!(rt.outstanding_tasks(), 0);
}

#[test]
fn a_memory_store_is_not_a_task() {
    let rt = Runtime::new(Profile::Trusted).unwrap();
    eval_in::<()>(&rt, "store = tidings.open_memory()");
    assert_eq!(rt.outstanding_tasks(), 0);
}

#[test]
fn a_store_whose_tasks_were_aborted_raises_and_its_feed_ends_with_a_resync() {
    let dir = TempDir::new();
    let rt = Runtime::new(Profile::Trusted).unwrap();
    eval_in::<()>(
        &rt,
        &format!(
            "store, feed = tidings.open_sqlite({})",
            lua_path(&dir.path().join("s"))
        ),
    );
    assert!(rt.abort_tasks().unwrap() > 0);
    let message = raises_in(&rt, "store:read('a.txt')");
    assert!(message.contains("abort_tasks"), "{message}");
    let got: (bool, bool) = eval_in(&rt, "return feed:next().resync, feed:next() == nil");
    assert_eq!(got, (true, true));
    // Closing it is still allowed, and is what to do with it.
    eval_in::<()>(&rt, "store:close()");
}
