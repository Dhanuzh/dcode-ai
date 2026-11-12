//! Code Mode: `run_script` lets the model chain several tool calls in one
//! step with a short JavaScript script, run by Boa (a JavaScript engine in
//! pure Rust) on its own thread. Only what the script prints or returns comes
//! back — intermediate results never enter the conversation.
//!
//! Inside: `tools.<name>(args)` calls any tool the session allows without
//! asking (reads and searches; edits in auto / accept-edits mode) and returns
//! its text; `mcp.<server>.<tool>(args)` the same for MCP tools; `print(…)`
//! adds a line to the result. A call that would need approval fails with a
//! message saying to make it directly. The agent runs each call (hooks, undo
//! checkpoints, redaction) and hands the result back.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use boa_engine::{
    Context, JsArgs, JsNativeError, JsResult, JsString, JsValue, NativeFunction, Source, js_string,
};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// Most tool calls one script may make.
pub const MAX_CALLS: u32 = 50;
/// How long a script may run.
pub const MAX_TIME: Duration = Duration::from_secs(60);
/// Most characters returned.
pub const MAX_OUTPUT: usize = 20_000;

/// From the script thread to the agent.
pub enum ScriptMsg {
    /// Run this tool; answer on `reply` (its text, or why it failed).
    Call {
        name: String,
        args: serde_json::Value,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
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

/// `__call(name, argsJson)` → the tool's text.
fn host_call(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    let json = args
        .get_or_undefined(1)
        .to_string(ctx)?
        .to_std_string_escaped();
    let args: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::json!({}));
    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
    let wait = BRIDGE.with(|b| -> Result<Duration, String> {
        let mut b = b.borrow_mut();
        let b = b.as_mut().ok_or("no bridge")?;
        b.calls += 1;
        if b.calls > MAX_CALLS {
            return Err(format!("more than {MAX_CALLS} tool calls in one script"));
        }
        let left = b.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("the script ran past {}s", MAX_TIME.as_secs()));
        }
        b.tx.send(ScriptMsg::Call {
            name,
            args,
            reply: reply_tx,
        })
        .map_err(|_| "the agent stopped".to_string())?;
        Ok(left)
    });
    let left = wait.map_err(js_err)?;
    match reply_rx.recv_timeout(left) {
        Ok(Ok(text)) => Ok(JsValue::from(JsString::from(text.as_str()))),
        Ok(Err(e)) => Err(js_err(e)),
        Err(_) => Err(js_err(format!(
            "the script ran past {}s",
            MAX_TIME.as_secs()
        ))),
    }
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
const console = { log: (...a) => print(...a) };
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
when it's JSON); `mcp.<server>.<tool>({...})` calls an MCP tool; `print(...)` adds a line; the \
script's return value (objects become JSON) ends the result. Plain synchronous JavaScript — no \
await, no require, no files or network except through tools. Only tools allowed without asking \
in the current mode can be called; one that needs approval fails (call it directly instead). \
Limits: 50 tool calls, 60 seconds, 20k characters. A failing call throws — catch it to carry on. \
Example: `const hits = tools.search({pattern: 'TODO'}); return hits.split('\\n').filter(l => l.includes('auth')).slice(0, 20);`"
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
                Some(ScriptMsg::Call { name, args, reply }) => {
                    called.push(name.clone());
                    let _ = reply.send(answer(&name, &args));
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
            "for (let i = 0; i < 100; i++) tools.read_file({});",
            |_, _| Ok(String::new()),
        )
        .await;
        assert!(r.unwrap_err().contains("more than 50 tool calls"));
        assert_eq!(called.len(), 50);
        let (r, _) = script("while (true) {}", |_, _| Ok(String::new())).await;
        assert!(r.is_err(), "a runaway loop stops");
        let (r, _) = script("this is not javascript", |_, _| Ok(String::new())).await;
        assert!(r.unwrap_err().starts_with("script error"));
    }
}
