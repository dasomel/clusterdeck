#![allow(dead_code)]

use hickory_proto::op::{Edns, Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::A;
use hickory_proto::rr::{DNSClass, RData, Record, RecordType};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime};

use crate::services::local_dns::zone_file::{
    load_and_validate, process_identity, write_status, StatusFile, ZoneTable,
    DEFAULT_LOCAL_DNS_PORT,
};

pub const ANSWER_TTL_SECS: u32 = 30;
const RELOAD_POLL_INTERVAL: Duration = Duration::from_secs(1);
const SOCKET_POLL_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_UDP_PACKET_LEN: usize = 4096;
/// D-fix1: bounded TCP concurrency so slow or idle clients cannot exhaust threads/fds.
const MAX_TCP_CONNECTIONS: usize = 32;
/// D-fix1: whole-message deadline (length prefix + body, and the reply write), not per read, so a
/// slow-drip client cannot hold a worker open by trickling bytes.
const TCP_MESSAGE_DEADLINE: Duration = Duration::from_secs(3);
/// Keep-alive bounds: a connection is closed after this many queries or this much total time,
/// so a client cannot pin a slot by trickling one valid exchange after another.
const TCP_MAX_QUERIES_PER_CONN: usize = 16;
const TCP_MAX_CONN_LIFETIME: Duration = Duration::from_secs(10);
/// RFC 1035 plain-UDP limit, used when the client sent no EDNS OPT.
const UDP_PLAIN_MAX_LEN: usize = 512;
/// DNS flag-day recommendation: never advertise or honour more than this over UDP.
const UDP_EDNS_MAX_LEN: usize = 1232;

#[derive(Debug, Clone, Copy)]
struct TcpLimits {
    max_connections: usize,
    message_deadline: Duration,
    max_queries: usize,
    max_lifetime: Duration,
}

impl Default for TcpLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_TCP_CONNECTIONS,
            message_deadline: TCP_MESSAGE_DEADLINE,
            max_queries: TCP_MAX_QUERIES_PER_CONN,
            max_lifetime: TCP_MAX_CONN_LIFETIME,
        }
    }
}

/// Releases one TCP connection slot on drop, including when the worker thread panics or fails
/// to spawn (the closure owning the guard is dropped in that case).
struct ConnGuard(Arc<AtomicUsize>);

