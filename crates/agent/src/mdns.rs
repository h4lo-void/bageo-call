// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Answers "<name>.local" on the local network (multicast DNS, RFC 6762), so
//! the web console opens at `http://bageo.local:8787/` without knowing
//! this machine's IP address.
//!
//! Deliberately tiny: one IPv4 address record, no service discovery, no name
//! conflict probing. It shares UDP port 5353 with the system's own responder
//! (Bonjour on macOS, avahi on Linux, the DNS client on Windows) through
//! SO_REUSEADDR / SO_REUSEPORT. When the port still cannot be had it logs one
//! line and stays off; the console address then falls back to the LAN IP (see
//! `settings_store::effective_console_url`).
//!
//! On macOS the name is registered with the system's own Bonjour instead
//! ([`bonjour`]): a program's socket there stops getting the network's
//! multicast queries a minute or so after it starts (it still gets them on
//! Windows and Linux), so answers only reached caches through the start
//! announcement and the name stopped working once they expired.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, info, warn};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_NSEC: u16 = 47;
const TYPE_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const CLASS_ANY: u16 = 255;
/// Top bit of a question's class: the asker wants a unicast reply.
const UNICAST_BIT: u16 = 0x8000;
/// Top bit of a record's class: this is the whole set, drop older ones.
const CACHE_FLUSH_BIT: u16 = 0x8000;
/// How long others may keep the answer (RFC 6762 §10: 120 s for host records).
const TTL_SECS: u32 = 120;
/// One-shot resolvers that do not speak mDNS get at most 10 s (§6.7).
const LEGACY_TTL_SECS: u32 = 10;
/// A query names at most this many questions / known answers we look at.
const MAX_ENTRIES: u16 = 64;
/// Wait between tries when the network is not ready yet.
const RETRY: Duration = Duration::from_secs(30);

/// True while the responder holds UDP 5353 and answers.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether `<name>.local` is being answered right now.
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// The host label as it goes on the network: lower case, letters, digits and
/// "-" only, at most 63 characters, without a trailing ".local".
/// Empty means off.
pub fn sanitize_name(raw: &str) -> String {
    let lower = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = lower.strip_suffix(".local").unwrap_or(&lower);
    let cleaned: String = host
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(63)
        .collect();
    cleaned.trim_matches('-').to_string()
}

/// The network name setting as written (environment, `.env`, web console):
/// `off`, `0`, `none` and `false` (any case) turn it off like an empty value (on
/// Windows an empty variable counts as not set, which would mean the
/// default); anything else goes through [`sanitize_name`].
pub fn parse_name_setting(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "0" | "none" | "false" => String::new(),
        _ => sanitize_name(raw),
    }
}

/// This machine's address on the LAN: the source address the system would use
/// to reach the internet (or, without a default route, the mDNS group).
/// Connecting a UDP socket sends nothing.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    for target in ["8.8.8.8:80", "224.0.0.251:5353"] {
        let Ok(sock) = UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(target).is_err() {
            continue;
        }
        if let Ok(SocketAddr::V4(local)) = sock.local_addr() {
            let ip = *local.ip();
            if !ip.is_unspecified() && !ip.is_loopback() {
                return Some(ip);
            }
        }
    }
    None
}

/// The address to announce for an agent listening on `listen`: that address
/// when it is one specific LAN address, else the detected LAN address.
pub fn answer_ip(listen: IpAddr) -> Option<Ipv4Addr> {
    match listen {
        IpAddr::V4(ip) if !ip.is_unspecified() && !ip.is_loopback() => Some(ip),
        _ => lan_ipv4(),
    }
}

/// Start answering `<name()>.local` in a background thread. `name` is read
/// again every few seconds, so a changed setting applies without a restart;
/// an empty name closes the socket. `port`: the console's port, for the
/// service macOS registers along with the name.
pub fn spawn<F>(listen: IpAddr, port: u16, name: F)
where
    F: Fn() -> String + Send + 'static,
{
    #[cfg(target_os = "macos")]
    let started = std::thread::Builder::new()
        .name("mdns".into())
        .spawn(move || bonjour::run(listen, port, name));
    #[cfg(not(target_os = "macos"))]
    let started = std::thread::Builder::new()
        .name("mdns".into())
        .spawn(move || {
            let _ = port;
            run(listen, name)
        });
    if let Err(e) = started {
        warn!("mDNS: could not start ({e})");
    }
}

