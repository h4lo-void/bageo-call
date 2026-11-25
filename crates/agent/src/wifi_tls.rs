// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Self-signed TLS (`wss://` and `https://`) with SHA-256 leaf-certificate
//! pinning for phones connecting and pairing over the local network.
//!
//! Why certificate pinning instead of public-CA PKI:
//! Bageo runs on dynamic LAN IP addresses (`192.168.x.y`) and `.local` mDNS
//! names where public CAs cannot issue certificates. On first start Bageo
//! generates an ECDSA P-256 keypair and X.509 v3 server certificate stored in
//! `<calls_data_dir>/config/phone-wifi-tls.der` and `phone-wifi-tls.key`
//! (`0600` on Unix).
//!
//! - **USB-paired phones** receive `wss://<ip>:<port>/ws?token=<key>&pin=<sha256>`
//!   over the loopback `adb reverse` link and verify the server leaf certificate's
//!   SHA-256 digest before sending the token or call audio.
//! - **IP-paired phones** connect over `https://` to `/api/phone-pairing/claim`,
//!   bind their 6-digit pairing code to the observed TLS leaf certificate digest
//!   via `HMAC-SHA256(code, "bageo-claim-req-v1:<cnonce>:<cert_sha256>")` (so the
//!   code is never sent in cleartext and cannot be replayed by an active MITM on
//!   a different certificate), verify the server's `proof` MAC, and pin all
//!   subsequent `wss://` control and PCM audio connections to `<cert_sha256>`.
//! - **Backward compatibility**: [`serve_dual`] peeks at the first byte of each
//!   accepted TCP stream (`0x16` = TLS handshake record) so upgraded phones use
//!   `wss://`/`https://` while un-upgraded phones and local browsers can still
//!   speak `ws://`/`http://` on the same port unless `tls_only`
//!   (`PHONE_WIFI_TLS_ONLY=true`) is enabled.

use std::future::Future;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ConnectInfo;
use axum::Router;
use hmac::{Hmac, Mac};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

const CERT_FILE: &str = "phone-wifi-tls.der";
const KEY_FILE: &str = "phone-wifi-tls.key";
const PEEK_TIMEOUT: Duration = Duration::from_secs(5);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct WifiTls {
    acceptor: TlsAcceptor,
    pin_sha256: String,
    cert_der: Vec<u8>,
}

impl WifiTls {
    /// Load or create the local phone Wi-Fi TLS identity under `<data_dir>/config/`.
    pub fn open(data_dir: &Path) -> Result<Self> {
        let cfg_dir = data_dir.join("config");
        let cert_path = cfg_dir.join(CERT_FILE);
        let key_path = cfg_dir.join(KEY_FILE);
        if let (Ok(cert), Ok(key)) = (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            if let Ok(tls) = Self::from_der(cert, key) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
                }
                return Ok(tls);
            }
            warn!("phone Wi-Fi TLS: stored certificate/key invalid; regenerating");
        }
        let (cert_der, key_pkcs8) = generate_self_signed_p256()?;
        if let Err(e) = write_private_file(&key_path, &key_pkcs8)
            .and_then(|()| write_private_file(&cert_path, &cert_der))
        {
            warn!(
                "phone Wi-Fi TLS: could not persist TLS identity in {} ({e}); using ephemeral key until restart",
                cfg_dir.display()
            );
        }
        Self::from_der(cert_der, key_pkcs8)
    }

    fn from_der(cert_der: Vec<u8>, key_pkcs8: Vec<u8>) -> Result<Self> {
        let rng = SystemRandom::new();
        // Verify key + cert pair matches before building rustls config.
        let _kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &key_pkcs8, &rng)
            .map_err(|_| anyhow::anyhow!("invalid PKCS#8 ECDSA P-256 key"))?;
        let pin_sha256 = sha256_hex(&cert_der);
        let cert = CertificateDer::from(cert_der.clone());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pkcs8));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let server_cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .context("TLS protocol versions")?
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .context("TLS server certificate")?;
        Ok(Self {
            acceptor: TlsAcceptor::from(Arc::new(server_cfg)),
            pin_sha256,
            cert_der,
        })
    }

    pub fn acceptor(&self) -> TlsAcceptor {
        self.acceptor.clone()
    }

    /// Lowercase 64-character hex SHA-256 digest of the DER leaf certificate.
    pub fn pin_sha256(&self) -> &str {
        &self.pin_sha256
    }

    #[cfg(test)]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }
}

static WIFI_TLS: OnceLock<Option<Arc<WifiTls>>> = OnceLock::new();

