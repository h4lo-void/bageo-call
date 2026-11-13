//! Answers "<name>.local" on the local network (multicast DNS, RFC 6762), so
//! the web console opens at `http://myna.local:8787/` without knowing
//! this machine's IP address.
//!
//! Deliberately tiny: one IPv4 address record, no service discovery, no name
//! conflict probing. It shares UDP port 5353 with the system's own responder
//! (Bonjour on macOS, avahi on Linux, the DNS client on Windows) through
//! SO_REUSEADDR / SO_REUSEPORT. When the port still cannot be had it logs one
//! line and stays off; the console address then falls back to the LAN IP (see
//! `settings_store::effective_console_url`).

use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tracing::{debug, info, warn};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const TYPE_A: u16 = 1;
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
/// an empty name closes the socket.
pub fn spawn<F>(listen: IpAddr, name: F)
where
    F: Fn() -> String + Send + 'static,
{
    let started = std::thread::Builder::new()
        .name("mdns".into())
        .spawn(move || run(listen, name));
    if let Err(e) = started {
        warn!("mDNS: could not start ({e})");
    }
}

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
                        let _ = s.send_to(&build_response(&host, ip, None), group);
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
        let reply = build_response(&host, ip, ask.legacy_id);
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
    /// Lower-case dotted name, e.g. "myna.local".
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
/// any record of it). `host` is already sanitized (lower case).
fn match_query(buf: &[u8], host: &str, from_port: u16) -> Option<Ask> {
    let (id, flags, questions, _) = parse_questions(buf)?;
    // Responses (QR bit) and other opcodes are not questions for us.
    if flags & 0x8000 != 0 || flags & 0x7800 != 0 {
        return None;
    }
    let fqdn = format!("{host}.local");
    let mut hit = false;
    let mut unicast = false;
    for q in &questions {
        let class = q.qclass & !UNICAST_BIT;
        if q.name == fqdn
            && matches!(q.qtype, TYPE_A | TYPE_ANY)
            && matches!(class, CLASS_IN | CLASS_ANY)
        {
            hit = true;
            unicast |= q.qclass & UNICAST_BIT != 0;
        }
    }
    if !hit {
        return None;
    }
    let legacy = from_port != PORT;
    Some(Ask {
        unicast: unicast || legacy,
        legacy_id: legacy.then_some(id),
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

/// The answer packet: one A record `<host>.local -> ip`. With `legacy_id` it
/// echoes the id and the question and uses a short TTL without the
/// cache-flush bit, as one-shot resolvers expect.
fn build_response(host: &str, ip: Ipv4Addr, legacy_id: Option<u16>) -> Vec<u8> {
    let name = encode_name(host);
    let legacy = legacy_id.is_some();
    let mut out = Vec::with_capacity(12 + 2 * name.len() + 18);
    out.extend_from_slice(&legacy_id.unwrap_or(0).to_be_bytes());
    // Response + authoritative answer.
    out.extend_from_slice(&0x8400u16.to_be_bytes());
    let questions: u16 = if legacy { 1 } else { 0 };
    for count in [questions, 1, 0, 0] {
        out.extend_from_slice(&count.to_be_bytes());
    }
    if legacy {
        out.extend_from_slice(&name);
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
    }
    out.extend_from_slice(&name);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    let class = if legacy { CLASS_IN } else { CLASS_IN | CACHE_FLUSH_BIT };
    out.extend_from_slice(&class.to_be_bytes());
    let ttl = if legacy { LEGACY_TTL_SECS } else { TTL_SECS };
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&ip.octets());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (_, _, _, mut pos) = parse_questions(buf).expect("header and questions");
        let count = be16(buf, 6).unwrap();
        let mut out = Vec::new();
        for _ in 0..count {
            let (name, next) = read_name(buf, pos).expect("answer name");
            let rtype = be16(buf, next).unwrap();
            let class = be16(buf, next + 2).unwrap();
            let ttl = u32::from_be_bytes(buf[next + 4..next + 8].try_into().unwrap());
            let len = usize::from(be16(buf, next + 8).unwrap());
            out.push((name, rtype, class, ttl, buf[next + 10..next + 10 + len].to_vec()));
            pos = next + 10 + len;
        }
        out
    }

    #[test]
    fn answers_a_standard_query_for_its_name() {
        let q = query(0, 0, &[("myna.local", TYPE_A, CLASS_IN)]);
        assert_eq!(
            match_query(&q, "myna", PORT),
            Some(Ask { unicast: false, legacy_id: None })
        );
        // ANY also asks for the address.
        let q = query(0, 0, &[("myna.local", TYPE_ANY, CLASS_IN)]);
        assert!(match_query(&q, "myna", PORT).is_some());
    }

    #[test]
    fn names_match_in_any_case() {
        let q = query(0, 0, &[("MyNa.LOCAL", TYPE_A, CLASS_IN)]);
        assert!(match_query(&q, "myna", PORT).is_some());
    }

    #[test]
    fn other_names_types_and_responses_get_no_answer() {
        // IPv6 address, service browsing, another host.
        assert_eq!(match_query(&query(0, 0, &[("myna.local", 28, CLASS_IN)]), "myna", PORT), None);
        assert_eq!(match_query(&query(0, 0, &[("myna.local", 12, CLASS_IN)]), "myna", PORT), None);
        assert_eq!(match_query(&query(0, 0, &[("other.local", TYPE_A, CLASS_IN)]), "myna", PORT), None);
        assert_eq!(match_query(&query(0, 0, &[("myna.local.example", TYPE_A, CLASS_IN)]), "myna", PORT), None);
        // Someone else's answer (QR bit set) is not a question.
        assert_eq!(match_query(&query(0, 0x8400, &[("myna.local", TYPE_A, CLASS_IN)]), "myna", PORT), None);
    }

    #[test]
    fn unicast_and_one_shot_queries_are_answered_directly() {
        let q = query(0, 0, &[("myna.local", TYPE_A, CLASS_IN | UNICAST_BIT)]);
        assert_eq!(
            match_query(&q, "myna", PORT),
            Some(Ask { unicast: true, legacy_id: None })
        );
        let q = query(0x1234, 0x0100, &[("myna.local", TYPE_A, CLASS_IN)]);
        assert_eq!(
            match_query(&q, "myna", 50000),
            Some(Ask { unicast: true, legacy_id: Some(0x1234) })
        );
    }

    #[test]
    fn compressed_names_in_later_questions_are_followed() {
        // Second question: "myna" + pointer to the "local" label of the first.
        let mut q = query(0, 0, &[("other.local", TYPE_A, CLASS_IN)]);
        q[5] = 2;
        q.extend_from_slice(&[4, b'm', b'y', b'n', b'a', 0xC0, 12 + 6]);
        q.extend_from_slice(&TYPE_A.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        assert!(match_query(&q, "myna", PORT).is_some());
    }

    #[test]
    fn broken_packets_are_ignored() {
        let q = query(0, 0, &[("myna.local", TYPE_A, CLASS_IN)]);
        for cut in 0..q.len() {
            assert_eq!(match_query(&q[..cut], "myna", PORT), None, "cut at {cut}");
        }
        // A pointer loop.
        let mut looped = query(0, 0, &[]);
        looped[5] = 1;
        looped.extend_from_slice(&[0xC0, 12, 0, 1, 0, 1]);
        assert_eq!(match_query(&looped, "myna", PORT), None);
    }

    #[test]
    fn the_answer_reads_back() {
        let ip = Ipv4Addr::new(192, 168, 1, 20);
        let r = build_response("myna", ip, None);
        let (id, flags, questions, _) = parse_questions(&r).unwrap();
        assert_eq!((id, flags), (0, 0x8400));
        assert!(questions.is_empty());
        assert_eq!(
            parse_answers(&r),
            vec![("myna.local".to_string(), TYPE_A, CLASS_IN | CACHE_FLUSH_BIT, TTL_SECS, ip.octets().to_vec())]
        );
        // Our own multicast answer comes back to us: it must not look like a question.
        assert_eq!(match_query(&r, "myna", PORT), None);
    }

    #[test]
    fn a_one_shot_answer_echoes_id_and_question() {
        let ip = Ipv4Addr::new(10, 0, 0, 7);
        let r = build_response("myna", ip, Some(0xBEEF));
        let (id, _, questions, _) = parse_questions(&r).unwrap();
        assert_eq!(id, 0xBEEF);
        assert_eq!(
            questions,
            vec![Question { name: "myna.local".into(), qtype: TYPE_A, qclass: CLASS_IN }]
        );
        let answers = parse_answers(&r);
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].2, CLASS_IN);
        assert_eq!(answers[0].3, LEGACY_TTL_SECS);
        assert_eq!(answers[0].4, vec![10, 0, 0, 7]);
    }

    #[test]
    fn names_are_cleaned_for_the_network() {
        assert_eq!(sanitize_name("myna"), "myna");
        assert_eq!(sanitize_name(" Myna "), "myna");
        assert_eq!(sanitize_name("Myna.local."), "myna");
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
        assert_eq!(parse_name_setting("Myna"), "myna");
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