/// macOS: the name is registered with the system's Bonjour by a `dns-sd -P`
/// process (an HTTP service on `<name>.local` at this computer's address),
/// started again when the name, the address or the port changes. A small
/// shell around it ends it when Bageo ends, even when Bageo is killed.
#[cfg(target_os = "macos")]
mod bonjour {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    /// `dns-sd` that ends this soon after it started: the name is taken on this network.
    const QUICK_END: Duration = Duration::from_secs(10);
    /// Wait before trying a taken name again.
    const TAKEN_RETRY: Duration = Duration::from_secs(60);

    /// The shell around `dns-sd`: `$1` = Bageo's process id, the rest = the command.
    const WATCH: &str = "P=$1; shift; \"$@\" & C=$!; trap 'kill $C 2>/dev/null; exit 0' TERM INT HUP; \
        while kill -0 \"$P\" 2>/dev/null && kill -0 \"$C\" 2>/dev/null; do sleep 1; done; kill $C 2>/dev/null";

    /// The `dns-sd` arguments for `<host>.local` at `ip`: with the web console
    /// at `port` (`console`), or else with only the phones' Wi-Fi entry there,
    /// which is not a web page.
    pub(super) fn args(host: &str, ip: Ipv4Addr, port: u16, console: bool) -> Vec<String> {
        let mut a = vec!["-P".to_string(), format!("Bageo ({host})")];
        let kind = if console { "_http._tcp" } else { "_bageo-phone._tcp" };
        a.extend([kind.to_string(), "local".into(), port.to_string(), format!("{host}.local"), ip.to_string()]);
        if console {
            a.push("path=/calls/".into());
        }
        a
    }

    fn start(host: &str, ip: Ipv4Addr, port: u16, console: bool) -> std::io::Result<Child> {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(WATCH)
            .arg("sh")
            .arg(std::process::id().to_string())
            .arg("/usr/bin/dns-sd")
            .args(args(host, ip, port, console))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            // The shell and `dns-sd` in a group of their own, ended together by [`stop`].
            .process_group(0)
            .spawn()
    }

    fn stop(child: &mut Child) {
        // The whole group: `dns-sd` too, even when the shell around it is already gone.
        let _ = Command::new("/bin/kill").args(["-TERM", "--", &format!("-{}", child.id())]).status();
        let _ = child.wait();
    }

