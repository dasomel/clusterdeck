#![allow(dead_code)]

use crate::services::inventory::model::Machine;
use std::{net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, time::timeout};

// D7: Kubernetes detection is evidence-based and read-only: kubeconfig server hosts first, then a TCP probe of 6443.
// A closed port only means "not detected"; a machine without an IP stays "unknown".
pub fn kubeconfig_servers(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let url = line
                .trim()
                .trim_start_matches("- ")
                .strip_prefix("server:")?
                .trim()
                .trim_matches(|c| c == '"' || c == '\'');
            let authority = url
                .split_once("://")
                .map_or(url, |(_, rest)| rest)
                .split('/')
                .next()?;
            let host = if let Some(v6) = authority.strip_prefix('[') {
                v6.split(']').next()?
            } else {
                authority.split(':').next()?
            };
            (!host.is_empty()).then(|| host.to_owned())
        })
        .collect()
}

async fn kubeconfig_hosts() -> Vec<String> {
    let paths: Vec<String> = match std::env::var("KUBECONFIG") {
        Ok(v) if !v.is_empty() => v.split(':').map(String::from).collect(),
        _ => std::env::var("HOME")
            .map(|h| vec![format!("{h}/.kube/config")])
            .unwrap_or_default(),
    };
    let mut servers = Vec::new();
    for p in paths {
        if let Ok(content) = tokio::fs::read_to_string(p).await {
            servers.extend(kubeconfig_servers(&content));
        }
    }
    servers
}

async fn api_reachable(ip: &str) -> bool {
    let Ok(addr) = format!("{ip}:6443")
        .parse::<SocketAddr>()
        .or_else(|_| format!("[{ip}]:6443").parse())
    else {
        return false;
    };
    matches!(
        timeout(Duration::from_millis(500), TcpStream::connect(addr)).await,
        Ok(Ok(_))
    )
}

pub async fn detect(machines: &mut [Machine]) {
    let hosts = kubeconfig_hosts().await;
    let mut probes = vec![];
    for (i, m) in machines.iter_mut().enumerate() {
        if m.ips.is_empty() {
            continue;
        }
        if m.ips.iter().any(|ip| hosts.contains(ip)) {
            m.kubernetes = "kubeconfig server".into();
            continue;
        }
        if m.state != "running" {
            continue;
        }
        let ips = m.ips.clone();
        probes.push((
            i,
            tokio::spawn(async move {
                for ip in ips {
                    if api_reachable(&ip).await {
                        return true;
                    }
                }
                false
            }),
        ));
    }
    for (i, probe) in probes {
        machines[i].kubernetes = if probe.await.unwrap_or(false) {
            "API server reachable (6443)"
        } else {
            "not detected"
        }
        .into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_hosts() {
        let text = "clusters:\n- cluster:\n    server: https://192.168.56.11:6443\n  name: a\n- cluster:\n    server: \"https://[fd00::1]:6443\"\n  name: b\n    # server: nothing";
        assert_eq!(kubeconfig_servers(text), vec!["192.168.56.11", "fd00::1"]);
    }
}
