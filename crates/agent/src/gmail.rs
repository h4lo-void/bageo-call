// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Gmail SMTP reply sender (Google app password, AUTH LOGIN + STARTTLS), and
//! the same conversation with any other SMTP server for the "Email (SMTP)"
//! notification channel ([`send_mail_via`]).
//!
//! Settings (see [`crate::settings_store::RuntimeSettings`]):
//! `gmail_user`, `gmail_app_password`, `gmail_from_name`.
//!
//! Sequence: TCP → EHLO → STARTTLS → rustls upgrade → EHLO → AUTH LOGIN →
//! MAIL/RCPT/DATA → QUIT (`smtp.gmail.com:587`). Servers on port 465 speak TLS
//! from the first byte instead: TCP → rustls → greeting → EHLO → ….

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tracing::info;
use uuid::Uuid;

use crate::locale;
use crate::notify::ChannelError;

const SMTP_HOST: &str = "smtp.gmail.com";
const SMTP_PORT: u16 = 587;
const EHLO_NAME: &str = "bageo-call";
/// A server that never answers (a firewall dropping the port) must not hold a
/// notification for minutes.
const SMTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Where an SMTP server listens and how its TLS starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpServer {
    pub host: String,
    pub port: u16,
    /// TLS from the first byte (port 465) instead of STARTTLS.
    pub implicit_tls: bool,
}

impl SmtpServer {
    fn gmail() -> Self {
        Self { host: SMTP_HOST.to_string(), port: SMTP_PORT, implicit_tls: false }
    }
}

fn bad_url(detail: &str) -> anyhow::Error {
    ChannelError::new("bad_value", "url", detail).into()
}

/// The server and login from the channel's one address field:
/// `smtp://login@host:port` (STARTTLS), `smtps://login@host:port` (TLS from the
/// start), or the same without the scheme. The login is usually an email
/// address, so the host starts after the last `@`; `%40` is accepted for the
/// login's own `@`. Port 465 always means TLS from the start; the default port
/// is 587 (465 for `smtps://`).
pub fn parse_smtp_url(s: &str) -> Result<(SmtpServer, String)> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ChannelError::new("missing_field", "url", "SMTP URL is empty").into());
    }
    let lower = t.to_ascii_lowercase();
    let (tls_scheme, rest) = if lower.starts_with("smtps://") {
        (true, &t[8..])
    } else if lower.starts_with("smtp://") {
        (false, &t[7..])
    } else if t.contains("://") {
        return Err(bad_url("the SMTP URL must start with smtp:// or smtps://"));
    } else {
        (false, t)
    };
    let rest = rest.trim_end_matches('/');
    let Some((login, hostport)) = rest.rsplit_once('@') else {
        return Err(bad_url("write the login before the server: smtp://you@example.com@smtp.example.com:587"));
    };
    let login = login.replace("%40", "@");
    if login.contains(':') {
        // "login:password@server" would make the password part of the login, and
        // the login is written to the log.
        return Err(bad_url("write only the login in the URL; the password goes in the password field"));
    }
    if !looks_like_address(&login) {
        return Err(bad_url("the login must be the sender's email address"));
    }
    let (host, port) = match hostport.rsplit_once(':') {
        Some((host, port)) => {
            let port: u16 = port.parse().map_err(|_| bad_url("the port must be a number such as 587"))?;
            (host, port)
        }
        None => (hostport, if tls_scheme { 465 } else { 587 }),
    };
    if host.is_empty() || port == 0 || host.contains(['/', '?', '#', ' ']) {
        return Err(bad_url("the SMTP server name does not look right"));
    }
    let server = SmtpServer { host: host.to_string(), port, implicit_tls: tls_scheme || port == 465 };
    Ok((server, login))
}

fn looks_like_address(s: &str) -> bool {
    s.contains('@') && !s.starts_with('@') && !s.ends_with('@') && !s.contains(breaks_smtp_line) && !s.contains(':')
}