/// Initialize the process-wide Wi-Fi TLS identity at server startup.
pub fn init(data_dir: &Path) -> Option<Arc<WifiTls>> {
    WIFI_TLS
        .get_or_init(|| match WifiTls::open(data_dir) {
            Ok(tls) => {
                info!("phone Wi-Fi TLS ready (pin={}…)", &tls.pin_sha256()[..12]);
                Some(Arc::new(tls))
            }
            Err(e) => {
                warn!("phone Wi-Fi TLS unavailable ({e:#}); falling back to plain ws://");
                None
            }
        })
        .clone()
}

pub fn current() -> Option<Arc<WifiTls>> {
    WIFI_TLS.get().and_then(Clone::clone)
}

pub fn cert_pin_sha256() -> Option<String> {
    current().map(|t| t.pin_sha256.clone())
}

/// When `PHONE_WIFI_TLS_ONLY` is `true`/`1`/`yes`/`on`, the dedicated Wi-Fi entry
/// port refuses non-TLS (`ws://`/`http://`) connections and advertises `wss://` only.
pub fn tls_only() -> bool {
    std::env::var("PHONE_WIFI_TLS_ONLY")
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hmac_sha256_hex(key: &str, msg: &str) -> String {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(msg.as_bytes());
    hex_encode(&mac.finalize().into_bytes())
}

/// Constant-time ASCII comparison for hex MACs and digests.
pub fn same_hex(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().as_bytes(), b.trim().as_bytes());
    a.len() == b.len()
        && !a.is_empty()
        && a.iter()
            .zip(b.iter())
            .fold(0u8, |acc, (&x, &y)| acc | (x.to_ascii_lowercase() ^ y.to_ascii_lowercase()))
            == 0
}

/// Client -> server channel-bound pairing MAC:
/// `HMAC-SHA256(code, "bageo-claim-req-v1:<cnonce>:<cert_pin_sha256>")`.
pub fn claim_request_mac(code: &str, cnonce: &str, cert_pin_sha256: &str) -> String {
    hmac_sha256_hex(
        code.trim(),
        &format!("bageo-claim-req-v1:{}:{}", cnonce.trim(), cert_pin_sha256.trim().to_ascii_lowercase()),
    )
}

/// Server -> client channel-bound pairing proof:
/// `HMAC-SHA256(code, "bageo-claim-ack-v1:<cnonce>:<cert_pin_sha256>:<token>")`.
pub fn claim_response_proof(code: &str, cnonce: &str, cert_pin_sha256: &str, token: &str) -> String {
    hmac_sha256_hex(
        code.trim(),
        &format!(
            "bageo-claim-ack-v1:{}:{}:{}",
            cnonce.trim(),
            cert_pin_sha256.trim().to_ascii_lowercase(),
            token.trim()
        ),
    )
}

/// Serve `app` on `listener`, transparently accepting both TLS (`wss://` / `https://`)
/// and — unless `require_tls` is true — plain (`ws://` / `http://`) connections.
pub async fn serve_dual(
    listener: TcpListener,
    app: Router,
    tls: Option<Arc<WifiTls>>,
    require_tls: bool,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(e) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        warn!("accept error: {e}");
                        continue;
                    }
                };
                let local = stream.local_addr().ok();
                let _ = stream.set_nodelay(true);
                let router = app.clone();
                let acceptor = tls.as_ref().map(|t| t.acceptor());
                tokio::spawn(async move {
                    handle_accepted(stream, peer, local, router, acceptor, require_tls).await;
                });
            }
        }
    }
}

async fn handle_accepted(
    stream: TcpStream,
    peer: SocketAddr,
    local: Option<SocketAddr>,
    app: Router,
    acceptor: Option<TlsAcceptor>,
    require_tls: bool,
) {
    let mut first = [0u8; 1];
    let n = match tokio::time::timeout(PEEK_TIMEOUT, stream.peek(&mut first)).await {
        Ok(Ok(n)) => n,
        _ => return,
    };
    if n == 0 {
        return;
    }
    // 0x16 = TLS Handshake record ContentType (TLS 1.2 / TLS 1.3 ClientHello).
    if first[0] == 0x16 {
        let Some(acceptor) = acceptor else {
            return;
        };
        match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
            Ok(Ok(tls_stream)) => serve_io(tls_stream, peer, local, app).await,
            Ok(Err(e)) => {
                tracing::debug!("TLS handshake from {peer} failed: {e}");
            }
            Err(_) => {
                tracing::debug!("TLS handshake from {peer} timed out");
            }
        }
        return;
    }
    if require_tls {
        warn!("refused unencrypted connection from {peer} (PHONE_WIFI_TLS_ONLY is enabled)");
        return;
    }
    serve_io(stream, peer, local, app).await;
}

