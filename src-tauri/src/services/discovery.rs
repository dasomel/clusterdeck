use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, ToSocketAddrs};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredHost {
    pub address: String,
    pub ssh_open: bool,
}

pub fn expand_targets(input: &str) -> Result<Vec<String>, String> {
    let trimmed = input.trim();
    if trimmed.contains('/') {
        let parts: Vec<&str> = trimmed.split('/').collect();
        if parts.len() != 2 {
            return Err("Invalid CIDR format".to_string());
        }
        let base_ip = Ipv4Addr::from_str(parts[0].trim())
            .map_err(|e| format!("Invalid IP address in CIDR: {e}"))?;
        let prefix: u32 = parts[1]
            .trim()
            .parse()
            .map_err(|_| "Invalid CIDR prefix".to_string())?;

        if prefix < 22 {
            return Err("CIDR range too large (max 1024 hosts)".to_string());
        }
        if prefix > 32 {
            return Err("Invalid CIDR prefix (must be <= 32)".to_string());
        }

        let ip_u32 = u32::from(base_ip);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX.checked_shl(32 - prefix).unwrap_or(0)
        };
        let net_u32 = ip_u32 & mask;
        let broadcast_u32 = net_u32 | !mask;

        let (start, end) = if prefix == 32 {
            (net_u32, net_u32)
        } else if prefix == 31 {
            (net_u32, broadcast_u32)
        } else {
            (net_u32 + 1, broadcast_u32 - 1)
        };

        let hosts = (start..=end)
            .map(|addr| Ipv4Addr::from(addr).to_string())
            .collect();
        Ok(hosts)
    } else {
        let targets: Vec<String> = trimmed
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if targets.len() > 1024 {
            return Err("Too many targets (max 1024 hosts)".to_string());
        }
        Ok(targets)
    }
}

pub async fn probe_targets(
    targets: Vec<String>,
    port: u16,
    timeout_ms: u64,
) -> Vec<DiscoveredHost> {
    let handles: Vec<_> = targets
        .into_iter()
        .map(|address| {
            tokio::task::spawn_blocking(move || {
                let target_addr = format!("{address}:{port}");
                let timeout = std::time::Duration::from_millis(timeout_ms);
                let ssh_open = match target_addr.to_socket_addrs() {
                    Ok(mut addrs) => {
                        if let Some(addr) = addrs.next() {
                            std::net::TcpStream::connect_timeout(&addr, timeout).is_ok()
                        } else {
                            false
                        }
                    }
                    Err(_) => false,
                };
                DiscoveredHost { address, ssh_open }
            })
        })
        .collect();

    let mut results = Vec::with_capacity(handles.len());
    for handle in handles {
        if let Ok(res) = handle.await {
            results.push(res);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_targets_parses_comma_list() {
        let out = expand_targets("192.0.2.10, 192.0.2.11").unwrap();
        assert_eq!(out, vec!["192.0.2.10", "192.0.2.11"]);
    }

    #[test]
    fn expand_targets_parses_small_cidr() {
        let out = expand_targets("192.0.2.0/30").unwrap();
        // /30 = 4 addresses, 2 usable hosts (.1, .2)
        assert_eq!(out, vec!["192.0.2.1", "192.0.2.2"]);
    }

    #[test]
    fn expand_targets_rejects_oversized_cidr() {
        assert!(expand_targets("10.0.0.0/8").is_err());
    }

    #[test]
    fn expand_targets_rejects_oversized_comma_list() {
        let list = (0..1025)
            .map(|i| format!("10.0.{}.{}", i / 256, i % 256))
            .collect::<Vec<_>>()
            .join(",");
        assert!(expand_targets(&list).is_err());
    }

    #[tokio::test]
    async fn probe_targets_marks_unreachable_localhost_port_closed() {
        // Port 1 is reserved/unlikely to be listening in CI.
        let results = probe_targets(vec!["127.0.0.1".into()], 1, 200).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].ssh_open);
    }
}