impl ConnGuard {
    fn acquire(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        if counter.fetch_add(1, Ordering::SeqCst) >= max {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(Arc::clone(counter)))
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Pure authoritative DNS response builder implementing ADR-0011 D5.
///
/// Returns `None` when `query` cannot be decoded or is not a standard single-question query.
pub fn respond(query: &[u8], table: &ZoneTable) -> Option<Vec<u8>> {
    let req = Message::from_vec(query).ok()?;
    if req.metadata.message_type != MessageType::Query
        || req.metadata.op_code != OpCode::Query
        || req.queries.len() != 1
    {
        return None;
    }

    let question = &req.queries[0];
    let mut resp = Message::response(req.metadata.id, OpCode::Query);
    resp.metadata.recursion_desired = req.metadata.recursion_desired;
    // D5: the local responder is strictly authoritative and never recursive.
    resp.metadata.recursion_available = false;
    resp.add_query(question.clone());

    if let Some(req_edns) = &req.edns {
        let mut edns = Edns::new();
        edns.set_version(0);
        edns.set_max_payload(req_edns.max_payload());
        resp.set_edns(edns);
    }

    let qname = question
        .name
        .to_ascii()
        .trim_end_matches('.')
        .to_ascii_lowercase();

    // D5: anything outside active zones (including `_dns.resolver.arpa` DDR probes) is REFUSED.
    if !table.is_in_zone(&qname) {
        resp.metadata.authoritative = false;
        resp.metadata.response_code = ResponseCode::Refused;
        return resp.to_vec().ok();
    }

    resp.metadata.authoritative = true;
    match table.lookup_ipv4(&qname) {
        Some(ips) => {
            resp.metadata.response_code = ResponseCode::NoError;
            if question.query_type == RecordType::A
                && matches!(question.query_class, DNSClass::IN | DNSClass::ANY)
            {
                for &ip in ips {
                    resp.add_answer(Record::from_rdata(
                        question.name.clone(),
                        ANSWER_TTL_SECS,
                        RData::A(A(ip)),
                    ));
                }
            }
        }
        None => {
            resp.metadata.response_code = ResponseCode::NXDomain;
        }
    }

    resp.to_vec().ok()
}

/// UDP flavour of [`respond`]: enforces the client's size limit (512 bytes, or the EDNS payload
/// size capped at 1232). An oversized answer is replaced by header + question with TC=1 and no
/// answers so the client retries over TCP, which always gets the full response.
pub fn respond_udp(query: &[u8], table: &ZoneTable) -> Option<Vec<u8>> {
    let full = respond(query, table)?;
    let limit = udp_response_limit(query);
    if full.len() <= limit {
        return Some(full);
    }
    let mut truncated = Message::from_vec(&full).ok()?;
    truncated.answers.clear();
    truncated.metadata.truncation = true;
    truncated.to_vec().ok()
}

fn udp_response_limit(query: &[u8]) -> usize {
    Message::from_vec(query)
        .ok()
        .and_then(|req| req.edns.map(|e| usize::from(e.max_payload())))
        .map_or(UDP_PLAIN_MAX_LEN, |n| {
            n.clamp(UDP_PLAIN_MAX_LEN, UDP_EDNS_MAX_LEN)
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponderCliArgs {
    pub zones_path: PathBuf,
    pub status_path: PathBuf,
    pub port: u16,
}

/// Parses `--dns-responder --zones <path> --status <path> --port <u16>` when `--dns-responder`
/// is present in `args`. Returns `None` when the process was not invoked in responder mode.
pub fn parse_cli_args<I, S>(args: I) -> Option<Result<ResponderCliArgs, String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let collected: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if !collected.iter().any(|a| a == "--dns-responder") {
        return None;
    }

    let mut zones_path: Option<PathBuf> = None;
    let mut status_path: Option<PathBuf> = None;
    let mut port: u16 = DEFAULT_LOCAL_DNS_PORT;

    let mut idx = 0;
    while idx < collected.len() {
        match collected[idx].as_str() {
            "--dns-responder" => {
                idx += 1;
            }
            "--zones" => {
                let val = collected.get(idx + 1).ok_or_else(|| {
                    "missing value for --zones (expected path to zones.json)".to_string()
                });
                match val {
                    Ok(v) if !v.is_empty() && !v.starts_with("--") => {
                        zones_path = Some(PathBuf::from(v));
                        idx += 2;
                    }
                    _ => return Some(Err("missing value for --zones".to_string())),
                }
            }
            "--status" => {
                let val = collected.get(idx + 1).ok_or_else(|| {
                    "missing value for --status (expected path to status.json)".to_string()
                });
                match val {
                    Ok(v) if !v.is_empty() && !v.starts_with("--") => {
                        status_path = Some(PathBuf::from(v));
                        idx += 2;
                    }
                    _ => return Some(Err("missing value for --status".to_string())),
                }
            }
            "--port" => {
                let Some(val) = collected.get(idx + 1) else {
                    return Some(Err("missing value for --port".to_string()));
                };
                match val.parse::<u16>() {
                    Ok(p) => {
                        port = p;
                        idx += 2;
                    }
                    Err(_) => return Some(Err(format!("invalid --port value: {val}"))),
                }
            }
            other => {
                return Some(Err(format!(
                    "unrecognized --dns-responder argument: {other}"
                )));
            }
        }
    }

    let Some(zones_path) = zones_path else {
        return Some(Err("--dns-responder requires --zones <path>".to_string()));
    };
    let status_path = status_path.unwrap_or_else(|| match zones_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.join("status.json"),
        _ => PathBuf::from("status.json"),
    });

    Some(Ok(ResponderCliArgs {
        zones_path,
        status_path,
        port,
    }))
}

/// Runs the authoritative loopback DNS responder until the process exits or an unrecoverable
/// startup bind error occurs.
pub fn run(
    zones_path: impl AsRef<Path>,
    status_path: impl AsRef<Path>,
    port: u16,
) -> Result<(), String> {
    let handle = start_responder(
        zones_path.as_ref().to_path_buf(),
        status_path.as_ref().to_path_buf(),
        port,
        RELOAD_POLL_INTERVAL,
    )?;
    handle.wait()
}

pub struct ResponderHandle {
    bound_port: u16,
    stop: Arc<AtomicBool>,
    udp_thread: Option<std::thread::JoinHandle<()>>,
    tcp_thread: Option<std::thread::JoinHandle<()>>,
    reload_thread: Option<std::thread::JoinHandle<()>>,
}

impl ResponderHandle {
    pub fn port(&self) -> u16 {
        self.bound_port
    }

    pub fn wait(mut self) -> Result<(), String> {
        if let Some(reload) = self.reload_thread.take() {
            reload
                .join()
                .map_err(|_| "responder reload thread panicked".to_string())?;
        }
        Ok(())
    }

    pub fn shutdown(mut self) {
        self.stop_all();
    }

    fn stop_all(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.reload_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.udp_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.tcp_thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ResponderHandle {
    fn drop(&mut self) {
        self.stop_all();
    }
}

pub fn start_responder(
    zones_path: PathBuf,
    status_path: PathBuf,
    port: u16,
    poll_interval: Duration,
) -> Result<ResponderHandle, String> {
    start_responder_with_limits(
        zones_path,
        status_path,
        port,
        poll_interval,
        TcpLimits::default(),
    )
}

fn start_responder_with_limits(
    zones_path: PathBuf,
    status_path: PathBuf,
    port: u16,
    poll_interval: Duration,
    tcp_limits: TcpLimits,
) -> Result<ResponderHandle, String> {
    // D2: bind 127.0.0.1:<port> only; never silently switch ports on EADDRINUSE.
    let (udp_socket, tcp_listener, bound_port) = bind_loopback_sockets(port)?;
    udp_socket
        .set_read_timeout(Some(SOCKET_POLL_TIMEOUT))
        .map_err(|e| format!("failed to configure UDP socket timeout: {e}"))?;
    tcp_listener
        .set_nonblocking(true)
        .map_err(|e| format!("failed to configure TCP listener nonblocking mode: {e}"))?;

    let table = Arc::new(RwLock::new(ZoneTable::default()));
    let mut active_generation: u64 = 0;
    let mut active_loaded_at = String::new();
    let last_stamp = file_stamp(&zones_path);
    let mut status_gate = StatusLogGate::default();
    let pending_status = apply_load_attempt(
        &zones_path,
        &status_path,
        &table,
        &mut active_generation,
        &mut active_loaded_at,
        &mut status_gate,
    );

    let stop = Arc::new(AtomicBool::new(false));

    let udp_table = Arc::clone(&table);
    let udp_stop = Arc::clone(&stop);
    let udp_thread = std::thread::Builder::new()
        .name("clusterdeck-dns-udp".into())
        .spawn(move || serve_udp(udp_socket, udp_table, udp_stop))
        .map_err(|e| format!("failed to spawn UDP DNS thread: {e}"))?;

    let tcp_table = Arc::clone(&table);
    let tcp_stop = Arc::clone(&stop);
    let tcp_thread = std::thread::Builder::new()
        .name("clusterdeck-dns-tcp".into())
        .spawn(move || serve_tcp(tcp_listener, tcp_table, tcp_stop, tcp_limits))
        .map_err(|e| format!("failed to spawn TCP DNS thread: {e}"))?;

    let reload_table = Arc::clone(&table);
    let reload_stop = Arc::clone(&stop);
    let reload_thread = std::thread::Builder::new()
        .name("clusterdeck-dns-reload".into())
        .spawn(move || {
            poll_reload_loop(
                zones_path,
                status_path,
                reload_table,
                reload_stop,
                poll_interval,
                last_stamp,
                active_generation,
                active_loaded_at,
                pending_status,
                status_gate,
            );
        })
        .map_err(|e| format!("failed to spawn DNS reload thread: {e}"))?;

    Ok(ResponderHandle {
        bound_port,
        stop,
        udp_thread: Some(udp_thread),
        tcp_thread: Some(tcp_thread),
        reload_thread: Some(reload_thread),
    })
}

fn bind_loopback_sockets(port: u16) -> Result<(UdpSocket, TcpListener, u16), String> {
    if port != 0 {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let udp = UdpSocket::bind(addr)
            .map_err(|e| format!("failed to bind UDP 127.0.0.1:{port}: {e}"))?;
        let tcp = TcpListener::bind(addr)
            .map_err(|e| format!("failed to bind TCP 127.0.0.1:{port}: {e}"))?;
        return Ok((udp, tcp, port));
    }

    // Ephemeral port 0 (used by socket tests): bind UDP on port 0, then bind TCP on the same
    // assigned port so both protocols share one port.
    let mut last_err = String::new();
    for _ in 0..8 {
        let udp = UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .map_err(|e| format!("failed to bind ephemeral UDP socket: {e}"))?;
        let assigned = udp
            .local_addr()
            .map_err(|e| format!("failed to inspect local UDP address: {e}"))?
            .port();
        match TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], assigned))) {
            Ok(tcp) => return Ok((udp, tcp, assigned)),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(format!(
        "failed to bind matching TCP ephemeral port on 127.0.0.1: {last_err}"
    ))
}

fn serve_udp(socket: UdpSocket, table: Arc<RwLock<ZoneTable>>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; MAX_UDP_PACKET_LEN];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buf) {
            Ok((len, peer)) => {
                let response = {
                    let guard = table.read().unwrap_or_else(|e| e.into_inner());
                    respond_udp(&buf[..len], &guard)
                };
                if let Some(bytes) = response {
                    let _ = socket.send_to(&bytes, peer);
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }
}

fn serve_tcp(
    listener: TcpListener,
    table: Arc<RwLock<ZoneTable>>,
    stop: Arc<AtomicBool>,
    limits: TcpLimits,
) {
    let active = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // Over the cap: drop the stream (closes it) rather than queue unbounded work.
                let Some(guard) = ConnGuard::acquire(&active, limits.max_connections) else {
                    continue;
                };
                let conn_table = Arc::clone(&table);
                let spawned = std::thread::Builder::new()
                    .name("clusterdeck-dns-tcp-conn".into())
                    .spawn(move || {
                        let _guard = guard;
                        handle_tcp_stream(stream, conn_table, limits);
                    });
                if let Err(err) = spawned {
                    // The closure (stream + guard) is dropped on failure; keep accepting.
                    eprintln!("clusterdeck dns: failed to spawn TCP worker: {err}");
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(SOCKET_POLL_TIMEOUT);
            }
            Err(_) => {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(SOCKET_POLL_TIMEOUT);
            }
        }
    }
}

/// Reads exactly `buf.len()` bytes before `deadline`; the socket timeout is re-armed with the
/// remaining time on every read so the limit covers the whole message, not each read.
fn read_exact_by(stream: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        stream.set_read_timeout(Some(remaining))?;
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes all of `buf` before `deadline`; the write timeout is re-armed with the remaining time
/// before every partial write so a slow reader cannot extend the limit.
fn write_all_by(stream: &mut TcpStream, buf: &[u8], deadline: Instant) -> std::io::Result<()> {
    let mut written = 0;
    while written < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(remaining))?;
        match stream.write(&buf[written..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => written += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn handle_tcp_stream(mut stream: TcpStream, table: Arc<RwLock<ZoneTable>>, limits: TcpLimits) {
    let _ = stream.set_nonblocking(false);

    // Hard cap on the whole connection (reads, writes and keep-alive gaps).
    let conn_deadline = Instant::now() + limits.max_lifetime;
    // The first message's clock starts at accept; later ones when the previous reply is sent.
    let message_deadline = |now: Instant| (now + limits.message_deadline).min(conn_deadline);
    let mut deadline = message_deadline(Instant::now());
    let mut len_buf = [0u8; 2];
    for _ in 0..limits.max_queries {
        if read_exact_by(&mut stream, &mut len_buf, deadline).is_err() {
            break;
        }
        let msg_len = u16::from_be_bytes(len_buf) as usize;
        if msg_len == 0 {
            break;
        }
        let mut query_buf = vec![0u8; msg_len];
        if read_exact_by(&mut stream, &mut query_buf, deadline).is_err() {
            break;
        }
        let response = {
            let guard = table.read().unwrap_or_else(|e| e.into_inner());
            respond(&query_buf, &guard)
        };
        let Some(resp_bytes) = response else {
            break;
        };
        let Ok(resp_len) = u16::try_from(resp_bytes.len()) else {
            break;
        };
        let mut wire = Vec::with_capacity(2 + resp_bytes.len());
        wire.extend_from_slice(&resp_len.to_be_bytes());
        wire.extend_from_slice(&resp_bytes);
        // Writes share the message deadline (starting from when the query was read).
        if write_all_by(&mut stream, &wire, deadline).is_err() {
            break;
        }
        deadline = message_deadline(Instant::now());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
    digest: [u8; 32],
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let bytes = std::fs::read(path).ok()?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    Some(FileStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
        digest,
    })
}

/// Applies one load attempt. Returns the status that could not be written to `status.json`
/// (`None` on success) so the caller can keep it dirty and retry on every poll tick.
fn apply_load_attempt(
    zones_path: &Path,
    status_path: &Path,
    table: &Arc<RwLock<ZoneTable>>,
    active_generation: &mut u64,
    active_loaded_at: &mut String,
    gate: &mut StatusLogGate,
) -> Option<StatusFile> {
    let (pid, instance) = process_identity();
    match load_and_validate(zones_path) {
        Ok(new_table) => {
            let gen = new_table.generation;
            let now = chrono::Utc::now().to_rfc3339();
            let mut guard = table.write().unwrap_or_else(|e| e.into_inner());
            *guard = new_table;
            drop(guard);

            *active_generation = gen;
            *active_loaded_at = now.clone();
            write_status_reporting(
                status_path,
                StatusFile {
                    generation: gen,
                    loaded_at: now,
                    error: None,
                    pid,
                    instance,
                },
                gate,
            )
        }
        Err(err) => {
            // R6: preserve the previous table in memory and record the validation/load error in
            // status.json against the currently loaded generation.
            if active_loaded_at.is_empty() {
                *active_loaded_at = chrono::Utc::now().to_rfc3339();
            }
            write_status_reporting(
                status_path,
                StatusFile {
                    generation: *active_generation,
                    loaded_at: active_loaded_at.clone(),
                    error: Some(err),
                    pid,
                    instance,
                },
                gate,
            )
        }
    }
}

/// Decides when status.json write results are worth logging: a retry loop that fails with the
/// same error every tick must not emit ~86k identical lines a day.
#[derive(Debug, Default)]
struct StatusLogGate {
    last_error: Option<String>,
}

impl StatusLogGate {
    /// True when `err` differs from the previously logged failure (or there was none).
    fn should_log_failure(&mut self, err: &str) -> bool {
        if self.last_error.as_deref() == Some(err) {
            return false;
        }
        self.last_error = Some(err.to_string());
        true
    }

    /// True for the first success after a logged failure.
    fn should_log_recovery(&mut self) -> bool {
        self.last_error.take().is_some()
    }
}

/// Writes status.json; failures go to stderr (deduplicated by `gate`) and the unwritten status
/// is handed back for retry.
fn write_status_reporting(
    status_path: &Path,
    status: StatusFile,
    gate: &mut StatusLogGate,
) -> Option<StatusFile> {
    match write_status(status_path, status.clone()) {
        Ok(()) => {
            if gate.should_log_recovery() {
                eprintln!(
                    "clusterdeck dns: wrote {} successfully after earlier failures",
                    status_path.display()
                );
            }
            None
        }
        Err(err) => {
            if gate.should_log_failure(&err) {
                eprintln!(
                    "clusterdeck dns: failed to write {}: {err} (retrying every poll tick)",
                    status_path.display()
                );
            }
            Some(status)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn poll_reload_loop(
    zones_path: PathBuf,
    status_path: PathBuf,
    table: Arc<RwLock<ZoneTable>>,
    stop: Arc<AtomicBool>,
    poll_interval: Duration,
    mut last_stamp: Option<FileStamp>,
    mut active_generation: u64,
    mut active_loaded_at: String,
    mut pending_status: Option<StatusFile>,
    mut gate: StatusLogGate,
) {
    while !stop.load(Ordering::Relaxed) {
        let start = Instant::now();
        while start.elapsed() < poll_interval {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let remaining = poll_interval.saturating_sub(start.elapsed());
            std::thread::sleep(remaining.min(SOCKET_POLL_TIMEOUT));
        }

        let current_stamp = file_stamp(&zones_path);
        if current_stamp != last_stamp {
            last_stamp = current_stamp;
            pending_status = apply_load_attempt(
                &zones_path,
                &status_path,
                &table,
                &mut active_generation,
                &mut active_loaded_at,
                &mut gate,
            );
        } else if let Some(status) = pending_status.take() {
            // Status write failed earlier: retry the latest status each tick until it lands,
            // even though zones.json is unchanged.
            pending_status = write_status_reporting(&status_path, status, &mut gate);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::local_dns::zone_file::{
        load_status, write_zones, RecordEntry, ZoneEntry, ZonesFile, DEFAULT_LOCAL_DNS_PORT,
    };
    use hickory_proto::op::Query;
    use hickory_proto::rr::Name;
    use std::net::Ipv4Addr;
    use std::sync::atomic::AtomicU64;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "clusterdeck-responder-test-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_zones_file(generation: u64, ipv4: &str) -> ZonesFile {
        ZonesFile {
            version: 1,
            generation,
            port: DEFAULT_LOCAL_DNS_PORT,
            zones: vec![ZoneEntry {
                zone: "lab.example.internal".into(),
                profile_id: "cka-lab".into(),
                records: vec![RecordEntry {
                    name: "api.lab.example.internal".into(),
                    ipv4: ipv4.into(),
                }],
            }],
        }
    }

    fn sample_table() -> ZoneTable {
        ZoneTable::from_zones_file(&sample_zones_file(1, "192.0.2.10")).unwrap()
    }

    fn build_query_bytes(id: u16, qname: &str, qtype: RecordType, with_edns: bool) -> Vec<u8> {
        let mut msg = Message::new(id, MessageType::Query, OpCode::Query);
        msg.metadata.recursion_desired = true;
        let fqdn = if qname.ends_with('.') {
            qname.to_string()
        } else {
            format!("{qname}.")
        };
        msg.add_query(Query::query(Name::from_ascii(&fqdn).unwrap(), qtype));
        if with_edns {
            let mut edns = Edns::new();
            edns.set_version(0);
            edns.set_max_payload(1232);
            msg.set_edns(edns);
        }
        msg.to_vec().unwrap()
    }

    #[test]
    fn respond_serves_a_record_with_ttl_30_aa_and_no_ra() {
        let table = sample_table();
        let raw_q = build_query_bytes(0x1234, "api.lab.example.internal", RecordType::A, false);
        let raw_resp = respond(&raw_q, &table).expect("expected response");
        let resp = Message::from_vec(&raw_resp).unwrap();

        assert_eq!(resp.metadata.id, 0x1234);
        assert_eq!(resp.metadata.message_type, MessageType::Response);
        assert_eq!(resp.metadata.response_code, ResponseCode::NoError);
        assert!(resp.metadata.authoritative);
        assert!(!resp.metadata.recursion_available);
        assert_eq!(resp.queries.len(), 1);
        assert_eq!(
            resp.queries[0].name,
            Name::from_ascii("api.lab.example.internal.").unwrap()
        );
        assert_eq!(resp.answers.len(), 1);
        assert_eq!(resp.answers[0].ttl, 30);
        assert_eq!(
            resp.answers[0].data,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 10)))
        );
        assert!(resp.edns.is_none());
    }

    #[test]
    fn respond_returns_noerror_nodata_for_other_qtypes_on_served_name() {
        let table = sample_table();
        for qtype in [
            RecordType::AAAA,
            RecordType::HTTPS,
            RecordType::SVCB,
            RecordType::TXT,
        ] {
            let raw_q = build_query_bytes(0x2222, "api.lab.example.internal", qtype, false);
            let raw_resp = respond(&raw_q, &table).expect("expected NODATA response");
            let resp = Message::from_vec(&raw_resp).unwrap();

            assert_eq!(resp.metadata.response_code, ResponseCode::NoError);
            assert!(resp.metadata.authoritative);
            assert!(!resp.metadata.recursion_available);
            assert!(
                resp.answers.is_empty(),
                "expected empty answers for {qtype:?}"
            );
        }
    }

    #[test]
    fn respond_returns_authoritative_nxdomain_for_unknown_name_in_zone() {
        let table = sample_table();
        let raw_q = build_query_bytes(0x3333, "missing.lab.example.internal", RecordType::A, false);
        let raw_resp = respond(&raw_q, &table).expect("expected NXDOMAIN response");
        let resp = Message::from_vec(&raw_resp).unwrap();

        assert_eq!(resp.metadata.id, 0x3333);
        assert_eq!(resp.metadata.response_code, ResponseCode::NXDomain);
        assert!(resp.metadata.authoritative);
        assert!(!resp.metadata.recursion_available);
        assert!(resp.answers.is_empty());
    }

    #[test]
    fn respond_returns_refused_outside_zones_including_ddr_resolver_arpa() {
        let table = sample_table();
        for (name, qtype) in [
            ("_dns.resolver.arpa", RecordType::SVCB),
            ("example.com", RecordType::A),
            ("other.example.internal", RecordType::A),
        ] {
            let raw_q = build_query_bytes(0x4444, name, qtype, false);
            let raw_resp = respond(&raw_q, &table).expect("expected REFUSED response");
            let resp = Message::from_vec(&raw_resp).unwrap();

            assert_eq!(
                resp.metadata.response_code,
                ResponseCode::Refused,
                "expected REFUSED for {name}"
            );
            assert!(!resp.metadata.authoritative);
            assert!(!resp.metadata.recursion_available);
            assert!(resp.answers.is_empty());
        }
    }

    #[test]
    fn respond_matches_case_insensitively_and_echoes_edns_opt() {
        let table = sample_table();
        let raw_q = build_query_bytes(0x5555, "ApI.LaB.ExAmPlE.InTeRnAl", RecordType::A, true);
        let raw_resp = respond(&raw_q, &table).expect("expected case-insensitive response");
        let resp = Message::from_vec(&raw_resp).unwrap();

        assert_eq!(resp.metadata.id, 0x5555);
        assert_eq!(resp.metadata.response_code, ResponseCode::NoError);
        assert!(resp.metadata.authoritative);
        assert!(!resp.metadata.recursion_available);
        assert_eq!(resp.answers.len(), 1);
        assert_eq!(
            resp.answers[0].data,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 10)))
        );
        let edns = resp.edns.expect("expected EDNS OPT record in response");
        assert_eq!(edns.version(), 0);
        assert_eq!(edns.max_payload(), 1232);
    }

    #[test]
    fn respond_returns_none_for_malformed_or_non_query_input() {
        let table = sample_table();
        assert!(respond(&[], &table).is_none());
        assert!(respond(&[0x00, 0x01, 0x02], &table).is_none());

        let mut response_msg = Message::response(0x7777, OpCode::Query);
        response_msg.add_query(Query::query(
            Name::from_ascii("api.lab.example.internal.").unwrap(),
            RecordType::A,
        ));
        assert!(respond(&response_msg.to_vec().unwrap(), &table).is_none());

        let empty_query = Message::new(0x8888, MessageType::Query, OpCode::Query);
        assert!(respond(&empty_query.to_vec().unwrap(), &table).is_none());
    }

    #[test]
    fn real_socket_serves_udp_and_tcp_and_rejects_port_in_use() {
        let dir = temp_dir("real-socket");
        let zones_path = dir.join("zones.json");
        let status_path = dir.join("status.json");
        write_zones(&zones_path, &sample_zones_file(1, "192.0.2.10")).unwrap();

        let handle = start_responder(
            zones_path.clone(),
            status_path.clone(),
            0,
            Duration::from_millis(100),
        )
        .expect("responder should bind ephemeral port");
        let port = handle.port();
        assert!(port > 0);

        // EADDRINUSE check: attempting to run a second responder on the same port must fail
        // immediately rather than picking a different port.
        let conflict_err = run(&zones_path, dir.join("status2.json"), port)
            .expect_err("second bind on same port must fail");
        assert!(conflict_err.contains(&port.to_string()));

        // UDP query
        let udp_client = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp_client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let udp_q = build_query_bytes(0x9001, "api.lab.example.internal", RecordType::A, true);
        udp_client
            .send_to(&udp_q, SocketAddr::from(([127, 0, 0, 1], port)))
            .unwrap();
        let mut udp_buf = [0u8; 1024];
        let (udp_len, _) = udp_client.recv_from(&mut udp_buf).unwrap();
        let udp_resp = Message::from_vec(&udp_buf[..udp_len]).unwrap();
        assert_eq!(udp_resp.metadata.id, 0x9001);
        assert_eq!(udp_resp.metadata.response_code, ResponseCode::NoError);
        assert_eq!(udp_resp.answers.len(), 1);
        assert_eq!(
            udp_resp.answers[0].data,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 10)))
        );
        assert!(udp_resp.edns.is_some());