    pub(super) fn run<F: Fn() -> String>(listen: IpAddr, port: u16, name: F) {
        // Listening on this computer only: the name leads to the phones' Wi-Fi entry.
        let console = !listen.is_loopback();
        let mut running: Option<((String, Ipv4Addr, u16), Child, Instant)> = None;
        // A name another device on the network already answers to, and since when.
        let mut taken: Option<((String, Ipv4Addr, u16), Instant)> = None;
        loop {
            let host = parse_name_setting(&name());
            let shown = if crate::lan::port_80() { 80 } else { port };
            let want = answer_ip(listen).filter(|_| !host.is_empty()).map(|ip| (host.clone(), ip, shown));
            if let Some((had, child, since)) = running.as_mut() {
                let ended = child.try_wait().ok().flatten().is_some();
                if ended || want.as_ref() != Some(had) {
                    if ended && since.elapsed() < QUICK_END {
                        if taken.as_ref().map(|t| &t.0) != Some(&*had) {
                            warn!(
                                "mDNS: another device on this network already answers as {}.local; Bageo tries again every minute (or pick another network name)",
                                had.0
                            );
                        }
                        taken = Some((had.clone(), Instant::now()));
                    } else if ended {
                        warn!("mDNS: Bonjour stopped answering as {}.local; starting it again", had.0);
                    }
                    stop(child);
                    running = None;
                    ACTIVE.store(false, Ordering::Relaxed);
                    if want.is_none() {
                        info!("mDNS: off (the network name setting is empty, or no LAN address)");
                    }
                }
            }
            if running.is_none() {
                if let Some(w) = want {
                    let retry = taken.as_ref().is_some_and(|(t, _)| *t == w);
                    let waiting = taken.as_ref().is_some_and(|(t, at)| *t == w && at.elapsed() < TAKEN_RETRY);
                    if !waiting {
                        match start(&w.0, w.1, w.2, console) {
                            Ok(child) => {
                                // A taken name stays off (the console shows its IP address) until it answers.
                                ACTIVE.store(!retry, Ordering::Relaxed);
                                if !retry {
                                    info!("mDNS: this computer answers as {}.local (through Bonjour)", w.0);
                                }
                                running = Some((w, child, Instant::now()));
                            }
                            Err(e) => warn!("mDNS: could not start dns-sd ({e}); {}.local will not work, open the console by its IP address", w.0),
                        }
                    }
                }
            }
            // A name that has answered for a while is no longer taken.
            if let Some((w, _, since)) = running.as_ref() {
                if since.elapsed() >= QUICK_END && taken.as_ref().is_some_and(|(t, _)| t == w) {
                    taken = None;
                    ACTIVE.store(true, Ordering::Relaxed);
                    info!("mDNS: this computer answers as {}.local (through Bonjour)", w.0);
                }
            }
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn run<F: Fn() -> String>(listen: IpAddr, name: F) {
    let mut sock: Option<UdpSocket> = None;
    let mut buf = vec![0u8; 9000];
    let group = SocketAddr::V4(SocketAddrV4::new(GROUP, PORT));
    let mut failed_before = false;
    loop {
        let host = parse_name_setting(&name());
        if host.is_empty() {
            if sock.take().is_some() {
                ACTIVE.store(false, Ordering::Relaxed);
                info!("mDNS: off (the network name setting is empty)");
            }
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        if sock.is_none() {
            match open(listen) {
                Ok(s) => {
                    ACTIVE.store(true, Ordering::Relaxed);
                    failed_before = false;
                    info!("mDNS: this computer answers as {host}.local");
                    // Announce once so caches that saw an older address update.
                    if let Some(ip) = answer_ip(listen) {
                        let _ = s.send_to(&build_response(&host, ip, None, true), group);
                    }
                    sock = Some(s);
                }
                Err((_, e)) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    warn!(
                        "mDNS: UDP port 5353 is held by a program that does not share it ({e}); {host}.local will not work, open the console by its IP address"
                    );
                    return;
                }
                Err((step, e)) => {
                    // Typically the network is not up yet (started at boot).
                    if failed_before {
                        debug!("mDNS: could not {step} ({e}); trying again in 30 s");
                    } else {
                        warn!("mDNS: could not {step} ({e}); trying again every 30 s");
                    }
                    failed_before = true;
                    std::thread::sleep(RETRY);
                    continue;
                }
            }
        }
        let Some(s) = sock.as_ref() else {
            continue;
        };
        let (n, from) = match s.recv_from(&mut buf) {
            Ok(got) => got,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => {
                debug!("mDNS: receive failed ({e})");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let Some(ask) = match_query(&buf[..n], &host, from.port()) else {
            continue;
        };
        let ours = answer_ip(listen);
        // RFC 6762 §11: answer only this network, never anyone further away.
        if !on_link(from.ip(), ours) {
            debug!("mDNS: ignored a query from {from}, not on this network");
            continue;
        }
        let Some(ip) = ours else {
            continue;
        };
        let reply = build_response(&host, ip, ask.legacy_id, ask.address);
        let to = if ask.unicast { from } else { group };
        if let Err(e) = s.send_to(&reply, to) {
            debug!("mDNS: reply to {to} failed ({e})");
        }
    }
}

/// Whether a query comes from this network (RFC 6762 §11). std cannot list
/// the interfaces and their netmasks, so this is an approximation: private
/// (10/8, 172.16/12, 192.168/16), link-local and loopback addresses, plus the
/// /24 of our own address (for LANs numbered from public ranges). Everything
/// else, e.g. a query arriving from the internet through a forwarded port,
/// is dropped.
fn on_link(src: IpAddr, ours: Option<Ipv4Addr>) -> bool {
    let IpAddr::V4(src) = src else {
        return false;
    };
    src.is_private()
        || src.is_link_local()
        || src.is_loopback()
        || ours.is_some_and(|o| o.octets()[..3] == src.octets()[..3])
}

/// The socket on UDP 5353, shared with the system's responder. The error
/// says which step failed.
fn open(listen: IpAddr) -> Result<UdpSocket, (&'static str, std::io::Error)> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .map_err(|e| ("open a UDP socket", e))?;
    sock.set_reuse_address(true)
        .map_err(|e| ("share UDP port 5353 (SO_REUSEADDR)", e))?;
    // Bonjour on macOS holds the port with SO_REUSEPORT: needed to share it.
    // Same platforms as socket2's own condition for set_reuse_port.
    #[cfg(all(
        unix,
        not(any(
            target_os = "solaris",
            target_os = "illumos",
            target_os = "cygwin",
            target_os = "nuttx",
            target_os = "wasi"
        ))
    ))]
    {
        sock.set_reuse_port(true)
            .map_err(|e| ("share UDP port 5353 (SO_REUSEPORT)", e))?;
    }
    let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT));
    sock.bind(&addr.into()).map_err(|e| ("bind UDP port 5353", e))?;
    let sock = UdpSocket::from(sock);
    let iface = answer_ip(listen).unwrap_or(Ipv4Addr::UNSPECIFIED);
    if sock.join_multicast_v4(&GROUP, &iface).is_err() {
        sock.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)
            .map_err(|e| ("join the mDNS group 224.0.0.251", e))?;
    }
    // RFC 6762 §11: everything we send carries IP TTL 255.
    sock.set_multicast_ttl_v4(255)
        .map_err(|e| ("set the multicast TTL", e))?;
    sock.set_ttl(255).map_err(|e| ("set the TTL", e))?;
    // Wake up now and then to notice a changed name.
    sock.set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| ("set the read timeout", e))?;
    Ok(sock)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Question {
    /// Lower-case dotted name, e.g. "bageo.local".
    name: String,
    qtype: u16,
    qclass: u16,
}

