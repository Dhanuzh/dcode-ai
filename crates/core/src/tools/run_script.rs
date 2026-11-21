//! Code Mode: `run_script` lets the model chain several tool calls in one
//! step with a short JavaScript script, run by Boa (a JavaScript engine in
//! pure Rust) on its own thread. Only what the script prints or returns comes
//! back — intermediate results never enter the conversation.
//!
//! Inside: `tools.<name>(args)` calls a tool and returns its text;
//! `mcp.<server>.<tool>(args)` the same for MCP tools; `parallel([...])` runs
//! several at once; `print`/`log`/`preview` add to the result; `sleep(ms)`
//! waits. The agent runs each call through the normal path — permission
//! checks (asking the user when needed), hooks, undo checkpoints, redaction,
//! events — and hands the result back.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use boa_engine::{
    Context, JsArgs, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// Most tool calls one script may make.
pub const MAX_CALLS: u32 = 200;
/// How long a script may run (time spent waiting for your approval doesn't
/// count).
pub const MAX_TIME: Duration = Duration::from_secs(600);
/// Most characters returned.
pub const MAX_OUTPUT: usize = 20_000;

/// The agent's answer to [`ScriptMsg::Calls`]: one result per call, in
/// order, and how long it waited for the user (added back to the deadline).
pub type ScriptReply = (Vec<Result<String, String>>, Duration);

/// From the script thread to the agent.
pub enum ScriptMsg {
    /// Run these tools (together when they are all read-only); answer on
    /// `reply` with each one's text, or why it failed.
    Calls {
        calls: Vec<(String, serde_json::Value)>,
        reply: std::sync::mpsc::Sender<ScriptReply>,
    },
    /// The script ended: what it printed and returned, or its error.
    Done(Result<String, String>),
}

struct Bridge {
    tx: tokio::sync::mpsc::UnboundedSender<ScriptMsg>,
    out: String,
    calls: u32,
    deadline: Instant,
}

thread_local! {
    static BRIDGE: RefCell<Option<Bridge>> = const { RefCell::new(None) };
}

fn js_err(msg: impl Into<String>) -> boa_engine::JsError {
    JsNativeError::error().with_message(msg.into()).into()
}

fn past_deadline() -> String {
    format!("the script ran past {}s", MAX_TIME.as_secs())
}

/// Sends `calls` to the agent and waits for the answers.
fn dispatch(
    calls: Vec<(String, serde_json::Value)>,
) -> Result<Vec<Result<String, String>>, String> {
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    BRIDGE.with(|b| -> Result<(), String> {
        let mut b = b.borrow_mut();
        let b = b.as_mut().ok_or("no bridge")?;
        b.calls += calls.len() as u32;
        if b.calls > MAX_CALLS {
            return Err(format!("more than {MAX_CALLS} tool calls in one script"));
        }
        if b.deadline <= Instant::now() {
            return Err(past_deadline());
        }
        b.tx.send(ScriptMsg::Calls {
            calls,
            reply: reply_tx,
        })
        .map_err(|_| "the agent stopped".to_string())
    })?;
    // The agent bounds each call by the deadline and answers (or drops the
    // sender when the run is stopped).
    let (results, waited) = reply_rx
        .recv()
        .map_err(|_| "the agent stopped".to_string())?;
    BRIDGE.with(|b| {
        if let Some(b) = b.borrow_mut().as_mut() {
            b.deadline += waited;
        }
    });
    Ok(results)
}

fn str_arg(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    Ok(args
        .get_or_undefined(i)
        .to_string(ctx)?
        .to_std_string_escaped())
}

/// `__call(name, argsJson)` → the tool's text.
fn host_call(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = str_arg(args, 0, ctx)?;
    let json = str_arg(args, 1, ctx)?;
    let args: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::json!({}));
    let mut results = dispatch(vec![(name, args)]).map_err(js_err)?;
    match results.pop() {
        Some(Ok(text)) => Ok(JsValue::from(JsString::from(text.as_str()))),
        Some(Err(e)) => Err(js_err(e)),
        None => Err(js_err("no answer from the agent")),
    }
}