async fn serve_io<I>(io: I, peer: SocketAddr, local: Option<SocketAddr>, app: Router)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let conn_addrs = crate::lan::ConnAddrs { peer, local };
    let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let mut router = app.clone();
        async move {
            let (mut parts, body) = req.into_parts();
            parts.extensions.insert(ConnectInfo(conn_addrs));
            parts.extensions.insert(ConnectInfo(peer));
            let axum_req = axum::http::Request::from_parts(parts, axum::body::Body::new(body));
            tower_service::Service::call(&mut router, axum_req).await
        }
    });
    let _ = http1::Builder::new()
        .serve_connection(TokioIo::new(io), svc)
        .with_upgrades()
        .await;
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("open {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", path.display()));
    }
    Ok(())
}

// ---- Minimal RFC 5280 X.509 v3 self-signed ECDSA P-256 DER generator ----------------

fn der_len(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else if len <= 0xff {
        vec![0x81, len as u8]
    } else {
        vec![0x82, ((len >> 8) & 0xff) as u8, (len & 0xff) as u8]
    }
}

fn der_tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 3 + content.len());
    out.push(tag);
    out.extend_from_slice(&der_len(content.len()));
    out.extend_from_slice(content);
    out
}

fn der_seq(items: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = items.iter().map(Vec::len).sum();
    let mut body = Vec::with_capacity(total);
    for it in items {
        body.extend_from_slice(it);
    }
    der_tlv(0x30, &body)
}

fn der_bit_string(bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(1 + bytes.len());
    body.push(0x00);
    body.extend_from_slice(bytes);
    der_tlv(0x03, &body)
}

