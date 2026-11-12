//! Gmail SMTP reply sender (Google app password, AUTH LOGIN + STARTTLS).
//!
//! Settings (see [`crate::settings_store::RuntimeSettings`]):
//! `gmail_user`, `gmail_app_password`, `gmail_from_name`.
//!
//! Sequence: TCP → EHLO → STARTTLS → rustls upgrade → EHLO → AUTH LOGIN →
//! MAIL/RCPT/DATA → QUIT (`smtp.gmail.com:587`).

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::info;
use uuid::Uuid;

const SMTP_HOST: &str = "smtp.gmail.com";
const SMTP_PORT: u16 = 587;
const EHLO_NAME: &str = "myna-call";

/// Send a plain-text reply through Gmail SMTP.
///
/// `http` is reserved for a future OAuth/API path; SMTP uses the credential args only.
pub async fn send_mail(
    _http: &Client,
    user: &str,
    app_password: &str,
    from_name: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    let user = validate_gmail_user(user)?.to_string();
    let app_password = validate_app_password(app_password)?.to_string();
    let to = validate_recipient(to)?.to_string();
    validate_non_empty("subject", subject)?;
    validate_non_empty("body", body)?;

    let message = format_rfc822(from_name.trim(), &user, &to, subject.trim(), body)?;
    info!(
        gmail_user = %user,
        to = %to,
        subject = subject.trim(),
        bytes = message.len(),
        "gmail: sending via SMTP STARTTLS"
    );

    let plain = smtp_connect_starttls().await?;
    let tls = tls_upgrade(plain).await?;
    smtp_send_authenticated(tls, &user, &app_password, &to, &message).await
}

/// Build an RFC 5322 message suitable for Gmail SMTP `DATA`.
pub fn format_rfc822(
    from_name: &str,
    from_addr: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<String> {
    let from_addr = validate_gmail_user(from_addr)?;
    let to = validate_recipient(to)?;
    validate_non_empty("subject", subject)?;
    validate_non_empty("body", body)?;

    let date = Utc::now().format("%a, %d %b %Y %H:%M:%S +0000").to_string();
    let message_id = format!("<{}@{}>", Uuid::new_v4(), EHLO_NAME);

    let from_header = if from_name.is_empty() {
        from_addr.to_string()
    } else {
        format!("{} <{}>", encode_header_word(from_name), from_addr)
    };

    let mut msg = String::new();
    msg.push_str(&format!("Date: {date}\r\n"));
    msg.push_str(&format!("Message-ID: {message_id}\r\n"));
    msg.push_str(&format!("From: {from_header}\r\n"));
    msg.push_str(&format!("To: {to}\r\n"));
    msg.push_str(&format!("Subject: {}\r\n", encode_header_word(subject)));
    msg.push_str("MIME-Version: 1.0\r\n");
    msg.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    msg.push_str("Content-Transfer-Encoding: 8bit\r\n");
    msg.push_str("\r\n");
    msg.push_str(&normalize_body_crlf(body));
    Ok(msg)
}

/// Dot-stuff and CRLF-terminate a message for SMTP `DATA`.
pub fn format_smtp_data_payload(rfc822: &str) -> String {
    let mut out = String::with_capacity(rfc822.len() + 8);
    for line in rfc822.replace("\r\n", "\n").split('\n') {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(".\r\n");
    out
}

/// AUTH LOGIN + MAIL/RCPT/DATA on an already TLS-upgraded SMTP stream.
pub async fn smtp_send_authenticated<S>(
    stream: S,
    user: &str,
    app_password: &str,
    to: &str,
    rfc822: &str,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let user = validate_gmail_user(user)?;
    let app_password = validate_app_password(app_password)?;
    let to = validate_recipient(to)?;

    let (reader, writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("EHLO {EHLO_NAME}\r\n"),
        250,
    )
    .await
    .context("gmail smtp ehlo")?;

    smtp_cmd_expect(&mut reader, &mut writer, "AUTH LOGIN\r\n", 334)
        .await
        .context("gmail smtp auth login")?;
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("{}\r\n", base64_encode(user.as_bytes())),
        334,
    )
    .await
    .context("gmail smtp auth user")?;
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("{}\r\n", base64_encode(app_password.as_bytes())),
        235,
    )
    .await
    .context("gmail smtp auth password")?;

    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("MAIL FROM:<{user}>\r\n"),
        250,
    )
    .await
    .context("gmail smtp mail from")?;
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("RCPT TO:<{to}>\r\n"),
        250,
    )
    .await
    .context("gmail smtp rcpt to")?;
    smtp_cmd_expect(&mut reader, &mut writer, "DATA\r\n", 354)
        .await
        .context("gmail smtp data")?;

    let payload = format_smtp_data_payload(rfc822);
    writer
        .write_all(payload.as_bytes())
        .await
        .context("gmail smtp data write")?;
    writer.flush().await.context("gmail smtp data flush")?;
    smtp_expect_code(&mut reader, 250)
        .await
        .context("gmail smtp data complete")?;

    let _ = smtp_cmd(&mut reader, &mut writer, "QUIT\r\n").await;
    info!(gmail_user = user, to, "gmail: SMTP message accepted");
    Ok(())
}

