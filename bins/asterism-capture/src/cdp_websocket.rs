use std::time::Duration;

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rand_core::{OsRng, RngCore};
use reqwest::Url;
use serde_json::Value;
use sha1::{Digest, Sha1};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use zeroize::{Zeroize, Zeroizing};

const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Minimal loopback-only WebSocket client for Chromium `DevTools`. The frame and
/// JSON buffers are bounded and zeroized because CDP responses can contain
/// credential-bearing headers and browser storage.
pub struct CdpWebSocket {
    stream: TcpStream,
}

impl CdpWebSocket {
    /// Connects to a loopback `ws://` `DevTools` endpoint and verifies the RFC
    /// 6455 upgrade response.
    ///
    /// # Errors
    ///
    /// Rejects non-loopback/non-WebSocket endpoints, invalid handshakes and
    /// transport failures.
    pub async fn connect(endpoint: &str) -> anyhow::Result<Self> {
        let url = Url::parse(endpoint).context("DevTools WebSocket URL is invalid")?;
        let host = url
            .host_str()
            .context("DevTools WebSocket URL has no host")?;
        if url.scheme() != "ws"
            || !matches!(host, "127.0.0.1" | "localhost" | "::1")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            bail!("DevTools WebSocket must be an unauthenticated loopback ws:// endpoint");
        }
        let port = url
            .port_or_known_default()
            .context("DevTools WebSocket URL has no port")?;
        let mut stream = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((host, port)))
            .await
            .context("DevTools WebSocket connection timed out")?
            .context("failed to connect to DevTools WebSocket")?;

        let mut nonce = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let key = STANDARD.encode(nonce);
        nonce.zeroize();
        let route = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let host_header = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let request = format!(
            "GET {route} HTTP/1.1\r\nHost: {host_header}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        tokio::time::timeout(IO_TIMEOUT, stream.write_all(request.as_bytes()))
            .await
            .context("DevTools WebSocket handshake timed out")?
            .context("failed to write DevTools WebSocket handshake")?;
        let response = read_http_head(&mut stream).await?;
        validate_handshake(&response, &key)?;
        Ok(Self { stream })
    }

    /// Sends one bounded JSON command in a masked text frame.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization, framing or the transport fails.
    pub async fn send_json(&mut self, value: &Value) -> anyhow::Result<()> {
        let payload = Zeroizing::new(
            serde_json::to_vec(value).context("failed to serialize DevTools command")?,
        );
        self.send_frame(0x1, &payload).await
    }

    /// Receives the next bounded JSON text frame, transparently answering
    /// ping frames. Returned JSON recursively zeroizes all owned strings.
    ///
    /// # Errors
    ///
    /// Rejects fragmented/binary/masked server frames, oversized payloads,
    /// invalid JSON, closed connections and transport timeouts.
    pub async fn receive_json(&mut self) -> anyhow::Result<ZeroizingJson> {
        tokio::time::timeout(IO_TIMEOUT, self.receive_json_inner())
            .await
            .context("DevTools WebSocket response timed out")?
    }

    async fn receive_json_inner(&mut self) -> anyhow::Result<ZeroizingJson> {
        loop {
            let (opcode, payload) = self.read_frame().await?;
            match opcode {
                0x1 => {
                    let value = serde_json::from_slice(&payload)
                        .context("DevTools returned invalid JSON")?;
                    return Ok(ZeroizingJson(value));
                }
                0x8 => bail!("DevTools WebSocket closed"),
                0x9 => self.send_frame(0xA, &payload).await?,
                0xA => {}
                _ => bail!("DevTools returned an unsupported WebSocket frame"),
            }
        }
    }

    async fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> anyhow::Result<()> {
        if payload.len() > MAX_FRAME_BYTES {
            bail!("DevTools WebSocket command exceeds its safety limit");
        }
        let mut mask = [0_u8; 4];
        OsRng.fill_bytes(&mut mask);
        let mut frame = Zeroizing::new(Vec::with_capacity(payload.len() + 14));
        frame.push(0x80 | opcode);
        match payload.len() {
            length @ 0..=125 => frame.push(0x80 | u8::try_from(length)?),
            length @ 126..=65_535 => {
                frame.push(0x80 | 0x7e);
                frame.extend_from_slice(&u16::try_from(length)?.to_be_bytes());
            }
            length => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&u64::try_from(length)?.to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        mask.zeroize();
        tokio::time::timeout(IO_TIMEOUT, self.stream.write_all(&frame))
            .await
            .context("DevTools WebSocket write timed out")?
            .context("failed to write DevTools WebSocket frame")
    }

    async fn read_frame(&mut self) -> anyhow::Result<(u8, Zeroizing<Vec<u8>>)> {
        let mut prefix = [0_u8; 2];
        self.stream
            .read_exact(&mut prefix)
            .await
            .context("failed to read DevTools WebSocket frame")?;
        let final_frame = prefix[0] & 0x80 != 0;
        let reserved_bits = prefix[0] & 0x70;
        let opcode = prefix[0] & 0x0f;
        let masked = prefix[1] & 0x80 != 0;
        if !final_frame || masked || reserved_bits != 0 || !matches!(opcode, 0x1 | 0x8..=0xA) {
            bail!("DevTools returned a fragmented or masked WebSocket frame");
        }
        let mut length = u64::from(prefix[1] & 0x7f);
        if length == 126 {
            let mut extended = [0_u8; 2];
            self.stream.read_exact(&mut extended).await?;
            length = u64::from(u16::from_be_bytes(extended));
        } else if length == 127 {
            let mut extended = [0_u8; 8];
            self.stream.read_exact(&mut extended).await?;
            length = u64::from_be_bytes(extended);
        }
        let length = usize::try_from(length).context("DevTools frame length overflowed")?;
        if length > MAX_FRAME_BYTES || (opcode >= 0x8 && length > 125) {
            bail!("DevTools WebSocket response exceeds its safety limit");
        }
        let mut payload = Zeroizing::new(vec![0_u8; length]);
        self.stream
            .read_exact(&mut payload)
            .await
            .context("failed to read DevTools WebSocket payload")?;
        Ok((opcode, payload))
    }
}