/// An address goes into `MAIL FROM:<…>`, `RCPT TO:<…>` and the headers as it
/// is: a line break, a space or an angle bracket in it would end the command or
/// the header there and start another one.
fn breaks_smtp_line(c: char) -> bool {
    c.is_control() || c.is_whitespace() || c == '<' || c == '>'
}

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

    deliver(&SmtpServer::gmail(), &user, &app_password, &to, &message).await
}

/// Send a plain-text message through any SMTP server that offers TLS (STARTTLS
/// or TLS from the start). The login is also the sender address, which almost
/// every mail provider requires anyway.
pub async fn send_mail_via(
    server: &SmtpServer,
    login: &str,
    password: &str,
    from_name: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<()> {
    let login = login.trim();
    if !looks_like_address(login) {
        return Err(bad_url("the login must be the sender's email address"));
    }
    let password = password.trim();
    if password.is_empty() {
        // Never echo the password field.
        return Err(ChannelError::new("missing_field", "token", "password is empty").into());
    }
    let to = validate_recipient(to)?;
    validate_non_empty("subject", subject)?;
    validate_non_empty("body", body)?;

    let message = format_rfc822(from_name.trim(), login, to, subject.trim(), body)?;
    info!(
        smtp_host = %server.host,
        smtp_port = server.port,
        implicit_tls = server.implicit_tls,
        to = %to,
        bytes = message.len(),
        "smtp: sending"
    );
    deliver(server, login, password, to, &message).await
}

async fn deliver(server: &SmtpServer, user: &str, password: &str, to: &str, message: &str) -> Result<()> {
    let session = async {
        if server.implicit_tls {
            let tcp = tcp_connect(server).await?;
            let tls = tls_upgrade(tcp, &server.host).await?;
            smtp_session(tls, true, user, password, to, message).await
        } else {
            let plain = smtp_connect_starttls(server).await?;
            let tls = tls_upgrade(plain, &server.host).await?;
            smtp_session(tls, false, user, password, to, message).await
        }
    };
    tokio::time::timeout(SMTP_TIMEOUT, session).await.map_err(|_| {
        anyhow::Error::from(ChannelError::new(
            "network_error",
            "",
            format!("smtp {}:{} did not answer within {} s", server.host, server.port, SMTP_TIMEOUT.as_secs()),
        ))
    })?
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
    smtp_session(stream, false, user, app_password, to, rfc822).await
}

/// The SMTP conversation on a TLS stream. `greeting`: the server's 220 greeting
/// is still unread (TLS from the start); after STARTTLS there is none.
async fn smtp_session<S>(stream: S, greeting: bool, user: &str, password: &str, to: &str, rfc822: &str) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    if greeting {
        smtp_expect_code(&mut reader, 220).await.context("smtp greeting")?;
    }
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("EHLO {EHLO_NAME}\r\n"),
        250,
    )
    .await
    .context("smtp ehlo")?;

    smtp_cmd_expect(&mut reader, &mut writer, "AUTH LOGIN\r\n", 334)
        .await
        .context("smtp auth login")?;
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("{}\r\n", base64_encode(user.as_bytes())),
        334,
    )
    .await
    .context("smtp auth user")?;
    let reply = smtp_cmd(
        &mut reader,
        &mut writer,
        &format!("{}\r\n", base64_encode(password.as_bytes())),
    )
    .await
    .context("smtp auth password")?;
    if let Some(e) = auth_reply_error(&reply) {
        return Err(e);
    }

    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("MAIL FROM:<{user}>\r\n"),
        250,
    )
    .await
    .context("smtp mail from")?;
    smtp_cmd_expect(
        &mut reader,
        &mut writer,
        &format!("RCPT TO:<{to}>\r\n"),
        250,
    )
    .await
    .context("smtp rcpt to")?;
    smtp_cmd_expect(&mut reader, &mut writer, "DATA\r\n", 354)
        .await
        .context("smtp data")?;

    let payload = format_smtp_data_payload(rfc822);
    writer
        .write_all(payload.as_bytes())
        .await
        .context("smtp data write")?;
    writer.flush().await.context("smtp data flush")?;
    smtp_expect_code(&mut reader, 250)
        .await
        .context("smtp data complete")?;

    let _ = smtp_cmd(&mut reader, &mut writer, "QUIT\r\n").await;
    info!(smtp_user = user, to, "smtp: message accepted");
    Ok(())
}