/// How to answer a query that asked for our name.
#[derive(Debug, PartialEq, Eq)]
struct Ask {
    /// Reply straight to the sender instead of the group.
    unicast: bool,
    /// A one-shot resolver (source port other than 5353): echo its id and
    /// question (RFC 6762 §6.7).
    legacy_id: Option<u16>,
    /// It asked for the IPv4 address (A or ANY); false: only for an IPv6
    /// address (AAAA), which this name does not have.
    address: bool,
}

fn be16(buf: &[u8], pos: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*buf.get(pos)?, *buf.get(pos + 1)?]))
}

/// The (possibly compressed) name at `pos`: lower-case dotted text and the
/// offset just after it in the packet.
fn read_name(buf: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut end: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let len = usize::from(*buf.get(pos)?);
        match len & 0xC0 {
            0x00 if len == 0 => return Some((name, end.unwrap_or(pos + 1))),
            0x00 => {
                let label = buf.get(pos + 1..pos + 1 + len)?;
                if !name.is_empty() {
                    name.push('.');
                }
                name.extend(label.iter().map(|b| char::from(b.to_ascii_lowercase())));
                if name.len() > 255 {
                    return None;
                }
                pos += 1 + len;
            }
            0xC0 => {
                let target = ((len & 0x3F) << 8) | usize::from(*buf.get(pos + 1)?);
                if end.is_none() {
                    end = Some(pos + 2);
                }
                jumps += 1;
                if jumps > 16 {
                    return None;
                }
                pos = target;
            }
            _ => return None,
        }
    }
}