/// `__parallel(callsJson)` → JSON `[{ok, text} | {ok: false, error}]`.
fn host_parallel(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let json = str_arg(args, 0, ctx)?;
    let list: Vec<serde_json::Value> =
        serde_json::from_str(&json).map_err(|e| js_err(format!("parallel: {e}")))?;
    let calls = list
        .into_iter()
        .map(|c| {
            let tool = c["tool"].as_str().unwrap_or_default().to_string();
            let args = match c.get("args") {
                Some(a) if !a.is_null() => a.clone(),
                _ => serde_json::json!({}),
            };
            (tool, args)
        })
        .collect::<Vec<_>>();
    if calls.is_empty() {
        return Ok(JsValue::from(js_string!("[]")));
    }
    let results = dispatch(calls).map_err(js_err)?;
    let out: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| match r {
            Ok(text) => serde_json::json!({"ok": true, "text": text}),
            Err(error) => serde_json::json!({"ok": false, "error": error}),
        })
        .collect();
    let s = serde_json::Value::Array(out).to_string();
    Ok(JsValue::from(JsString::from(s.as_str())))
}

/// `sleep(ms)` — waits, at most until the deadline.
fn host_sleep(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let ms = args.get_or_undefined(0).to_number(ctx)?;
    let want = Duration::from_millis(if ms.is_finite() && ms > 0.0 {
        ms as u64
    } else {
        0
    });
    let left = BRIDGE.with(|b| {
        b.borrow()
            .as_ref()
            .map(|b| b.deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    });
    std::thread::sleep(want.min(left));
    if want > left {
        return Err(js_err(past_deadline()));
    }
    Ok(JsValue::undefined())
}

/// `print(...)` → a line of the result.
fn host_print(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let mut parts = Vec::new();
    for a in args {
        parts.push(a.to_string(ctx)?.to_std_string_escaped());
    }
    BRIDGE.with(|b| {
        if let Some(b) = b.borrow_mut().as_mut()
            && b.out.len() < MAX_OUTPUT * 2
        {
            b.out.push_str(&parts.join(" "));
            b.out.push('\n');
        }
    });
    Ok(JsValue::undefined())
}

/// `tools.x(args)` and `mcp.server.tool(args)` over `__call`; the script's
/// body runs as a function, so `return` works.
const PRELUDE: &str = r#"
const tools = new Proxy({}, { get: (_, name) => (args) => __call(String(name), JSON.stringify(args ?? {})) });
const mcp = new Proxy({}, { get: (_, server) => new Proxy({}, {
  get: (_, tool) => (args) => __call("mcp__" + String(server) + "__" + String(tool), JSON.stringify(args ?? {})) }) });
const console = { log: (...a) => print(...a), error: (...a) => print(...a) };
const log = (...a) => print(...a);
const preview = (v, n = 2000) => {
  let s = typeof v === 'string' ? v : JSON.stringify(v, null, 2);
  if (s === undefined) s = String(v);
  print(s.length > n ? s.slice(0, n) + '… (+' + (s.length - n) + ' characters)' : s);
};
const parallel = (calls) => JSON.parse(__parallel(JSON.stringify((calls ?? []).map(c => ({ tool: String(c.tool), args: c.args ?? {} })))));
"#;

/// Runs `code` on the current thread, sending tool calls over `tx`; ends
/// with [`ScriptMsg::Done`]. Call it on a thread of its own.
pub fn run(code: String, tx: tokio::sync::mpsc::UnboundedSender<ScriptMsg>) {
    BRIDGE.with(|b| {
        *b.borrow_mut() = Some(Bridge {
            tx: tx.clone(),
            out: String::new(),
            calls: 0,
            deadline: Instant::now() + MAX_TIME,
        })
    });
    let result = (|| -> Result<String, String> {
        let mut ctx = Context::default();
        // A runaway loop stops instead of spinning forever.
        ctx.runtime_limits_mut().set_loop_iteration_limit(5_000_000);
        ctx.runtime_limits_mut().set_recursion_limit(2_000);
        ctx.register_global_callable(
            js_string!("__call"),
            2,
            NativeFunction::from_fn_ptr(host_call),
        )
        .map_err(|e| e.to_string())?;
        ctx.register_global_callable(
            js_string!("__parallel"),
            1,
            NativeFunction::from_fn_ptr(host_parallel),
        )
        .map_err(|e| e.to_string())?;
        ctx.register_global_callable(
            js_string!("sleep"),
            1,
            NativeFunction::from_fn_ptr(host_sleep),
        )
        .map_err(|e| e.to_string())?;
        ctx.register_global_callable(
            js_string!("print"),
            1,
            NativeFunction::from_fn_ptr(host_print),
        )
        .map_err(|e| e.to_string())?;
        ctx.eval(Source::from_bytes(PRELUDE))
            .map_err(|e| e.to_string())?;
        let wrapped = format!(
            "(() => {{ const __r = (() => {{\n{code}\n}})(); return __r === undefined ? undefined : (typeof __r === 'string' ? __r : JSON.stringify(__r, null, 2)); }})()"
        );
        let v = ctx
            .eval(Source::from_bytes(wrapped.as_bytes()))
            .map_err(|e| format!("script error: {e}"))?;
        let mut out = BRIDGE.with(|b| {
            b.borrow()
                .as_ref()
                .map(|b| b.out.clone())
                .unwrap_or_default()
        });
        if !v.is_undefined() && !v.is_null() {
            let r = v
                .to_string(&mut ctx)
                .map_err(|e| e.to_string())?
                .to_std_string_escaped();
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&r);
        }
        Ok(out)
    })();
    let result = result.map(|mut s| {
        if s.chars().count() > MAX_OUTPUT {
            s = s.chars().take(MAX_OUTPUT).collect::<String>() + "\n… (cut at 20k characters)";
        }
        s
    });
    let _ = tx.send(ScriptMsg::Done(result));
    BRIDGE.with(|b| *b.borrow_mut() = None);
}

