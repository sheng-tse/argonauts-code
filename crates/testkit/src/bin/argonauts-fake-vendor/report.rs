//! The fake's MCP client: starts a stdio server the session was given, as an agent would, and
//! calls one of its tools once. The `report` step calls `argonauts_report` on the server an ACP
//! client named, and `call_tool` any tool of the server in Claude Code's `--mcp-config`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const REPORT_TOOL: &str = "argonauts_report";
const EXIT_GRACE: Duration = Duration::from_secs(2);

/// `server` is an ACP stdio MCP server: `name`, `command`, `args`, and `env` as name and value
/// pairs. Returns the tool's result, or `{"error": ...}` when the call gets a JSON-RPC error.
pub fn call(server: &Value, report: &Value) -> anyhow::Result<Value> {
    call_tool(server, REPORT_TOOL, report)
}

/// The first server of Claude Code's `--mcp-config` file (`{"mcpServers": {name: {command,
/// args, env}}}`), in the shape [`call_tool`] takes, with its env as name and value pairs.
pub fn config_server(config: &Value) -> Option<Value> {
    let (name, server) = config.get("mcpServers")?.as_object()?.iter().next()?;
    let env: Vec<Value> = server
        .get("env")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(name, value)| json!({ "name": name, "value": value }))
        .collect();
    Some(json!({
        "name": name,
        "command": server.get("command")?,
        "args": server.get("args").cloned().unwrap_or_else(|| json!([])),
        "env": env,
    }))
}

/// Calls `tool` with `arguments` on `server`, in the shape [`call`] takes.
pub fn call_tool(server: &Value, tool: &str, arguments: &Value) -> anyhow::Result<Value> {
    let command = server["command"]
        .as_str()
        .context("the MCP server has no command")?;
    let args = server["args"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let env = server["env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|var| Some((var["name"].as_str()?, var["value"].as_str()?)));
    let mut child = Command::new(command)
        .args(args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("starting the MCP server {command}"))?;
    let reply = call_one(&mut child, tool, arguments);
    finish(child);
    reply
}

/// Closes the server's stdin on return, which tells it to exit.
fn call_one(child: &mut Child, tool: &str, arguments: &Value) -> anyhow::Result<Value> {
    let mut stdin = child.stdin.take().context("no stdin for the MCP server")?;
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .context("no stdout from the MCP server")?,
    );
    let init = request(
        &mut stdin,
        &mut stdout,
        0,
        "initialize",
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "argonauts-fake-vendor", "version": env!("CARGO_PKG_VERSION") },
        }),
    )?;
    if let Some(error) = init.get("error") {
        anyhow::bail!("the MCP server refused initialize: {error}");
    }
    send(
        &mut stdin,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )?;
    let reply = request(
        &mut stdin,
        &mut stdout,
        1,
        "tools/call",
        json!({ "name": tool, "arguments": arguments }),
    )?;
    Ok(match reply.get("error") {
        Some(error) => json!({ "error": error }),
        None => reply.get("result").cloned().unwrap_or(Value::Null),
    })
}

fn request(
    stdin: &mut ChildStdin,
    stdout: &mut BufReader<ChildStdout>,
    id: u64,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    send(
        stdin,
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )?;
    let mut line = String::new();
    loop {
        line.clear();
        if stdout.read_line(&mut line)? == 0 {
            anyhow::bail!("the MCP server closed before answering {method}");
        }
        // Logs and the server's own requests are skipped; only our answer matters.
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if frame["id"] == id && frame.get("method").is_none() {
            return Ok(frame);
        }
    }
}

fn send(stdin: &mut ChildStdin, frame: &Value) -> anyhow::Result<()> {
    writeln!(stdin, "{frame}").context("writing to the MCP server")?;
    stdin.flush().context("writing to the MCP server")
}

/// Gives the server a moment to exit after its stdin closes, then kills it.
fn finish(mut child: Child) {
    let deadline = Instant::now() + EXIT_GRACE;
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // It may have exited since, and the fake has nothing to report either way.
    let _ = child.kill();
    let _ = child.wait();
}