fn generate_self_signed_p256() -> Result<(Vec<u8>, Vec<u8>)> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| anyhow::anyhow!("ECDSA P-256 key generation failed"))?;
    let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| anyhow::anyhow!("ECDSA P-256 key parse failed"))?;

    let mut serial = [0u8; 16];
    if getrandom::getrandom(&mut serial).is_err() {
        serial.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    serial[0] &= 0x7f;
    serial[0] |= 0x01;

    // ecdsa-with-SHA256 (1.2.840.10045.4.3.2), parameters omitted per RFC 5758 §3.2.
    let sig_alg = der_seq(&[der_tlv(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02])]);

    // CN = bageo.local
    let cn_atv = der_seq(&[
        der_tlv(0x06, &[0x55, 0x04, 0x03]),
        der_tlv(0x0c, b"bageo.local"),
    ]);
    let name = der_seq(&[der_tlv(0x31, &cn_atv)]);

    // Validity: 2025-01-01T00:00:00Z .. 2075-01-01T00:00:00Z
    let validity = der_seq(&[
        der_tlv(0x17, b"250101000000Z"),
        der_tlv(0x18, b"20750101000000Z"),
    ]);

    // SubjectPublicKeyInfo: id-ecPublicKey (1.2.840.10045.2.1) + secp256r1 (1.2.840.10045.3.1.7)
    let ec_alg = der_seq(&[
        der_tlv(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 02, 01]),
        der_tlv(0x06, &[0x2a, 0x86, 0x48, 0xce, 0x3d, 03, 01, 07]),
    ]);
    let spki = der_seq(&[ec_alg, der_bit_string(kp.public_key().as_ref())]);

    // X.509 v3 extensions: BasicConstraints(cA=FALSE), KeyUsage(digitalSignature),
    // ExtendedKeyUsage(serverAuth), SubjectAltName(DNS:bageo.local, IP:127.0.0.1).
    let ext_basic = der_seq(&[
        der_tlv(0x06, &[0x55, 0x1d, 0x13]),
        der_tlv(0x01, &[0xff]),
        der_tlv(0x04, &der_seq(&[])),
    ]);
    let ext_ku = der_seq(&[
        der_tlv(0x06, &[0x55, 0x1d, 0x0f]),
        der_tlv(0x01, &[0xff]),
        der_tlv(0x04, &[0x03, 0x02, 0x07, 0x80]),
    ]);
    let ext_eku = der_seq(&[
        der_tlv(0x06, &[0x55, 0x1d, 0x25]),
        der_tlv(
            0x04,
            &der_seq(&[der_tlv(0x06, &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01])]),
        ),
    ]);
    let san_seq = der_seq(&[
        der_tlv(0x82, b"bageo.local"),
        der_tlv(0x87, &[127, 0, 0, 1]),
    ]);
    let ext_san = der_seq(&[
        der_tlv(0x06, &[0x55, 0x1d, 0x11]),
        der_tlv(0x04, &san_seq),
    ]);
    let extensions = der_tlv(0xa3, &der_seq(&[ext_basic, ext_ku, ext_eku, ext_san]));

    let tbs = der_seq(&[
        der_tlv(0xa0, &[0x02, 0x01, 0x02]),
        der_tlv(0x02, &serial),
        sig_alg.clone(),
        name.clone(),
        validity,
        name,
        spki,
        extensions,
    ]);

    let sig = kp
        .sign(&rng, &tbs)
        .map_err(|_| anyhow::anyhow!("ECDSA P-256 certificate signing failed"))?;
    let cert_der = der_seq(&[tbs, sig_alg, der_bit_string(sig.as_ref())]);
    Ok((cert_der, pkcs8.as_ref().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Debug)]
    struct PinVerifier {
        expected_pin: String,
    }

    impl ServerCertVerifier for PinVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            if same_hex(&sha256_hex(end_entity.as_ref()), &self.expected_pin) {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::General("certificate pin mismatch".into()))
            }
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    #[test]
    fn cert_persists_across_restarts_and_claim_mac_binds_to_pin() {
        let dir = std::env::temp_dir().join(format!("bageo-wifi-tls-{}", uuid::Uuid::new_v4()));
        let first = WifiTls::open(&dir).expect("generate WifiTls");
        assert_eq!(first.pin_sha256().len(), 64);
        assert_eq!(sha256_hex(first.cert_der()), first.pin_sha256());

        let second = WifiTls::open(&dir).expect("reload WifiTls");
        assert_eq!(first.pin_sha256(), second.pin_sha256(), "pin survives restart");

        let mac_real = claim_request_mac("654321", "nonce123", first.pin_sha256());
        let mac_mitm = claim_request_mac("654321", "nonce123", &"0".repeat(64));
        assert_ne!(mac_real, mac_mitm, "active MITM cert produces a different MAC");
        let proof = claim_response_proof("654321", "nonce123", first.pin_sha256(), "tok_abc");
        assert!(same_hex(&proof, &claim_response_proof("654321", "nonce123", &first.pin_sha256().to_uppercase(), "tok_abc")));
        assert!(!same_hex(&proof, &claim_response_proof("000000", "nonce123", first.pin_sha256(), "tok_abc")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn serve_dual_handles_pinned_tls_and_plain_http_on_same_port() {
        let dir = std::env::temp_dir().join(format!("bageo-dual-tls-{}", uuid::Uuid::new_v4()));
        let tls = Arc::new(WifiTls::open(&dir).unwrap());
        let pin = tls.pin_sha256().to_string();

        let app = Router::new().route(
            "/ping",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { format!("pong:{}", peer.ip()) }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server_tls = tls.clone();
        let srv = tokio::spawn(async move {
            serve_dual(listener, app, Some(server_tls), false, async {
                let _ = stop_rx.await;
            })
            .await
        });

        // 1. Pinned TLS client succeeds and verifies ECDSA P-256 signature + SHA-256 pin.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let client_cfg = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinVerifier { expected_pin: pin }))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let tcp = TcpStream::connect(addr).await.unwrap();
        let mut tls_stream = connector
            .connect(ServerName::try_from("bageo.local").unwrap(), tcp)
            .await
            .expect("pinned TLS handshake");
        tls_stream
            .write_all(b"GET /ping HTTP/1.1\r\nHost: bageo.local\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = String::new();
        tls_stream.read_to_string(&mut resp).await.unwrap();
        assert!(resp.contains("200 OK") && resp.contains("pong:127.0.0.1"), "{resp}");

        // 2. Wrong pin is rejected during TLS handshake (MITM blocked).
        let bad_cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinVerifier {
                expected_pin: "f".repeat(64),
            }))
            .with_no_client_auth();
        let bad_conn = tokio_rustls::TlsConnector::from(Arc::new(bad_cfg));
        let tcp2 = TcpStream::connect(addr).await.unwrap();
        assert!(
            bad_conn
                .connect(ServerName::try_from("bageo.local").unwrap(), tcp2)
                .await
                .is_err(),
            "wrong certificate pin must fail handshake"
        );

        // 3. Legacy plain HTTP on the same port still works when require_tls is false.
        let mut plain = TcpStream::connect(addr).await.unwrap();
        plain
            .write_all(b"GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut plain_resp = String::new();
        plain.read_to_string(&mut plain_resp).await.unwrap();
        assert!(plain_resp.contains("200 OK") && plain_resp.contains("pong:127.0.0.1"), "{plain_resp}");

        let _ = stop_tx.send(());
        let _ = srv.await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