        // TCP query (2-byte big-endian length prefix)
        let mut tcp_client = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
        tcp_client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let tcp_q = build_query_bytes(0x9002, "api.lab.example.internal", RecordType::A, false);
        let q_len = u16::try_from(tcp_q.len()).unwrap();
        tcp_client.write_all(&q_len.to_be_bytes()).unwrap();
        tcp_client.write_all(&tcp_q).unwrap();
        tcp_client.flush().unwrap();

        let mut resp_len_buf = [0u8; 2];
        tcp_client.read_exact(&mut resp_len_buf).unwrap();
        let resp_len = u16::from_be_bytes(resp_len_buf) as usize;
        let mut tcp_resp_buf = vec![0u8; resp_len];
        tcp_client.read_exact(&mut tcp_resp_buf).unwrap();
        let tcp_resp = Message::from_vec(&tcp_resp_buf).unwrap();
        assert_eq!(tcp_resp.metadata.id, 0x9002);
        assert_eq!(tcp_resp.metadata.response_code, ResponseCode::NoError);
        assert_eq!(
            tcp_resp.answers[0].data,
            RData::A(A(Ipv4Addr::new(192, 0, 2, 10)))
        );

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_keeps_previous_table_on_invalid_file_then_swaps_on_next_valid_file() {
        let dir = temp_dir("reload");
        let zones_path = dir.join("zones.json");
        let status_path = dir.join("status.json");

        // 1. Start with valid generation 1 -> 192.0.2.10
        write_zones(&zones_path, &sample_zones_file(1, "192.0.2.10")).unwrap();
        let handle = start_responder(
            zones_path.clone(),
            status_path.clone(),
            0,
            Duration::from_millis(50),
        )
        .unwrap();
        let port = handle.port();

        let status1 = load_status(&status_path).unwrap();
        assert_eq!(status1.generation, 1);
        assert!(status1.error.is_none());
        let (self_pid, self_instance) = process_identity();
        assert_eq!(status1.pid, self_pid);
        assert_eq!(status1.instance, self_instance);

        let query_ip = |id: u16| -> Ipv4Addr {
            let client = UdpSocket::bind("127.0.0.1:0").unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let q = build_query_bytes(id, "api.lab.example.internal", RecordType::A, false);
            client
                .send_to(&q, SocketAddr::from(([127, 0, 0, 1], port)))
                .unwrap();
            let mut buf = [0u8; 1024];
            let (len, _) = client.recv_from(&mut buf).unwrap();
            let msg = Message::from_vec(&buf[..len]).unwrap();
            match &msg.answers[0].data {
                RData::A(A(ip)) => *ip,
                other => panic!("expected A record, got {other:?}"),
            }
        };

        assert_eq!(query_ip(1), Ipv4Addr::new(192, 0, 2, 10));

        // 2. Write an invalid zones.json (disallowed public zone)
        let mut invalid = sample_zones_file(2, "192.0.2.99");
        invalid.zones[0].zone = "apps.github.com".into();
        invalid.zones[0].records[0].name = "api.apps.github.com".into();
        write_zones(&zones_path, &invalid).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let status2 = loop {
            let s = load_status(&status_path).unwrap();
            if s.error.is_some() {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for status.error after invalid reload"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(status2.generation, 1);
        assert!(status2.error.as_deref().unwrap().contains("zone"));
        // Previous table must still be served!
        assert_eq!(query_ip(2), Ipv4Addr::new(192, 0, 2, 10));

        // 3. Write valid generation 3 -> 192.0.2.20
        write_zones(&zones_path, &sample_zones_file(3, "192.0.2.20")).unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let status3 = loop {
            let s = load_status(&status_path).unwrap();
            if s.generation == 3 && s.error.is_none() {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for generation 3 reload"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(status3.generation, 3);
        assert!(status3.error.is_none());
        assert_eq!(query_ip(3), Ipv4Addr::new(192, 0, 2, 20));

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn many_answers_zones_file(count: u8) -> ZonesFile {
        let mut file = sample_zones_file(1, "192.0.2.1");
        file.zones[0].records = (1..=count)
            .map(|i| RecordEntry {
                name: "api.lab.example.internal".into(),
                ipv4: format!("192.0.2.{i}"),
            })
            .collect();
        file
    }

    fn tcp_query(stream: &mut TcpStream, query: &[u8]) -> std::io::Result<Message> {
        stream.write_all(&u16::try_from(query.len()).unwrap().to_be_bytes())?;
        stream.write_all(query)?;
        let mut len_buf = [0u8; 2];
        stream.read_exact(&mut len_buf)?;
        let mut buf = vec![0u8; u16::from_be_bytes(len_buf) as usize];
        stream.read_exact(&mut buf)?;
        Ok(Message::from_vec(&buf).unwrap())
    }

    fn loopback_tcp(port: u16) -> TcpStream {
        let stream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
    }

    #[test]
    fn respond_udp_truncates_oversized_responses_but_tcp_path_keeps_all_answers() {
        let table = ZoneTable::from_zones_file(&many_answers_zones_file(100)).unwrap();

        // Non-EDNS: 512-byte limit, header + question only, TC=1.
        let q = build_query_bytes(0xA001, "api.lab.example.internal", RecordType::A, false);
        let raw = respond_udp(&q, &table).unwrap();
        assert!(raw.len() <= 512, "got {} bytes", raw.len());
        let resp = Message::from_vec(&raw).unwrap();
        assert!(resp.metadata.truncation);
        assert!(resp.answers.is_empty());
        assert_eq!(resp.queries.len(), 1);
        assert_eq!(resp.metadata.id, 0xA001);

        // EDNS 1232: 100 A answers (~1.6 kB) still exceed the cap -> TC=1 within 1232 bytes.
        let q = build_query_bytes(0xA002, "api.lab.example.internal", RecordType::A, true);
        let raw = respond_udp(&q, &table).unwrap();
        assert!(raw.len() <= 1232, "got {} bytes", raw.len());
        let resp = Message::from_vec(&raw).unwrap();
        assert!(resp.metadata.truncation);
        assert!(resp.answers.is_empty());
        assert!(resp.edns.is_some());

        // The TCP path (plain `respond`) is never truncated.
        let full = Message::from_vec(&respond(&q, &table).unwrap()).unwrap();
        assert!(!full.metadata.truncation);
        assert_eq!(full.answers.len(), 100);

        // A response that fits is returned untouched.
        let small = sample_table();
        let q = build_query_bytes(0xA003, "api.lab.example.internal", RecordType::A, false);
        let resp = Message::from_vec(&respond_udp(&q, &small).unwrap()).unwrap();
        assert!(!resp.metadata.truncation);
        assert_eq!(resp.answers.len(), 1);
    }

    #[test]
    fn real_socket_oversized_answer_gets_tc_over_udp_and_full_set_over_tcp() {
        let dir = temp_dir("udp-tc");
        let zones_path = dir.join("zones.json");
        write_zones(&zones_path, &many_answers_zones_file(100)).unwrap();
        let handle = start_responder(
            zones_path,
            dir.join("status.json"),
            0,
            Duration::from_millis(100),
        )
        .unwrap();
        let addr = SocketAddr::from(([127, 0, 0, 1], handle.port()));

        let q = build_query_bytes(0xB001, "api.lab.example.internal", RecordType::A, false);
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        udp.send_to(&q, addr).unwrap();
        let mut buf = [0u8; 4096];
        let (len, _) = udp.recv_from(&mut buf).unwrap();
        assert!(len <= 512, "got {len} bytes");
        let udp_resp = Message::from_vec(&buf[..len]).unwrap();
        assert!(udp_resp.metadata.truncation);
        assert!(udp_resp.answers.is_empty());

        let mut tcp = loopback_tcp(handle.port());
        let tcp_resp = tcp_query(&mut tcp, &q).unwrap();
        assert!(!tcp_resp.metadata.truncation);
        assert_eq!(tcp_resp.answers.len(), 100);

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tcp_connection_cap_drops_excess_connections_and_releases_slots() {
        let dir = temp_dir("tcp-cap");
        let zones_path = dir.join("zones.json");
        write_zones(&zones_path, &sample_zones_file(1, "192.0.2.10")).unwrap();
        let handle = start_responder_with_limits(
            zones_path,
            dir.join("status.json"),
            0,
            Duration::from_millis(100),
            TcpLimits {
                max_connections: 2,
                message_deadline: Duration::from_secs(10),
                ..TcpLimits::default()
            },
        )
        .unwrap();
        let port = handle.port();
        let q = build_query_bytes(0xC001, "api.lab.example.internal", RecordType::A, false);

        // A completed round trip proves each connection holds a slot (keep-alive, still open).
        let mut held_a = loopback_tcp(port);
        let mut held_b = loopback_tcp(port);
        tcp_query(&mut held_a, &q).unwrap();
        tcp_query(&mut held_b, &q).unwrap();

        let mut over_cap = loopback_tcp(port);
        assert!(
            tcp_query(&mut over_cap, &q).is_err(),
            "connection over the cap must be dropped, not served"
        );

        // Freeing a slot lets a new connection through (release happens asynchronously).
        drop(held_a);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let mut retry = loopback_tcp(port);
            if tcp_query(&mut retry, &q).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "slot was never released");
            std::thread::sleep(Duration::from_millis(25));
        }

        drop(held_b);
        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tcp_slow_drip_client_is_cut_off_at_whole_message_deadline() {
        let dir = temp_dir("tcp-slow");
        let zones_path = dir.join("zones.json");
        write_zones(&zones_path, &sample_zones_file(1, "192.0.2.10")).unwrap();
        let handle = start_responder_with_limits(
            zones_path,
            dir.join("status.json"),
            0,
            Duration::from_millis(100),
            TcpLimits {
                max_connections: 4,
                message_deadline: Duration::from_millis(300),
                ..TcpLimits::default()
            },
        )
        .unwrap();
        let q = build_query_bytes(0xD001, "api.lab.example.internal", RecordType::A, false);
        let mut client = loopback_tcp(handle.port());

        // Announce the full length, then drip one byte per 100 ms. Each gap is far below the
        // deadline, so only a whole-message deadline (not a per-read timeout) can cut this off.
        let started = Instant::now();
        let mut wire = u16::try_from(q.len()).unwrap().to_be_bytes().to_vec();
        wire.extend_from_slice(&q);
        for byte in wire.iter().take(6) {
            let _ = client.write_all(&[*byte]);
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut sink = [0u8; 16];
        let outcome = client.read(&mut sink);
        let elapsed = started.elapsed();

        assert!(
            matches!(outcome, Ok(0))
                || outcome.as_ref().is_err_and(|e| {
                    !matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                }),
            "server must close the slow connection, got {outcome:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "cut off too late: {elapsed:?}"
        );

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tcp_write_to_a_non_reading_peer_aborts_at_the_absolute_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // The client connects but never reads, so the server's send buffer eventually fills.
        let _idle_client = TcpStream::connect(addr).unwrap();
        let (mut server_side, _) = listener.accept().unwrap();

        let payload = vec![0u8; 64 * 1024 * 1024];
        let started = Instant::now();
        let result = write_all_by(
            &mut server_side,
            &payload,
            Instant::now() + Duration::from_millis(300),
        );
        assert!(result.is_err(), "write to a stalled reader must fail");
        assert!(
            started.elapsed() < Duration::from_millis(1500),
            "write outlived the deadline: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn tcp_connection_is_closed_after_max_queries() {
        let dir = temp_dir("tcp-maxq");
        let zones_path = dir.join("zones.json");
        write_zones(&zones_path, &sample_zones_file(1, "192.0.2.10")).unwrap();
        let handle = start_responder_with_limits(
            zones_path,
            dir.join("status.json"),
            0,
            Duration::from_millis(100),
            TcpLimits {
                max_queries: 2,
                ..TcpLimits::default()
            },
        )
        .unwrap();
        let q = build_query_bytes(0xE001, "api.lab.example.internal", RecordType::A, false);
        let mut client = loopback_tcp(handle.port());
        tcp_query(&mut client, &q).unwrap();
        tcp_query(&mut client, &q).unwrap();
        assert!(
            tcp_query(&mut client, &q).is_err(),
            "third query on one connection must hit the per-connection cap"
        );

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_log_gate_logs_only_new_errors_and_first_recovery() {
        let mut gate = StatusLogGate::default();
        assert!(
            !gate.should_log_recovery(),
            "no failure yet, nothing to recover from"
        );
        assert!(gate.should_log_failure("disk full"));
        for _ in 0..100 {
            assert!(
                !gate.should_log_failure("disk full"),
                "identical error repeated"
            );
        }
        assert!(gate.should_log_failure("permission denied"));
        assert!(!gate.should_log_failure("permission denied"));
        assert!(gate.should_log_recovery());
        assert!(!gate.should_log_recovery(), "recovery is logged once");
        assert!(
            gate.should_log_failure("disk full"),
            "a new failure episode logs again"
        );
    }

    #[test]
    fn status_write_failure_is_retried_until_it_succeeds_without_zones_change() {
        let dir = temp_dir("status-retry");
        let zones_path = dir.join("zones.json");
        let status_path = dir.join("status.json");
        write_zones(&zones_path, &sample_zones_file(7, "192.0.2.10")).unwrap();

        // A directory squatting on status.json makes the atomic rename fail.
        std::fs::create_dir_all(&status_path).unwrap();
        let handle = start_responder(
            zones_path.clone(),
            status_path.clone(),
            0,
            Duration::from_millis(50),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(status_path.is_dir(), "status write must still be failing");

        // Recover without touching zones.json: the retry alone must publish the status.
        std::fs::remove_dir_all(&status_path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let status = loop {
            if let Ok(s) = load_status(&status_path) {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "status.json was never retried after the failure cleared"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(status.generation, 7);
        assert!(status.error.is_none());

        handle.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_cli_args_handles_responder_flags_and_normal_startup() {
        assert!(parse_cli_args(["clusterdeck"]).is_none());

        let parsed = parse_cli_args([
            "--dns-responder",
            "--zones",
            "/tmp/zones.json",
            "--status",
            "/tmp/status.json",
            "--port",
            "15353",
        ])
        .unwrap()
        .unwrap();
        assert_eq!(parsed.zones_path, PathBuf::from("/tmp/zones.json"));
        assert_eq!(parsed.status_path, PathBuf::from("/tmp/status.json"));
        assert_eq!(parsed.port, 15353);

        assert!(parse_cli_args(["--dns-responder"]).unwrap().is_err());
        assert!(parse_cli_args([
            "--dns-responder",
            "--zones",
            "/tmp/zones.json",
            "--port",
            "not-a-port"
        ])
        .unwrap()
        .is_err());
    }
}