pub struct ZeroizingJson(Value);

impl ZeroizingJson {
    pub fn value(&self) -> &Value {
        &self.0
    }

    #[cfg(test)]
    pub fn value_mut(&mut self) -> &mut Value {
        &mut self.0
    }
}

impl fmt::Debug for ZeroizingJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ZeroizingJson([REDACTED])")
    }
}

impl Drop for ZeroizingJson {
    fn drop(&mut self) {
        zeroize_json(&mut self.0);
    }
}

fn zeroize_json(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => {
            for item in items.iter_mut() {
                zeroize_json(item);
            }
            items.clear();
        }
        Value::Object(object) => {
            let entries = std::mem::take(object);
            for (mut key, mut value) in entries {
                key.zeroize();
                zeroize_json(&mut value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

async fn read_http_head(stream: &mut TcpStream) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    let mut response = Zeroizing::new(Vec::with_capacity(1024));
    while !response.ends_with(b"\r\n\r\n") {
        if response.len() >= MAX_HANDSHAKE_BYTES {
            bail!("DevTools WebSocket handshake is oversized");
        }
        let mut byte = [0_u8; 1];
        tokio::time::timeout(IO_TIMEOUT, stream.read_exact(&mut byte))
            .await
            .context("DevTools WebSocket handshake timed out")?
            .context("failed to read DevTools WebSocket handshake")?;
        response.push(byte[0]);
    }
    Ok(response)
}

fn validate_handshake(response: &[u8], key: &str) -> anyhow::Result<()> {
    let response = std::str::from_utf8(response).context("WebSocket handshake is not UTF-8")?;
    let mut lines = response.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status
        .strip_prefix("HTTP/1.1 101")
        .is_some_and(|reason| reason.is_empty() || reason.starts_with(' '))
    {
        bail!("DevTools rejected the WebSocket upgrade with {status}");
    }
    let mut accept = None;
    let mut upgrade = false;
    let mut connection = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("sec-websocket-accept") {
            accept = Some(value);
        } else if name.eq_ignore_ascii_case("upgrade") && value.eq_ignore_ascii_case("websocket") {
            upgrade = true;
        } else if name.eq_ignore_ascii_case("connection")
            && value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        {
            connection = true;
        }
    }
    let expected = STANDARD.encode(Sha1::digest(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
    if accept != Some(expected.as_str()) || !upgrade || !connection {
        bail!("DevTools returned an invalid WebSocket upgrade response");
    }
    Ok(())
}

use std::fmt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_websocket_acceptance_is_exact() {
        let response = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        assert!(validate_handshake(response, "dGhlIHNhbXBsZSBub25jZQ==").is_ok());

        let chromium_response = b"HTTP/1.1 101 WebSocket Protocol Handshake\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
        assert!(validate_handshake(chromium_response, "dGhlIHNhbXBsZSBub25jZQ==").is_ok());

        let wrong = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: wrong\r\n\r\n";
        assert!(validate_handshake(wrong, "dGhlIHNhbXBsZSBub25jZQ==").is_err());
    }

    #[test]
    fn zeroizing_json_redacts_debug_and_clears_nested_strings() {
        let mut value = ZeroizingJson(serde_json::json!({"Authorization": ["secret"]}));
        assert_eq!(format!("{value:?}"), "ZeroizingJson([REDACTED])");
        zeroize_json(value.value_mut());
        assert_eq!(value.value(), &serde_json::json!({}));
    }
}