/// TCP connect + EHLO + STARTTLS; returns the plain socket ready for TLS upgrade.
async fn smtp_connect_starttls() -> Result<TcpStream> {
    let addr = format!("{SMTP_HOST}:{SMTP_PORT}");
    let stream = TcpStream::connect(&addr)
        .await
        .with_context(|| format!("gmail tcp connect {addr}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    smtp_expect_code(&mut reader, 220)
        .await
        .context("gmail smtp banner")?;
    smtp_cmd(&mut reader, &mut writer, &format!("EHLO {EHLO_NAME}\r\n"))
        .await
        .context("gmail smtp ehlo")?;
    smtp_cmd_expect(&mut reader, &mut writer, "STARTTLS\r\n", 220)
        .await
        .context("gmail smtp starttls")?;

    let reader_inner = reader.into_inner();
    let stream = reader_inner
        .reunite(writer)
        .map_err(|_| anyhow::anyhow!("gmail: failed to reunite TCP halves after STARTTLS"))?;
    Ok(stream)
}

async fn tls_upgrade(stream: TcpStream) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(SMTP_HOST.to_string())
        .map_err(|_| anyhow::anyhow!("gmail: invalid TLS server name"))?;
    connector
        .connect(server_name, stream)
        .await
        .context("gmail tls handshake")
}

/// Safe ZH when Gmail user/password empty — keep in sync with `fulfill::GMAIL_MISSING_MSG`.
/// Never include credential values in any bail string.
pub const GMAIL_MISSING_CRED_MSG: &str =
    "Gmail 憑證未設定 — 請至「設定」分頁填寫 Gmail 帳號與應用程式密碼後再確認";

fn validate_gmail_user(user: &str) -> Result<&str> {
    let user = user.trim();
    if user.is_empty() {
        // Defense in depth: fulfill should bail first; keep safe ZH (no EN opaque).
        bail!("{GMAIL_MISSING_CRED_MSG}");
    }
    if !user.contains('@') || user.starts_with('@') || user.ends_with('@') {
        bail!("Gmail 帳號格式不正確 — 請至「設定」分頁檢查 Gmail 帳號");
    }
    Ok(user)
}

fn validate_recipient(to: &str) -> Result<&str> {
    let to = to.trim();
    if to.is_empty() {
        bail!("寄件失敗：收件信箱為空");
    }
    if !to.contains('@') {
        bail!("寄件失敗：收件信箱格式不正確");
    }
    Ok(to)
}

fn validate_app_password(app_password: &str) -> Result<&str> {
    let app_password = app_password.trim();
    if app_password.is_empty() {
        // Never echo the password field; same ZH as fulfill missing-cred.
        bail!("{GMAIL_MISSING_CRED_MSG}");
    }
    if app_password.len() < 8 {
        bail!("Gmail 應用程式密碼過短 — 請至「設定」分頁重新填寫（不會顯示密碼內容）");
    }
    Ok(app_password)
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("gmail: {field} is required");
    }
    Ok(())
}

