//! The interactive prompt.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use avarice::mlua::{self, Function, MultiValue};
use avarice::{Error, Runtime};
use reedline::{
    FileBackedHistory, Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus,
    Reedline, Signal,
};

use super::escape::takes_colour;
use super::highlight::LuaHighlighter;
use super::output::{eprintln, println};
use super::{CliError, run_and_settle_tasks};

/// How many entries the history file keeps.
const HISTORY_CAPACITY: usize = 2_000;

/// What the prompt shows on a fresh entry, and on the continuation of an unfinished one.
const FRESH_INDICATOR: &str = "> ";
const CONTINUATION_INDICATOR: &str = ">> ";

struct LuaPrompt {
    indicator: &'static str,
}

const FRESH: LuaPrompt = LuaPrompt {
    indicator: FRESH_INDICATOR,
};
const CONTINUED: LuaPrompt = LuaPrompt {
    indicator: CONTINUATION_INDICATOR,
};

impl Prompt for LuaPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed(self.indicator)
    }

    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed(CONTINUATION_INDICATOR)
    }

    fn render_prompt_history_search_indicator(&self, search: PromptHistorySearch) -> Cow<'_, str> {
        let prefix = match search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({prefix}reverse-search: {}) ", search.term))
    }
}

/// Runs the prompt until Ctrl-D or end of input.
pub fn run(rt: &Runtime) -> Result<(), Error> {
    // Reedline paints to standard error, so that is the stream whose colour it follows. It reads
    // no environment variable itself, and its default highlighter colours typed text, so both are
    // set explicitly (ADR 0009). `buffer` holds the lines of an unfinished multi-line entry, and
    // is shared with the highlighter (ADR 0013) so a `--[[` or `[[` opened on an earlier line is
    // still recognised on the line being typed now.
    let colour = takes_colour(&std::io::stderr());
    let buffer = Arc::new(Mutex::new(String::new()));
    let mut editor = Reedline::create()
        .with_ansi_colors(colour)
        .with_highlighter(Box::new(LuaHighlighter::new(Arc::clone(&buffer))));
    if let Some(path) = history_path() {
        match FileBackedHistory::with_file(HISTORY_CAPACITY, path) {
            Ok(history) => editor = editor.with_history(Box::new(history)),
            Err(e) => eprintln!("avarice: history unavailable: {e}"),
        }
    }

    println!(
        "avarice {} — Lua 5.4 ({} profile). Ctrl-D to exit.",
        env!("CARGO_PKG_VERSION"),
        rt.profile()
    );

    loop {
        let prompt: &dyn Prompt = if buffer.lock().unwrap().is_empty() {
            &FRESH
        } else {
            &CONTINUED
        };
        match editor.read_line(prompt) {
            Ok(Signal::Success(line)) => {
                buffer.lock().unwrap().push_str(&line);
                // Cloned out rather than matched on directly: a guard borrowed in a match's
                // scrutinee stays alive for the whole match, and an arm below locks it again.
                let entry = buffer.lock().unwrap().clone();
                match compile(rt, &entry) {
                    Ok(chunk) => {
                        buffer.lock().unwrap().clear();
                        // The tasks an entry leaves are finished, or given up, before the next
                        // prompt, so the line is read with nothing else running.
                        if let Err(e) = run_and_settle_tasks(rt, evaluate(rt, chunk)) {
                            eprintln!("avarice: {e}");
                        }
                    }
                    Err(Incomplete) => buffer.lock().unwrap().push('\n'),
                    Err(Failed(message)) => {
                        buffer.lock().unwrap().clear();
                        eprintln!("avarice: {message}");
                    }
                }
            }
            // Ctrl-C at the prompt abandons whatever has been typed so far, including a
            // half-finished multi-line entry.
            Ok(Signal::CtrlC) => buffer.lock().unwrap().clear(),
            Ok(Signal::CtrlD) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("avarice: {e}");
                break;
            }
        }
    }
    Ok(())
}

/// Why a line could not be turned into a chunk.
#[derive(Debug)]
enum CompileError {
    /// Lua wants more input: the entry continues on the next line.
    Incomplete,
    /// A real syntax error.
    Failed(String),
}

use CompileError::{Failed, Incomplete};