async fn tcp_connect(server: &SmtpServer) -> Result<TcpStream> {
    let addr = format!("{}:{}", server.host, server.port);
    TcpStream::connect(&addr)
        .await
        .map_err(|e| ChannelError::new("network_error", "", format!("smtp tcp connect {addr}: {e}")).into())
}

/// TCP connect + EHLO + STARTTLS; returns the plain socket ready for TLS upgrade.
async fn smtp_connect_starttls(server: &SmtpServer) -> Result<TcpStream> {
    let stream = tcp_connect(server).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    smtp_expect_code(&mut reader, 220)
        .await
        .context("smtp banner")?;
    smtp_cmd(&mut reader, &mut writer, &format!("EHLO {EHLO_NAME}\r\n"))
        .await
        .context("smtp ehlo")?;
    smtp_cmd_expect(&mut reader, &mut writer, "STARTTLS\r\n", 220)
        .await
        .context("smtp starttls")?;

    let reader_inner = reader.into_inner();
    let stream = reader_inner
        .reunite(writer)
        .map_err(|_| anyhow::anyhow!("smtp: failed to reunite TCP halves after STARTTLS"))?;
    Ok(stream)
}

async fn tls_upgrade(stream: TcpStream, host: &str) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(host.to_string())
        .map_err(|_| ChannelError::new("bad_value", "url", format!("{host:?} is not a valid TLS server name")))?;
    connector
        .connect(server_name, stream)
        .await
        .context("smtp tls handshake")
}

/// Gmail address or app password not set, in the language the owner reads
/// (`notify_language`; also what a task follow-up shows). Never include
/// credential values in an error.
pub fn gmail_missing_cred_msg() -> &'static str {
    texts().f_gmail_missing
}

fn texts() -> &'static locale::Phrases {
    owner_texts(&locale::active())
}

/// What the owner reads: the language of `notify_language`, which is the AI's
/// own unless set apart.
fn owner_texts(active: &locale::Active) -> &'static locale::Phrases {
    locale::phrases(active.notify_lang)
}

fn validate_gmail_user(user: &str) -> Result<&str> {
    let user = user.trim();
    if user.is_empty() {
        // Defense in depth: fulfill and the notification channel check first.
        return Err(ChannelError::new("missing_field", "url", gmail_missing_cred_msg()).into());
    }
    if !user.contains('@') || user.starts_with('@') || user.ends_with('@') || user.contains(breaks_smtp_line) {
        return Err(ChannelError::new("bad_value", "url", texts().g_bad_address).into());
    }
    Ok(user)
}

fn validate_recipient(to: &str) -> Result<&str> {
    let to = to.trim();
    if to.is_empty() {
        return Err(ChannelError::new("missing_field", "target", texts().g_no_recipient).into());
    }
    if !to.contains('@') || to.contains(breaks_smtp_line) {
        return Err(ChannelError::new("bad_value", "target", texts().g_bad_recipient).into());
    }
    Ok(to)
}

fn validate_app_password(app_password: &str) -> Result<&str> {
    let app_password = app_password.trim();
    if app_password.is_empty() {
        // Never echo the password field.
        return Err(ChannelError::new("missing_field", "token", gmail_missing_cred_msg()).into());
    }
    if app_password.len() < 8 {
        return Err(ChannelError::new("bad_value", "token", texts().g_short_password).into());
    }
    Ok(app_password)
}

fn validate_non_empty(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("smtp: {field} is required");
    }
    Ok(())
}

