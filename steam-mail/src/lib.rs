use std::net::SocketAddr;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// Minimal SMTP server that extracts Steam Guard codes from incoming emails.
pub struct SteamMailServer {
    code_rx: mpsc::Receiver<String>,
    local_addr: SocketAddr,
}

impl SteamMailServer {
    /// Bind to `addr` and start accepting SMTP connections.
    ///
    /// Spawns a background task that handles the SMTP handshake and
    /// extracts Steam Guard codes from email bodies.
    pub async fn new(addr: impl tokio::net::ToSocketAddrs) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let (code_tx, code_rx) = mpsc::channel(16);

        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let tx = code_tx.clone();
                tokio::spawn(async move {
                    let _ = handle_smtp(stream, tx).await;
                });
            }
        });

        Ok(Self { code_rx, local_addr })
    }

    /// The local address this server is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Wait for the next Steam Guard code to arrive via email.
    pub async fn recv_code(&mut self) -> Option<String> {
        self.code_rx.recv().await
    }
}

async fn handle_smtp(
    stream: tokio::net::TcpStream,
    code_tx: mpsc::Sender<String>,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    writer.write_all(b"220 steamdepot ESMTP\r\n").await?;

    let mut in_data = false;
    let mut body = String::new();

    while let Some(line) = lines.next_line().await? {
        if in_data {
            if line == "." {
                in_data = false;
                if let Some(code) = extract_guard_code(&body) {
                    let _ = code_tx.send(code).await;
                }
                body.clear();
                writer.write_all(b"250 OK\r\n").await?;
            } else {
                body.push_str(&line);
                body.push('\n');
            }
            continue;
        }

        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            writer.write_all(b"250 Hello\r\n").await?;
        } else if upper.starts_with("MAIL FROM") {
            writer.write_all(b"250 OK\r\n").await?;
        } else if upper.starts_with("RCPT TO") {
            writer.write_all(b"250 OK\r\n").await?;
        } else if upper == "DATA" {
            writer
                .write_all(b"354 Start mail input; end with <CRLF>.<CRLF>\r\n")
                .await?;
            in_data = true;
        } else if upper == "QUIT" {
            writer.write_all(b"221 Bye\r\n").await?;
            break;
        } else if upper == "RSET" {
            body.clear();
            writer.write_all(b"250 OK\r\n").await?;
        } else {
            writer.write_all(b"250 OK\r\n").await?;
        }
    }

    Ok(())
}

/// Extract a 5-character alphanumeric Steam Guard code from an email body.
///
/// Steam Guard emails contain the code on its own line or in a pattern like
/// "Your Steam Guard code is: XXXXX" or just the 5-char code surrounded by
/// whitespace/newlines.
fn extract_guard_code(body: &str) -> Option<String> {
    // Try pattern: "code is: XXXXX" or "code: XXXXX"
    for line in body.lines() {
        let trimmed = line.trim();

        // Look for explicit "code" mentions
        if let Some(pos) = trimmed.to_ascii_lowercase().find("code") {
            let after = &trimmed[pos + 4..];
            // Skip "is", ":", whitespace
            let after = after
                .trim_start_matches(|c: char| c == ':' || c == ' ' || c.eq_ignore_ascii_case(&'i') || c.eq_ignore_ascii_case(&'s'));
            let candidate: String = after.chars().take_while(|c| c.is_alphanumeric()).collect();
            if candidate.len() == 5 && candidate.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some(candidate);
            }
        }
    }

    // Fallback: look for a standalone 5-char alphanumeric token on its own line
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.len() == 5 && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Some(trimmed.to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_code_with_label() {
        let body = "Subject: Steam Guard code\n\nYour Steam Guard code is: F4K2N\n\nThanks.";
        assert_eq!(extract_guard_code(body), Some("F4K2N".to_string()));
    }

    #[test]
    fn extract_code_standalone_line() {
        let body = "Some header stuff\n\nAB3XY\n\nFooter";
        assert_eq!(extract_guard_code(body), Some("AB3XY".to_string()));
    }

    #[test]
    fn no_code_found() {
        let body = "Hello, this is a regular email with no code.";
        assert_eq!(extract_guard_code(body), None);
    }

    #[tokio::test]
    async fn smtp_server_binds() {
        let server = SteamMailServer::new("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr();
        assert_ne!(addr.port(), 0);
    }
}