// ── Running a script in its own process ─────────────────────────────────
//
// Boa has no heap limit and no way to stop a running script, so a script
// that builds a huge array could fill the machine's memory, and one stopped
// with Esc kept computing on its thread. The `dcode-ai` binary runs each
// script in a child copy of itself (memory-capped on Unix, killed when the
// run is stopped), speaking JSON lines over stdin/stdout:
//   parent → child  {"code": "..."}
//   child  → parent {"calls": [["read_file", {...}], ...]}
//   parent → child  {"results": [{"ok": "..."} | {"err": "..."}], "waited_ms": 0}
//   child  → parent {"done": {"ok": "..."} | {"err": "..."}}

/// The hidden argument that makes `dcode-ai` a script child.
pub const CHILD_ARG: &str = "__dcode-run-script";
/// Address-space cap for a script child (Unix).
pub const CHILD_MEMORY_LIMIT: u64 = 4 << 30;

static SUBPROCESS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called by the `dcode-ai` binary: scripts run in child processes. (Tests
/// and other embedders run them on a thread.)
pub fn enable_subprocess() {
    SUBPROCESS.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Starts `code`; its tool calls and end arrive on `tx` as [`ScriptMsg`].
pub fn spawn(
    code: String,
    tx: tokio::sync::mpsc::UnboundedSender<ScriptMsg>,
) -> Result<(), String> {
    if SUBPROCESS.load(std::sync::atomic::Ordering::SeqCst)
        && let Ok(exe) = std::env::current_exe()
        && spawn_child(&exe, &code, tx.clone()).is_ok()
    {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("run_script".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run(code, tx))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn result_json(r: &Result<String, String>) -> serde_json::Value {
    match r {
        Ok(s) => serde_json::json!({ "ok": s }),
        Err(e) => serde_json::json!({ "err": e }),
    }
}

fn result_from_json(v: &serde_json::Value) -> Result<String, String> {
    if let Some(s) = v.get("ok").and_then(|s| s.as_str()) {
        Ok(s.to_string())
    } else {
        Err(v
            .get("err")
            .and_then(|s| s.as_str())
            .unwrap_or("bad answer")
            .to_string())
    }
}

/// Runs `code` in a child `exe` (see [`CHILD_ARG`]).
pub fn spawn_child(
    exe: &std::path::Path,
    code: &str,
    tx: tokio::sync::mpsc::UnboundedSender<ScriptMsg>,
) -> Result<(), String> {
    use std::io::{BufRead, Write};
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe)
        .arg(CHILD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    writeln!(stdin, "{}", serde_json::json!({ "code": code })).map_err(|e| e.to_string())?;
    let child = std::sync::Arc::new(std::sync::Mutex::new(child));
    // Stopped run (the agent dropped its end): kill the child.
    {
        let (child, tx) = (child.clone(), tx.clone());
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
                if matches!(c.try_wait(), Ok(Some(_)) | Err(_)) {
                    return;
                }
                if tx.is_closed() {
                    let _ = c.kill();
                    let _ = c.wait();
                    return;
                }
            }
        });
    }
    std::thread::spawn(move || {
        let kill = |child: &std::sync::Arc<std::sync::Mutex<std::process::Child>>| {
            let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
            let _ = c.kill();
            let _ = c.wait();
        };
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(done) = msg.get("done") {
                let _ = tx.send(ScriptMsg::Done(result_from_json(done)));
                let _ = child.lock().map(|mut c| c.wait());
                return;
            }
            let Some(list) = msg.get("calls").and_then(|c| c.as_array()) else {
                continue;
            };
            let calls = list
                .iter()
                .map(|c| {
                    (
                        c[0].as_str().unwrap_or_default().to_string(),
                        c.get(1).cloned().unwrap_or(serde_json::json!({})),
                    )
                })
                .collect();
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            if tx
                .send(ScriptMsg::Calls {
                    calls,
                    reply: reply_tx,
                })
                .is_err()
            {
                return kill(&child);
            }
            let Ok((results, waited)) = reply_rx.recv() else {
                return kill(&child);
            };
            let answer = serde_json::json!({
                "results": results.iter().map(result_json).collect::<Vec<_>>(),
                "waited_ms": waited.as_millis() as u64,
            });
            if writeln!(stdin, "{answer}").is_err() {
                break;
            }
        }
        // Ended without a result: killed, crashed or out of memory.
        let status = child.lock().ok().and_then(|mut c| c.wait().ok());
        let why = match status {
            Some(s) if s.success() => "the script process ended without a result".to_string(),
            _ => format!(
                "the script stopped: it ran out of memory (limit {} GiB) or crashed",
                CHILD_MEMORY_LIMIT >> 30
            ),
        };
        let _ = tx.send(ScriptMsg::Done(Err(why)));
    });
    Ok(())
}