/// Header id, flags and the questions of a packet, plus the offset after them.
fn parse_questions(buf: &[u8]) -> Option<(u16, u16, Vec<Question>, usize)> {
    if buf.len() < 12 {
        return None;
    }
    let id = be16(buf, 0)?;
    let flags = be16(buf, 2)?;
    let count = be16(buf, 4)?;
    if count > MAX_ENTRIES {
        return None;
    }
    let mut pos = 12;
    let mut questions = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let (name, next) = read_name(buf, pos)?;
        let qtype = be16(buf, next)?;
        let qclass = be16(buf, next + 2)?;
        pos = next + 4;
        questions.push(Question { name, qtype, qclass });
    }
    Some((id, flags, questions, pos))
}

/// A standard query that asks for the IPv4 address of `<host>.local` (or for
/// any record of it), or for its IPv6 address, which is answered "none"
/// ([`build_response`]). `host` is already sanitized (lower case).
fn match_query(buf: &[u8], host: &str, from_port: u16) -> Option<Ask> {
    let (id, flags, questions, _) = parse_questions(buf)?;
    // Responses (QR bit) and other opcodes are not questions for us.
    if flags & 0x8000 != 0 || flags & 0x7800 != 0 {
        return None;
    }
    let fqdn = format!("{host}.local");
    let mut hit = false;
    let mut unicast = false;
    let mut address = false;
    for q in &questions {
        let class = q.qclass & !UNICAST_BIT;
        if q.name == fqdn
            && matches!(q.qtype, TYPE_A | TYPE_ANY | TYPE_AAAA)
            && matches!(class, CLASS_IN | CLASS_ANY)
        {
            hit = true;
            unicast |= q.qclass & UNICAST_BIT != 0;
            address |= q.qtype != TYPE_AAAA;
        }
    }
    if !hit {
        return None;
    }
    let legacy = from_port != PORT;
    Some(Ask {
        unicast: unicast || legacy,
        legacy_id: legacy.then_some(id),
        address,
    })
}

