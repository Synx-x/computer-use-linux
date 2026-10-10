//! `computer-use-linux desktops mcp`: an MCP server for the agent that hands out work.
//!
//! An orchestrating agent uses it to run tasks on agent desktops: one hidden, sandboxed
//! desktop per task, each with its own Claude agent. It talks to the desktops daemon
//! over its local HTTP API, so the daemon must be running (`desktops install` sets it
//! up as a service).

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock},
    schemars::JsonSchema,
    tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DEFAULT_PORT: u16 = 7420;

#[derive(Clone)]
pub struct DesktopsMcp {
    port: u16,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

/// Serve the MCP over stdio. `--port` picks the daemon, default 7420.
pub async fn serve(args: &[String]) -> Result<()> {
    let mut port = std::env::var("CUL_DESKTOPS_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" => {
                port = iter
                    .next()
                    .ok_or_else(|| anyhow!("--port needs a value"))?
                    .parse()
                    .context("bad --port")?
            }
            other => bail!("unknown desktops mcp option {other}"),
        }
    }
    DesktopsMcp {
        port,
        tool_router: DesktopsMcp::tool_router(),
    }
    .serve(rmcp::transport::stdio())
    .await?
    .waiting()
    .await?;
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StartTaskParams {
    /// Short name for the desktop, letters, digits and dashes, such as "fix-login".
    name: String,
    /// The full task for the agent, written as you would brief a colleague.
    prompt: String,
    /// Screen size, such as "1280x800" (default) or "1920x1080".
    #[serde(default)]
    resolution: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateParams {
    /// Short name for the desktop.
    name: String,
    /// Screen size, such as "1280x800" (default).
    #[serde(default)]
    resolution: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AssignParams {
    /// The desktop id, such as "d2", from list_desktops or start_task.
    desktop_id: String,
    /// The full task for the agent.
    prompt: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DesktopParams {
    /// The desktop id, such as "d2".
    desktop_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitParams {
    /// The desktop id, such as "d2".
    desktop_id: String,
    /// Longest wait in seconds, default 300, at most 1800.
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[tool_router]
impl DesktopsMcp {
    #[tool(
        name = "list_desktops",
        description = "List every agent desktop with its status (running, needs, idle), task, last action and who has control.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_desktops(&self) -> Result<CallToolResult, ErrorData> {
        let rows = self
            .call("GET", "/api/desktops", None)
            .await
            .map_err(tool_error)?;
        let summary: Vec<Value> = rows
            .as_array()
            .map(|rows| rows.iter().map(summarize).collect())
            .unwrap_or_default();
        text(json!({"desktops": summary, "viewer": self.viewer()}))
    }

    #[tool(
        name = "start_task",
        description = "Run one task in isolation: create a new agent desktop and start a sandboxed Claude agent on it with this prompt. Use it to run several tasks in parallel, or for any task the user wants kept away from their own session. Returns the desktop id. Follow up with wait_for_desktop or desktop_status.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn start_task(
        &self,
        Parameters(p): Parameters<StartTaskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let created = self
            .call("POST", "/api/desktops", Some(json!({"name": p.name, "resolution": p.resolution.unwrap_or_else(|| "1280x800".into())})))
            .await
            .map_err(tool_error)?;
        let id = created["id"].as_str().unwrap_or_default().to_string();
        let row = self
            .call(
                "POST",
                &format!("/api/desktops/{id}/agent"),
                Some(json!({"prompt": p.prompt})),
            )
            .await
            .map_err(tool_error)?;
        text(json!({"started": summarize(&row), "watch": format!("{}/#focus/{id}", self.viewer())}))
    }

    #[tool(
        name = "create_desktop",
        description = "Create an empty agent desktop without an agent. Use assign_task to give it work later.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_desktop(
        &self,
        Parameters(p): Parameters<CreateParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let row = self
            .call("POST", "/api/desktops", Some(json!({"name": p.name, "resolution": p.resolution.unwrap_or_else(|| "1280x800".into())})))
            .await
            .map_err(tool_error)?;
        text(summarize(&row))
    }

    #[tool(
        name = "assign_task",
        description = "Start a sandboxed Claude agent on an existing desktop that has none.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn assign_task(
        &self,
        Parameters(p): Parameters<AssignParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let row = self
            .call(
                "POST",
                &format!("/api/desktops/{}/agent", p.desktop_id),
                Some(json!({"prompt": p.prompt})),
            )
            .await
            .map_err(tool_error)?;
        text(summarize(&row))
    }

    #[tool(
        name = "desktop_status",
        description = "One desktop's status and its recent agent actions, newest first. Status needs means the agent waits on a person.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn desktop_status(
        &self,
        Parameters(p): Parameters<DesktopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let row = self
            .call("GET", &format!("/api/desktops/{}", p.desktop_id), None)
            .await
            .map_err(tool_error)?;
        let mut out = summarize(&row);
        out["log"] = row["log"].clone();
        text(out)
    }

    #[tool(
        name = "wait_for_desktop",
        description = "Wait until a desktop's agent stops working (idle) or needs a person (needs), then return its status and recent actions.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn wait_for_desktop(
        &self,
        Parameters(p): Parameters<WaitParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = Duration::from_secs(p.timeout_seconds.unwrap_or(300).min(1800));
        let start = Instant::now();
        // Let a just-started agent leave idle before the first check counts.
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let row = self
                .call("GET", &format!("/api/desktops/{}", p.desktop_id), None)
                .await
                .map_err(tool_error)?;
            let status = row["status"].as_str().unwrap_or("idle");
            if status != "running" || start.elapsed() >= limit {
                let mut out = summarize(&row);
                out["log"] = row["log"].clone();
                out["timed_out"] = json!(status == "running");
                return text(out);
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }

    #[tool(
        name = "desktop_screenshot",
        description = "A small screenshot of one desktop, to check an agent's progress by eye.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn desktop_screenshot(
        &self,
        Parameters(p): Parameters<DesktopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let (status, _, body) = self
            .request(
                "GET",
                &format!("/api/desktops/{}/thumb.webp", p.desktop_id),
                None,
            )
            .await
            .map_err(tool_error)?;
        if status != 200 {
            return Err(tool_error(anyhow!("no screenshot: HTTP {status}")));
        }
        let data = base64::engine::general_purpose::STANDARD.encode(body);
        Ok(CallToolResult::success(vec![ContentBlock::image(
            data,
            "image/webp".to_string(),
        )]))
    }