/// The script child's `main`: reads the script, runs it, relays its tool
/// calls to the parent and exits with its result. Never returns.
pub fn child_main() -> ! {
    use std::io::{BufRead, Write};
    #[cfg(unix)]
    unsafe {
        // SAFETY: plain libc call on a struct we own.
        let lim = libc::rlimit {
            rlim_cur: CHILD_MEMORY_LIMIT as libc::rlim_t,
            rlim_max: CHILD_MEMORY_LIMIT as libc::rlim_t,
        };
        libc::setrlimit(libc::RLIMIT_AS, &lim);
        // Die with the parent: a crashed or killed dcode-ai mustn't leave a
        // script computing on its own.
        #[cfg(target_os = "linux")]
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // No parent-death signal here: notice the parent is gone (we get
        // re-parented) and stop.
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() };
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if unsafe { libc::getppid() } != parent {
                    std::process::exit(1);
                }
            }
        });
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut first = String::new();
    let _ = input.read_line(&mut first);
    let code = serde_json::from_str::<serde_json::Value>(&first)
        .ok()
        .and_then(|v| v["code"].as_str().map(String::from))
        .unwrap_or_default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || run(code, tx));
    let mut out = std::io::stdout();
    if started.is_err() {
        let _ = writeln!(
            out,
            "{}",
            serde_json::json!({"done": {"err": "couldn't start the script"}})
        );
        std::process::exit(1);
    }
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            ScriptMsg::Calls { calls, reply } => {
                let line = serde_json::json!({
                    "calls": calls.iter().map(|(n, a)| serde_json::json!([n, a])).collect::<Vec<_>>()
                });
                if writeln!(out, "{line}").and_then(|_| out.flush()).is_err() {
                    std::process::exit(1);
                }
                let mut answer = String::new();
                if input.read_line(&mut answer).unwrap_or(0) == 0 {
                    std::process::exit(1);
                }
                let v: serde_json::Value = serde_json::from_str(&answer).unwrap_or_default();
                let results = v["results"]
                    .as_array()
                    .map(|a| a.iter().map(result_from_json).collect())
                    .unwrap_or_default();
                let waited = Duration::from_millis(v["waited_ms"].as_u64().unwrap_or(0));
                let _ = reply.send((results, waited));
            }
            ScriptMsg::Done(r) => {
                // Boa reports a refused allocation as a RangeError: say why.
                let r = r.map_err(|e| {
                    if e.contains("while allocating") || e.contains("memory allocation") {
                        format!(
                            "the script ran out of memory (limit {} GiB): {e}",
                            CHILD_MEMORY_LIMIT >> 30
                        )
                    } else {
                        e
                    }
                });
                let _ = writeln!(out, "{}", serde_json::json!({ "done": result_json(&r) }));
                let _ = out.flush();
                std::process::exit(0);
            }
        }
    }
    std::process::exit(1);
}