fn encode_header_word(value: &str) -> String {
    // One header, one line: a line break in a title would start another header.
    let value = value.replace(['\r', '\n'], " ");
    if value.is_ascii() {
        value
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
        .context("smtp write")?;
    writer.flush().await.context("smtp flush")?;
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
            .context("smtp read")?;
        if n == 0 {
            bail!("smtp: connection closed unexpectedly");
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
        bail!("smtp: expected {expected}, got: {}", reply.trim());
    }
    Ok(())
}

/// The reply to the app password when it is not 235: a 5xx (535 = wrong address
/// or app password) is a refusal the settings page names as such; anything else
/// stays a plain failure.
fn auth_reply_error(reply: &str) -> Option<anyhow::Error> {
    let e = smtp_check_code(reply, 235).err()?;
    Some(if reply.trim_start().starts_with('5') {
        ChannelError::new("auth_failed", "token", format!("smtp auth password: {e:#}")).into()
    } else {
        e.context("smtp auth password")
    })
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
        assert_eq!(msg, gmail_missing_cred_msg());
        // Never leak a password-looking value into the error text.
        assert!(!msg.contains("abcdefgh"));
        // The settings page learns which field is empty.
        let ce = crate::notify::channel_error(&err).unwrap();
        assert_eq!((ce.code, ce.field), ("missing_field", "token"));
        let bad = validate_recipient("not-an-address").unwrap_err();
        let ce = crate::notify::channel_error(&bad).unwrap();
        assert_eq!((ce.code, ce.field), ("bad_value", "target"));
    }

    #[test]
    fn validation_short_password_no_secret_echo() {
        let err = validate_app_password("shortpw").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("過短") || msg.contains("設定"));
        assert!(!msg.contains("shortpw"));
    }

    #[test]
    fn addresses_and_subjects_cannot_add_smtp_commands_or_headers() {
        for bad in [
            "a@example.org>\r\nRCPT TO:<b@example.org",
            "a@example.org\nBcc: b@example.org",
            "a b@example.org",
            "<a@example.org>",
        ] {
            let e = validate_recipient(bad).unwrap_err();
            let ce = crate::notify::channel_error(&e).unwrap();
            assert_eq!((ce.code, ce.field), ("bad_value", "target"), "{bad:?}");
        }
        assert!(validate_recipient(" you@example.org ").is_ok());
        assert!(validate_gmail_user("me@example.org\r\nRCPT TO:<x@example.org>").is_err());
        let msg = format_rfc822("Bageo", "me@example.org", "you@example.org", "Call\r\nBcc: x@example.org", "body").unwrap();
        assert!(msg.contains("Subject: Call  Bcc: x@example.org\r\n"), "{msg}");
        assert!(!msg.contains("\r\nBcc:"), "{msg}");
        // A password written into the URL would become part of the login, which is logged.
        let e = parse_smtp_url("smtp://me@example.org:secret-pw@smtp.example.org:587").unwrap_err();
        let ce = crate::notify::channel_error(&e).unwrap();
        assert_eq!((ce.code, ce.field), ("bad_value", "url"));
        assert!(!format!("{e:#}").contains("secret-pw"));
    }

    #[test]
    fn only_a_refusal_counts_as_a_wrong_password() {
        let e = auth_reply_error("535-5.7.8 Username and Password not accepted.\r\n").unwrap();
        assert_eq!(crate::notify::channel_error(&e).map(|c| c.code), Some("auth_failed"));
        // A busy server is not a wrong password.
        let e = auth_reply_error("454 4.7.0 Too many login attempts, please try again later\r\n").unwrap();
        assert!(crate::notify::channel_error(&e).is_none(), "{e:#}");
        assert!(auth_reply_error("235 2.7.0 Accepted\r\n").is_none());
    }

    #[test]
    fn an_smtp_url_names_the_server_the_login_and_how_tls_starts() {
        let (s, login) = parse_smtp_url("smtp://me@example.org@smtp.example.org:587").unwrap();
        assert_eq!(s, SmtpServer { host: "smtp.example.org".into(), port: 587, implicit_tls: false });
        assert_eq!(login, "me@example.org");
        let (s, login) = parse_smtp_url("SMTPS://me%40example.org@mail.example.org").unwrap();
        assert_eq!((s.host.as_str(), s.port, s.implicit_tls, login.as_str()), ("mail.example.org", 465, true, "me@example.org"));
        // Port 465 means TLS from the start even without a scheme.
        let (s, _) = parse_smtp_url(" me@example.org@mail.example.org:465/ ").unwrap();
        assert_eq!((s.port, s.implicit_tls), (465, true));
        let (s, _) = parse_smtp_url("me@example.org@mail.example.org").unwrap();
        assert_eq!((s.port, s.implicit_tls), (587, false));
        let e = parse_smtp_url(" ").unwrap_err();
        let ce = crate::notify::channel_error(&e).unwrap();
        assert_eq!((ce.code, ce.field), ("missing_field", "url"));
        for bad in [
            "https://me@example.org@smtp.example.org",
            "smtp.example.org:587",
            "smtp://me@example.org@smtp.example.org:abc",
            "smtp://me@example.org@smtp.example.org:0",
            "smtp://nobody@smtp.example.org",
            "smtp://me@example.org@",
        ] {
            let e = parse_smtp_url(bad).unwrap_err();
            let ce = crate::notify::channel_error(&e).unwrap_or_else(|| panic!("{bad}: {e:#}"));
            assert_eq!((ce.code, ce.field), ("bad_value", "url"), "{bad}");
        }
    }

    #[tokio::test]
    async fn a_server_with_tls_from_the_start_is_greeted_before_ehlo() {
        // The stream stands in for the TLS session: without reading the 220
        // greeting first, the client would take it for the answer to EHLO.
        let (client, server) = tokio::io::duplex(8192);
        let fake = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(server);
            let mut r = BufReader::new(r);
            w.write_all(b"220 mail.example.org ESMTP\r\n").await.unwrap();
            let mut seen = Vec::new();
            for reply in [
                "250-mail.example.org\r\n250 AUTH LOGIN PLAIN\r\n",
                "334 VXNlcm5hbWU6\r\n",
                "334 UGFzc3dvcmQ6\r\n",
                "235 2.7.0 Accepted\r\n",
                "250 OK\r\n",
                "250 OK\r\n",
                "354 Go ahead\r\n",
            ] {
                let mut line = String::new();
                r.read_line(&mut line).await.unwrap();
                seen.push(line);
                w.write_all(reply.as_bytes()).await.unwrap();
            }
            let mut data = String::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).await.unwrap() == 0 || line == ".\r\n" {
                    break;
                }
                data.push_str(&line);
            }
            w.write_all(b"250 queued\r\n").await.unwrap();
            let mut quit = String::new();
            r.read_line(&mut quit).await.unwrap();
            w.write_all(b"221 bye\r\n").await.unwrap();
            (seen, data, quit)
        });
        smtp_session(client, true, "me@example.org", "secret-pw", "wang@example.org", "Subject: hi\r\n\r\n.dot line")
            .await
            .unwrap();
        let (seen, data, quit) = fake.await.unwrap();
        assert_eq!(seen[0], "EHLO bageo-call\r\n");
        assert_eq!(seen[1], "AUTH LOGIN\r\n");
        assert_eq!(seen[2], format!("{}\r\n", base64_encode(b"me@example.org")));
        assert_eq!(seen[3], format!("{}\r\n", base64_encode(b"secret-pw")));
        assert_eq!(seen[4], "MAIL FROM:<me@example.org>\r\n");
        assert_eq!(seen[5], "RCPT TO:<wang@example.org>\r\n");
        assert_eq!(seen[6], "DATA\r\n");
        assert!(data.contains("\r\n..dot line\r\n"), "{data:?}");
        assert_eq!(quit, "QUIT\r\n");
    }

    #[test]
    fn gmail_messages_are_in_the_language_the_owner_reads_not_the_one_the_ai_speaks() {
        let active = locale::Active { lang: locale::Lang::En, notify_lang: locale::Lang::Ja, ..Default::default() };
        assert_eq!(owner_texts(&active).f_gmail_missing, locale::phrases(locale::Lang::Ja).f_gmail_missing);
        assert_ne!(owner_texts(&active).f_gmail_missing, locale::phrases(locale::Lang::En).f_gmail_missing);
    }
}