    #[tool(
        name = "stop_desktop",
        description = "Stop a desktop: end its agent, its apps and the desktop itself. Collect what you need first.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn stop_desktop(
        &self,
        Parameters(p): Parameters<DesktopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("DELETE", &format!("/api/desktops/{}", p.desktop_id), None)
            .await
            .map_err(tool_error)?;
        text(json!({"stopped": p.desktop_id}))
    }
}

#[tool_handler(
    router = self.tool_router.clone(),
    name = "agent-desktops",
    version = "0.8.2",
    instructions = "Agent desktops run tasks in isolation. Each desktop is a hidden Wayland desktop with its own sandboxed Claude agent, which can see only its own folder and screen. Use start_task when the user asks for parallel work, or wants a task kept away from their own session. Give each task a full, self-contained prompt. Then call wait_for_desktop, read desktop_status, and check desktop_screenshot when unsure. Status needs means the agent waits on a person: tell the user and share the watch link. Stop desktops you no longer need. The user can watch and take over any desktop in the viewer."
)]
impl ServerHandler for DesktopsMcp {}

impl DesktopsMcp {
    fn viewer(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, _, bytes) = self.request(method, path, body).await?;
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if !(200..300).contains(&status) {
            let message = value["error"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| format!("HTTP {status}"));
            bail!("{message}");
        }
        Ok(value)
    }

    /// One HTTP/1.1 request to the daemon on localhost, read to the end.
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(u16, String, Vec<u8>)> {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .with_context(|| format!("the agent desktops daemon is not running on port {}; start it with `computer-use-linux desktops serve` or `desktops install`", self.port))?;
        let payload = body.map(|body| body.to_string()).unwrap_or_default();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        );
        stream.write_all(head.as_bytes()).await?;
        stream.write_all(payload.as_bytes()).await?;
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(60), stream.read_to_end(&mut raw))
            .await
            .context("the daemon did not answer in time")??;
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| anyhow!("malformed reply from the daemon"))?;
        let headers = String::from_utf8_lossy(&raw[..split]).to_string();
        let status = headers
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let mut body = raw[split + 4..].to_vec();
        if headers
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            body = dechunk(&body);
        }
        Ok((status, headers, body))
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = data.windows(2).position(|window| window == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&data[..line_end]).trim(), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        let end = (start + size).min(data.len());
        out.extend_from_slice(&data[start..end]);
        data = data.get(end + 2..).unwrap_or_default();
    }
    out
}

/// The fields an orchestrating agent needs from a desktop row.
fn summarize(row: &Value) -> Value {
    json!({
        "id": row["id"],
        "name": row["name"],
        "status": row["status"],
        "task": row["task"],
        "agent": row["agent"],
        "last": row["last"],
        "ago": row["ago"],
        "control": row["control"],
        "display": row["display"],
    })
}

fn text(value: Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(&value).unwrap_or_default(),
    )]))
}

fn tool_error(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(format!("{error:#}"), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dechunks_a_chunked_body() {
        assert_eq!(
            dechunk(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n"),
            b"hello world"
        );
    }

    #[test]
    fn summary_keeps_the_fields_an_agent_needs() {
        let row =
            json!({"id": "d1", "name": "x", "status": "needs", "log": [1, 2], "vnc_port": 5901});
        let out = summarize(&row);
        assert_eq!(out["status"], "needs");
        assert!(out.get("vnc_port").is_none());
    }
}
