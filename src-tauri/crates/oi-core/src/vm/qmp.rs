//! QEMU Machine Protocol client.
//!
//! QMP is the only control channel the launcher uses: it is bound to loopback on a randomly
//! allocated port and is never exposed to the network (`OILinux_PRD.md` §6.2). All commands used
//! here are capability negotiation, status querying and the two shutdown paths.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::error::{OiError, OiResult};

pub struct QmpClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    next_id: u64,
    pending_events: Vec<Value>,
}

impl QmpClient {
    /// Connect, read the greeting and negotiate capabilities.
    pub async fn connect(port: u16, timeout: Duration) -> OiResult<Self> {
        let stream = tokio::time::timeout(
            timeout,
            TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .map_err(|_| OiError::timeout(format!("QMP did not accept a connection on port {port}")))?
        .map_err(|e| OiError::vm(format!("could not connect to QMP on port {port}: {e}")))?;

        let (read, write) = stream.into_split();
        let mut client = QmpClient {
            reader: BufReader::new(read),
            writer: write,
            next_id: 1,
            pending_events: Vec::new(),
        };

        // greeting
        let greeting = client.read_message(timeout).await?;
        if greeting.get("QMP").is_none() {
            return Err(OiError::vm(format!(
                "the QMP greeting was not understood: {greeting}"
            )));
        }
        // capabilities
        let res = client
            .execute_with_timeout("qmp_capabilities", None, timeout)
            .await?;
        if !res.is_null() && res.get("error").is_some() {
            return Err(OiError::vm(format!(
                "qmp_capabilities was refused: {res}"
            )));
        }
        Ok(client)
    }

    async fn read_message(&mut self, timeout: Duration) -> OiResult<Value> {
        let mut line = String::new();
        let n = tokio::time::timeout(timeout, self.reader.read_line(&mut line))
            .await
            .map_err(|_| OiError::timeout("timed out waiting for a QMP message"))?
            .map_err(|e| OiError::vm(format!("QMP read failed: {e}")))?;
        if n == 0 {
            return Err(OiError::vm("the QMP channel was closed by QEMU"));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Err(OiError::vm(
                "QMP produced an empty line, which is not valid protocol traffic",
            ));
        }
        serde_json::from_str(trimmed)
            .map_err(|e| OiError::vm(format!("QMP produced invalid JSON ({e}): {trimmed}")))
    }

    /// Send a command and return its `return` value, ignoring asynchronous events that arrive in
    /// between.
    pub async fn execute(&mut self, cmd: &str, args: Option<Value>) -> OiResult<Value> {
        self.execute_with_timeout(cmd, args, Duration::from_secs(10))
            .await
    }

    pub async fn execute_with_timeout(
        &mut self,
        cmd: &str,
        args: Option<Value>,
        timeout: Duration,
    ) -> OiResult<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = json!({ "execute": cmd, "id": id });
        if let Some(a) = args {
            request["arguments"] = a;
        }
        let mut payload = serde_json::to_string(&request)?;
        payload.push_str("\r\n");
        self.writer
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| OiError::vm(format!("QMP write failed: {e}")))?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .unwrap_or(Duration::from_millis(1));
            if remaining.is_zero() {
                return Err(OiError::timeout(format!(
                    "QMP command {cmd} did not return within {timeout:?}"
                )));
            }
            let msg = self.read_message(remaining).await?;
            if let Some(ev) = msg.get("event") {
                tracing::debug!(event = %ev, "qmp event");
                self.pending_events.push(msg);
                continue;
            }
            if msg.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(OiError::vm(format!("QMP command {cmd} failed: {err}")));
                }
                return Ok(msg.get("return").cloned().unwrap_or(Value::Null));
            }
            tracing::debug!(message = %msg, "unmatched QMP message");
        }
    }

    /// `system_powerdown`: ask the guest OS to shut down cleanly.
    pub async fn system_powerdown(&mut self) -> OiResult<()> {
        self.execute("system_powerdown", None).await.map(|_| ())
    }

    /// `quit`: terminate QEMU immediately. Only used after a graceful shutdown timed out.
    pub async fn quit(&mut self) -> OiResult<()> {
        self.execute("quit", None).await.map(|_| ())
    }

    pub async fn status(&mut self) -> OiResult<String> {
        let v = self.execute("query-status", None).await?;
        Ok(v.get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string())
    }

    pub async fn events(&self) -> &[Value] {
        &self.pending_events
    }
}

/// Wait until the QMP socket answers, or give up after `timeout`.
pub async fn wait_until_listening(port: u16, timeout: Duration) -> OiResult<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if QmpClient::connect(port, Duration::from_secs(2)).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(OiError::timeout(format!(
                "QEMU never opened its QMP channel on port {port}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