/// The `run_script` definition. The agent runs it itself (it needs the
/// session's other tools); `execute` here only answers if called directly.
pub struct RunScriptTool;

#[async_trait::async_trait]
impl ToolExecutor for RunScriptTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_script".into(),
            description: "Chain several tool calls in one step with a short JavaScript script, \
when you'd otherwise make 3+ calls in a row or need to filter big results — only what the \
script prints or returns comes back to you, so intermediate data doesn't fill the conversation. \
Inside: `tools.<tool_name>({...args})` calls a tool and returns its text output (JSON.parse it \
when it's JSON); `mcp.<server>.<tool>({...})` calls an MCP tool; \
`parallel([{tool: 'read_file', args: {path: 'a'}}, ...])` runs several calls together (read-only \
ones at once) and returns `[{ok, text} | {ok: false, error}]` in order; `print(...)` / `log(...)` \
add a line; `preview(value, n)` prints a value cut at n characters; `sleep(ms)` waits; the \
script's return value (objects become JSON) ends the result. Plain synchronous JavaScript — no \
await, no require, no files or network except through tools. Calls go through the normal \
permission checks: one that needs approval asks the user (a denial throws). \
Limits: 200 tool calls, 10 minutes, 20k characters. A failing call throws — catch it to carry on. \
Example: `const hits = tools.search_code({pattern: 'TODO'}); return hits.split('\\n').filter(l => l.includes('auth')).slice(0, 20);`"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "The script body (JavaScript; `return` a value to end)"}
                },
                "required": ["code"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some("run_script is run by the agent loop".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a script with a fake agent answering tool calls.
    async fn script(
        code: &str,
        answer: impl Fn(&str, &serde_json::Value) -> Result<String, String>,
    ) -> (Result<String, String>, Vec<String>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let code = code.to_string();
        std::thread::spawn(move || run(code, tx));
        let mut called = Vec::new();
        loop {
            match rx.recv().await {
                Some(ScriptMsg::Calls { calls, reply }) => {
                    let results = calls
                        .iter()
                        .map(|(name, args)| {
                            called.push(name.clone());
                            answer(name, args)
                        })
                        .collect();
                    let _ = reply.send((results, Duration::ZERO));
                }
                Some(ScriptMsg::Done(r)) => return (r, called),
                None => return (Err("gone".into()), called),
            }
        }
    }

    #[tokio::test]
    async fn chains_calls_and_returns_only_the_result() {
        let (r, called) = script(
            "const a = tools.read_file({path: 'a.txt'});\nconst b = JSON.parse(mcp.jira.search({jql: 'x'}));\nprint('read', a.length);\nreturn { keys: b.issues.map(i => i.key) };",
            |name, args| match name {
                "read_file" => {
                    assert_eq!(args["path"], "a.txt");
                    Ok("hello".into())
                }
                "mcp__jira__search" => Ok(r#"{"issues":[{"key":"BSI-1"},{"key":"BSI-2"}]}"#.into()),
                _ => Err("unknown".into()),
            },
        )
        .await;
        let out = r.unwrap();
        assert_eq!(called, ["read_file", "mcp__jira__search"]);
        assert!(out.starts_with("read 5\n"), "{out}");
        assert!(out.contains("\"BSI-2\""), "{out}");
    }

    #[tokio::test]
    async fn a_failing_call_throws_and_can_be_caught() {
        let (r, _) = script(
            "try { tools.write_file({path: 'x'}); } catch (e) { return 'caught: ' + e.message; }",
            |_, _| Err("`write_file` needs your approval — call it directly".into()),
        )
        .await;
        assert_eq!(
            r.unwrap(),
            "caught: `write_file` needs your approval — call it directly"
        );
        let (r, _) = script("tools.nope({})", |_, _| Err("no tool `nope`".into())).await;
        assert!(r.unwrap_err().contains("no tool `nope`"));
    }

    #[tokio::test]
    async fn limits_hold() {
        let (r, called) = script(
            "for (let i = 0; i < 300; i++) tools.read_file({});",
            |_, _| Ok(String::new()),
        )
        .await;
        assert!(r.unwrap_err().contains("more than 200 tool calls"));
        assert_eq!(called.len(), 200);
        let (r, _) = script("while (true) {}", |_, _| Ok(String::new())).await;
        assert!(r.is_err(), "a runaway loop stops");
        let (r, _) = script("this is not javascript", |_, _| Ok(String::new())).await;
        assert!(r.unwrap_err().starts_with("script error"));
    }

    #[tokio::test]
    async fn parallel_returns_each_result_in_order() {
        let (r, called) = script(
            "const r = parallel([{tool: 'read_file', args: {path: 'a'}}, {tool: 'nope'}, {tool: 'read_file', args: {path: 'b'}}]);\n\
             return r.map(x => x.ok ? x.text : 'ERR ' + x.error).join('|');",
            |name, args| match name {
                "read_file" => Ok(format!("<{}>", args["path"].as_str().unwrap_or(""))),
                _ => Err("no tool".into()),
            },
        )
        .await;
        assert_eq!(r.unwrap(), "<a>|ERR no tool|<b>");
        assert_eq!(called, ["read_file", "nope", "read_file"]);
        let (r, called) = script("return parallel([]).length", |_, _| Ok(String::new())).await;
        assert_eq!(r.unwrap(), "0");
        assert!(called.is_empty());
    }

    #[tokio::test]
    async fn built_ins_log_preview_sleep() {
        let (r, _) = script(
            "log('a', 1); preview({k: 'x'.repeat(50)}, 10); preview('short'); sleep(5); console.error('e');",
            |_, _| Ok(String::new()),
        )
        .await;
        let out = r.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "a 1");
        assert!(
            lines[1].starts_with("{\n") || lines[1].starts_with('{'),
            "{out}"
        );
        assert!(out.contains("characters)"), "{out}");
        assert!(out.contains("short\ne"), "{out}");
        // A script may declare its own `log`.
        let (r, _) = script("const log = (x) => 'mine ' + x; return log(1);", |_, _| {
            Ok(String::new())
        })
        .await;
        assert_eq!(r.unwrap(), "mine 1");
    }
}