/// Compiles an entry, trying it as an expression first.
///
/// `1 + 1` is not a statement, so a bare expression would be a syntax error; stock `lua` tries
/// `return <entry>` first for exactly this reason, and prints whatever it evaluates to.
fn compile(rt: &Runtime, entry: &str) -> Result<Function, CompileError> {
    if !entry.contains('\n')
        && let Ok(chunk) = rt
            .load(format!("return {entry};"), "=stdin")
            .into_function()
    {
        return Ok(chunk);
    }
    match rt.load(entry.to_owned(), "=stdin").into_function() {
        Ok(chunk) => Ok(chunk),
        Err(mlua::Error::SyntaxError {
            incomplete_input: true,
            ..
        }) => Err(Incomplete),
        Err(mlua::Error::SyntaxError { message, .. }) => Err(Failed(message)),
        Err(e) => Err(Failed(e.to_string())),
    }
}

/// Runs an entry and prints whatever it returned, as stock `lua` does.
async fn evaluate(rt: &Runtime, chunk: Function) -> Result<(), CliError> {
    // A previous entry may have been cancelled or timed out; each entry starts fresh.
    if let Some(cancel) = rt.cancel_handle() {
        cancel.reset();
    }
    rt.run(async {
        let values: MultiValue = chunk.call_async(()).await?;
        if !values.is_empty() {
            let print: Function = rt.lua().globals().get("print")?;
            print.call_async::<()>(values).await?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

/// `$XDG_STATE_HOME/avarice/repl-history`, the conventional home for a REPL's history.
fn history_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
        })?;
    let dir = base.join("avarice");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("repl-history"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use avarice::Profile;

    fn runtime() -> Runtime {
        Runtime::new(Profile::Trusted).unwrap()
    }

    fn classify(rt: &Runtime, entry: &str) -> &'static str {
        match compile(rt, entry) {
            Ok(_) => "complete",
            Err(Incomplete) => "incomplete",
            Err(Failed(_)) => "failed",
        }
    }

    #[test]
    fn a_bare_expression_is_compiled_as_one() {
        let rt = runtime();
        let chunk = compile(&rt, "6 * 7").expect("should compile");
        assert_eq!(rt.block_on(chunk.call_async::<i64>(())).unwrap(), 42);
    }

    #[test]
    fn a_statement_still_compiles() {
        let rt = runtime();
        let chunk = compile(&rt, "x = 6 * 7").expect("should compile");
        rt.block_on(chunk.call_async::<()>(())).unwrap();
        assert_eq!(
            rt.block_on(rt.eval::<i64>("return x", "=test")).unwrap(),
            42
        );
    }

    #[test]
    fn an_unfinished_entry_asks_for_more() {
        let rt = runtime();
        for entry in ["function f()", "local t = {", "if x then", "('abc", "(1 +"] {
            assert_eq!(classify(&rt, entry), "incomplete", "{entry:?}");
        }
    }

    #[test]
    fn a_real_syntax_error_is_reported_rather_than_waited_on() {
        // `1 +` is one of these rather than an unfinished entry, because as a *statement* it is
        // already wrong at the `1`, not at the end. Stock `lua` reports it the same way.
        let rt = runtime();
        for entry in [")", "local 1x = 2", "x ==== 3", "1 +"] {
            assert_eq!(classify(&rt, entry), "failed", "{entry:?}");
        }
    }

    #[test]
    fn a_multi_line_entry_compiles_once_it_is_finished() {
        let rt = runtime();
        assert_eq!(classify(&rt, "function f(a)"), "incomplete");
        assert_eq!(classify(&rt, "function f(a)\nreturn a + 1"), "incomplete");
        let chunk = compile(&rt, "function f(a)\nreturn a + 1\nend").expect("should compile");
        rt.block_on(chunk.call_async::<()>(())).unwrap();
        assert_eq!(
            rt.block_on(rt.eval::<i64>("return f(41)", "=test"))
                .unwrap(),
            42
        );
    }

    #[test]
    fn the_expression_attempt_is_only_made_for_a_single_line() {
        // `return` is prepended only on the first line; once an entry spans lines it is a
        // statement, and an expression spanning lines is not something the REPL tries to guess.
        let rt = runtime();
        assert_eq!(classify(&rt, "6 *\n7"), "failed");
    }
}