fn encode_header_word(value: &str) -> String {
    if value.is_ascii() {
        value.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", base64_encode(value.as_bytes()))
    }
}

fn normalize_body_crlf(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

async fn smtp_cmd<R, W>(reader: &mut R, writer: &mut W, cmd: &str) -> Result<String>
where
    R: AsyncBufReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    writer
        .write_all(cmd.as_bytes())
        .await
        .context("gmail smtp write")?;
    writer.flush().await.context("gmail smtp flush")?;
    smtp_read_reply(reader).await
}

async fn smtp_cmd_expect<R, W>(
    reader: &mut R,
    writer: &mut W,
    cmd: &str,
    code: u16,
) -> Result<String>
where
    R: AsyncBufReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let reply = smtp_cmd(reader, writer, cmd).await?;
    smtp_check_code(&reply, code)?;
    Ok(reply)
}

async fn smtp_expect_code<R>(reader: &mut R, code: u16) -> Result<String>
where
    R: AsyncBufReadExt + Unpin,
{
    let reply = smtp_read_reply(reader).await?;
    smtp_check_code(&reply, code)?;
    Ok(reply)
}

async fn smtp_read_reply<R>(reader: &mut R) -> Result<String>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .await
            .context("gmail smtp read")?;
        if n == 0 {
            bail!("gmail: SMTP connection closed unexpectedly");
        }
        let complete = line.len() >= 4 && line.as_bytes()[3] == b' ';
        lines.push(line);
        if complete {
            break;
        }
    }
    Ok(lines.concat())
}

fn smtp_check_code(reply: &str, expected: u16) -> Result<()> {
    let code = reply
        .get(..3)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    if code != expected {
        bail!("gmail: SMTP expected {expected}, got: {}", reply.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_rfc822_basic_headers() {
        let msg = format_rfc822(
            "Pixel Agent",
            "bot@gmail.com",
            "caller@example.com",
            "Re: your call",
            "Thanks for calling.",
        )
        .unwrap();
        assert!(msg.contains("From: Pixel Agent <bot@gmail.com>"));
        assert!(msg.contains("To: caller@example.com"));
        assert!(msg.contains("Subject: Re: your call"));
        assert!(msg.contains("Content-Type: text/plain; charset=UTF-8"));
        assert!(msg.contains("\r\n\r\nThanks for calling."));
    }

    #[test]
    fn format_rfc822_utf8_subject_encoded() {
        let msg = format_rfc822("代理", "bot@gmail.com", "a@b.co", "回電確認", "body").unwrap();
        assert!(msg.contains("Subject: =?UTF-8?B?"));
        assert!(msg.contains("From: =?UTF-8?B?"));
    }

    #[test]
    fn dot_stuffing() {
        let payload = format_smtp_data_payload("line one\r\n.line two\r\n");
        // A line starting with a dot gets a second one.
        assert!(payload.contains("\r\n..line two\r\n"));
        assert!(payload.ends_with("\r\n.\r\n"));
    }

    #[test]
    fn base64_roundtrip_chars() {
        assert_eq!(base64_encode(b"user@gmail.com"), "dXNlckBnbWFpbC5jb20=");
    }

    #[test]
    fn validation_rejects_empty_user() {
        let err = validate_gmail_user("  ").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("憑證未設定"));
        assert!(!msg.to_lowercase().contains("credentials not configured"));
    }

    #[test]
    fn validation_rejects_empty_password_zh() {
        let err = validate_app_password("").unwrap_err();
        let msg = err.to_string();
        assert_eq!(msg, GMAIL_MISSING_CRED_MSG);
        // Never leak a password-looking value into the error text.
        assert!(!msg.contains("abcdefgh"));
    }

    #[test]
    fn validation_short_password_no_secret_echo() {
        let err = validate_app_password("shortpw").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("過短") || msg.contains("設定"));
        assert!(!msg.contains("shortpw"));
    }
}