fn encode_name(host: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(host.len() + 8);
    for label in [host, "local"] {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

/// The NSEC record that says `<host>.local` has an A record and nothing else
/// (RFC 6762 §6.1): next name = the name itself, type bitmap = A only.
fn nsec_record(name: &[u8], class: u16, ttl: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * name.len() + 13);
    out.extend_from_slice(name);
    out.extend_from_slice(&TYPE_NSEC.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&((name.len() + 3) as u16).to_be_bytes());
    out.extend_from_slice(name);
    // Window 0, one bitmap byte, bit 1 (from the top) = type 1 (A).
    out.extend_from_slice(&[0, 1, 0x40]);
    out
}

/// The answer packet. For the IPv4 address (`address`): the A record
/// `<host>.local -> ip`, with the NSEC record (no IPv6 address) as an
/// additional record; for the IPv6 address only: the NSEC record as the
/// answer, so resolvers do not wait for one that never comes. With
/// `legacy_id` it echoes the id and the question and uses a short TTL
/// without the cache-flush bit, as one-shot resolvers expect.
fn build_response(host: &str, ip: Ipv4Addr, legacy_id: Option<u16>, address: bool) -> Vec<u8> {
    let name = encode_name(host);
    let legacy = legacy_id.is_some();
    let mut out = Vec::with_capacity(12 + 5 * name.len() + 40);
    out.extend_from_slice(&legacy_id.unwrap_or(0).to_be_bytes());
    // Response + authoritative answer.
    out.extend_from_slice(&0x8400u16.to_be_bytes());
    let questions: u16 = if legacy { 1 } else { 0 };
    let additional: u16 = if address { 1 } else { 0 };
    for count in [questions, 1, 0, additional] {
        out.extend_from_slice(&count.to_be_bytes());
    }
    if legacy {
        out.extend_from_slice(&name);
        let asked = if address { TYPE_A } else { TYPE_AAAA };
        out.extend_from_slice(&asked.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
    }
    let class = if legacy { CLASS_IN } else { CLASS_IN | CACHE_FLUSH_BIT };
    let ttl = if legacy { LEGACY_TTL_SECS } else { TTL_SECS };
    if address {
        out.extend_from_slice(&name);
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&class.to_be_bytes());
        out.extend_from_slice(&ttl.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ip.octets());
    }
    out.extend_from_slice(&nsec_record(&name, class, ttl));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn bonjour_names_the_web_console_only_when_it_is_on_the_network() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let lan = bonjour::args("bageo", ip, 80, true);
        assert!(lan.iter().any(|a| a == "_http._tcp") && lan.iter().any(|a| a == "path=/calls/"));
        let local = bonjour::args("bageo", ip, 8788, false);
        assert!(!local.iter().any(|a| a == "_http._tcp" || a.starts_with("path=")), "the phones' entry is no web page");
        assert!(local.iter().any(|a| a == "bageo.local") && local.iter().any(|a| a == "192.168.1.20"));
    }

    /// A query the way resolvers send it: header, then the questions.
    fn query(id: u16, flags: u16, questions: &[(&str, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&(questions.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        for (name, qtype, qclass) in questions {
            for label in name.split('.') {
                out.push(label.len() as u8);
                out.extend_from_slice(label.as_bytes());
            }
            out.push(0);
            out.extend_from_slice(&qtype.to_be_bytes());
            out.extend_from_slice(&qclass.to_be_bytes());
        }
        out
    }

    /// Answer records after the questions: (name, type, class, ttl, data).
    fn parse_answers(buf: &[u8]) -> Vec<(String, u16, u16, u32, Vec<u8>)> {
        parse_records(buf, 0)
    }

    /// Additional records after the answers.
    fn parse_additional(buf: &[u8]) -> Vec<(String, u16, u16, u32, Vec<u8>)> {
        parse_records(buf, 1)
    }

    /// The answer (`section` 0) or additional (1) records; no authority records are sent.
    fn parse_records(buf: &[u8], section: usize) -> Vec<(String, u16, u16, u32, Vec<u8>)> {
        let (_, _, _, mut pos) = parse_questions(buf).expect("header and questions");
        assert_eq!(be16(buf, 8).unwrap(), 0, "no authority records");
        let counts = [be16(buf, 6).unwrap(), be16(buf, 10).unwrap()];
        let mut sections = vec![Vec::new(), Vec::new()];
        for (i, count) in counts.iter().enumerate() {
            for _ in 0..*count {
                let (name, next) = read_name(buf, pos).expect("record name");
                let rtype = be16(buf, next).unwrap();
                let class = be16(buf, next + 2).unwrap();
                let ttl = u32::from_be_bytes(buf[next + 4..next + 8].try_into().unwrap());
                let len = usize::from(be16(buf, next + 8).unwrap());
                sections[i].push((name, rtype, class, ttl, buf[next + 10..next + 10 + len].to_vec()));
                pos = next + 10 + len;
            }
        }
        assert_eq!(pos, buf.len(), "nothing after the records");
        sections.swap_remove(section)
    }

    /// The NSEC data for `bageo.local`: the name, then "A only".
    fn nsec_a_only(host: &str) -> Vec<u8> {
        let mut d = encode_name(host);
        d.extend_from_slice(&[0, 1, 0x40]);
        d
    }

    #[test]
    fn answers_a_standard_query_for_its_name() {
        let q = query(0, 0, &[("bageo.local", TYPE_A, CLASS_IN)]);
        assert_eq!(
            match_query(&q, "bageo", PORT),
            Some(Ask { unicast: false, legacy_id: None, address: true })
        );
        // ANY also asks for the address.
        let q = query(0, 0, &[("bageo.local", TYPE_ANY, CLASS_IN)]);
        assert_eq!(match_query(&q, "bageo", PORT).map(|a| a.address), Some(true));
        // A and AAAA in one query: the address, with "no IPv6" beside it.
        let q = query(0, 0, &[("bageo.local", TYPE_AAAA, CLASS_IN), ("bageo.local", TYPE_A, CLASS_IN)]);
        assert_eq!(match_query(&q, "bageo", PORT).map(|a| a.address), Some(true));
    }

    #[test]
    fn an_ipv6_question_gets_nsec_saying_only_a_exists() {
        let q = query(0, 0, &[("bageo.local", TYPE_AAAA, CLASS_IN)]);
        let ask = match_query(&q, "bageo", PORT).expect("answered");
        assert_eq!(ask, Ask { unicast: false, legacy_id: None, address: false });
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let r = build_response("bageo", ip, ask.legacy_id, ask.address);
        assert_eq!(
            parse_answers(&r),
            vec![("bageo.local".to_string(), TYPE_NSEC, CLASS_IN | CACHE_FLUSH_BIT, TTL_SECS, nsec_a_only("bageo"))]
        );
        assert!(parse_additional(&r).is_empty());
        // No IPv6 address is made up anywhere in the packet.
        assert!(!r.windows(2).any(|w| w == TYPE_AAAA.to_be_bytes()), "{r:?}");
        // A one-shot resolver gets its AAAA question back, then the NSEC.
        let q = query(0x4242, 0x0100, &[("bageo.local", TYPE_AAAA, CLASS_IN)]);
        let ask = match_query(&q, "bageo", 50000).expect("answered");
        assert_eq!(ask, Ask { unicast: true, legacy_id: Some(0x4242), address: false });
        let r = build_response("bageo", ip, ask.legacy_id, ask.address);
        let (id, _, questions, _) = parse_questions(&r).unwrap();
        assert_eq!(id, 0x4242);
        assert_eq!(questions, vec![Question { name: "bageo.local".into(), qtype: TYPE_AAAA, qclass: CLASS_IN }]);
        assert_eq!(
            parse_answers(&r),
            vec![("bageo.local".to_string(), TYPE_NSEC, CLASS_IN, LEGACY_TTL_SECS, nsec_a_only("bageo"))]
        );
        // Another name's IPv6 address is not ours to deny.
        assert_eq!(match_query(&query(0, 0, &[("other.local", TYPE_AAAA, CLASS_IN)]), "bageo", PORT), None);
    }

    #[test]
    fn names_match_in_any_case() {
        let q = query(0, 0, &[("BaGeO.LOCAL", TYPE_A, CLASS_IN)]);
        assert!(match_query(&q, "bageo", PORT).is_some());
    }

    #[test]
    fn other_names_types_and_responses_get_no_answer() {
        // IPv6 address, service browsing, another host.
        // (Service browsing and another host; the IPv6 address gets NSEC, see above.)
        assert_eq!(match_query(&query(0, 0, &[("bageo.local", 12, CLASS_IN)]), "bageo", PORT), None);
        assert_eq!(match_query(&query(0, 0, &[("other.local", TYPE_A, CLASS_IN)]), "bageo", PORT), None);
        assert_eq!(match_query(&query(0, 0, &[("bageo.local.example", TYPE_A, CLASS_IN)]), "bageo", PORT), None);
        // Someone else's answer (QR bit set) is not a question.
        assert_eq!(match_query(&query(0, 0x8400, &[("bageo.local", TYPE_A, CLASS_IN)]), "bageo", PORT), None);
    }

    #[test]
    fn unicast_and_one_shot_queries_are_answered_directly() {
        let q = query(0, 0, &[("bageo.local", TYPE_A, CLASS_IN | UNICAST_BIT)]);
        assert_eq!(
            match_query(&q, "bageo", PORT),
            Some(Ask { unicast: true, legacy_id: None, address: true })
        );
        let q = query(0x1234, 0x0100, &[("bageo.local", TYPE_A, CLASS_IN)]);
        assert_eq!(
            match_query(&q, "bageo", 50000),
            Some(Ask { unicast: true, legacy_id: Some(0x1234), address: true })
        );
    }

    #[test]
    fn compressed_names_in_later_questions_are_followed() {
        // Second question: "bageo" + pointer to the "local" label of the first.
        let mut q = query(0, 0, &[("other.local", TYPE_A, CLASS_IN)]);
        q[5] = 2;
        q.extend_from_slice(&[5, b'b', b'a', b'g', b'e', b'o', 0xC0, 12 + 6]);
        q.extend_from_slice(&TYPE_A.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        assert!(match_query(&q, "bageo", PORT).is_some());
    }

    #[test]
    fn broken_packets_are_ignored() {
        let q = query(0, 0, &[("bageo.local", TYPE_A, CLASS_IN)]);
        for cut in 0..q.len() {
            assert_eq!(match_query(&q[..cut], "bageo", PORT), None, "cut at {cut}");
        }
        // A pointer loop.
        let mut looped = query(0, 0, &[]);
        looped[5] = 1;
        looped.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1]);
        assert_eq!(match_query(&looped, "bageo", PORT), None);
    }

    #[test]
    fn the_answer_reads_back() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let r = build_response("bageo", ip, None, true);
        let (id, flags, questions, _) = parse_questions(&r).unwrap();
        assert_eq!((id, flags), (0, 0x8400));
        assert!(questions.is_empty());
        assert_eq!(
            parse_answers(&r),
            vec![("bageo.local".to_string(), TYPE_A, CLASS_IN | CACHE_FLUSH_BIT, TTL_SECS, ip.octets().to_vec())]
        );
        // Beside it: this name has no IPv6 address (RFC 6762 §6.1).
        assert_eq!(
            parse_additional(&r),
            vec![("bageo.local".to_string(), TYPE_NSEC, CLASS_IN | CACHE_FLUSH_BIT, TTL_SECS, nsec_a_only("bageo"))]
        );
        // Our own multicast answer comes back to us: it must not look like a question.
        assert_eq!(match_query(&r, "bageo", PORT), None);
    }

    #[test]
    fn a_one_shot_answer_echoes_id_and_question() {
        let ip = Ipv4Addr::new(10, 0, 0, 7);
        let r = build_response("bageo", ip, Some(0xBEEF), true);
        let (id, _, questions, _) = parse_questions(&r).unwrap();
        assert_eq!(id, 0xBEEF);
        assert_eq!(
            questions,
            vec![Question { name: "bageo.local".into(), qtype: TYPE_A, qclass: CLASS_IN }]
        );
        let answers = parse_answers(&r);
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].2, CLASS_IN);
        assert_eq!(answers[0].3, LEGACY_TTL_SECS);
        assert_eq!(answers[0].4, vec![10, 0, 0, 7]);
    }

    #[test]
    fn names_are_cleaned_for_the_network() {
        assert_eq!(sanitize_name("bageo"), "bageo");
        assert_eq!(sanitize_name(" Bageo "), "bageo");
        assert_eq!(sanitize_name("Bageo.local."), "bageo");
        assert_eq!(sanitize_name("office pc_2"), "officepc2");
        assert_eq!(sanitize_name("-front-desk-"), "front-desk");
        assert_eq!(sanitize_name(""), "");
        assert_eq!(sanitize_name("八哥"), "");
        assert_eq!(sanitize_name(&"a".repeat(80)).len(), 63);
    }

    #[test]
    fn off_words_turn_the_name_off() {
        for off in ["off", "OFF", " None ", "0", "false", "False", ""] {
            assert_eq!(parse_name_setting(off), "", "{off:?}");
        }
        assert_eq!(parse_name_setting("Bageo"), "bageo");
        assert_eq!(parse_name_setting("office"), "office");
    }

    #[test]
    fn only_queries_from_this_network_are_answered() {
        let ours = Some(Ipv4Addr::new(203, 0, 113, 20));
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        assert!(on_link(v4(192, 168, 1, 7), ours));
        assert!(on_link(v4(10, 1, 2, 3), None));
        assert!(on_link(v4(169, 254, 9, 9), None));
        // A LAN numbered from a public range: same /24 as us.
        assert!(on_link(v4(203, 0, 113, 99), ours));
        // Anyone else on the internet.
        assert!(!on_link(v4(8, 8, 8, 8), ours));
        assert!(!on_link(v4(203, 0, 114, 1), ours));
        assert!(!on_link(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), ours));
    }
}

