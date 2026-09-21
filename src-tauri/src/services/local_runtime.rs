#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

use crate::services::process::CommandRunner;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredLocalHost {
    pub provider: String,
    pub instance_name: String,
    pub status: String,
    pub host_name: String,
    pub address: String,
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
    pub runtime: Option<String>,
    pub kube_context: Option<String>,
    pub kube_remote_path: Option<String>,
    pub arch: Option<String>,
    pub cpus: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub docker_context: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SshConfigBlock {
    pub host: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
}

pub fn parse_ssh_config_blocks(raw: &str) -> Vec<SshConfigBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<SshConfigBlock> = None;

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Host ") {
            if let Some(b) = current.take() {
                blocks.push(b);
            }
            current = Some(SshConfigBlock {
                host: Some(rest.trim().to_string()),
                ..Default::default()
            });
            continue;
        }

        if let Some(ref mut b) = current {
            if let Some(rest) = trimmed.strip_prefix("Hostname ") {
                b.hostname = Some(rest.trim().trim_matches('"').to_string());
            } else if let Some(rest) = trimmed.strip_prefix("HostName ") {
                b.hostname = Some(rest.trim().trim_matches('"').to_string());
            } else if let Some(rest) = trimmed.strip_prefix("Port ") {
                if let Ok(p) = rest.trim().parse::<u16>() {
                    b.port = Some(p);
                }
            } else if let Some(rest) = trimmed.strip_prefix("User ") {
                b.user = Some(rest.trim().trim_matches('"').to_string());
            } else if let Some(rest) = trimmed.strip_prefix("IdentityFile ") {
                let candidate = rest.trim().trim_matches('"').to_string();
                if b.identity_file.is_none() {
                    b.identity_file = Some(candidate);
                } else if let Some(ref existing) = b.identity_file {
                    if !Path::new(existing).exists() && Path::new(&candidate).exists() {
                        b.identity_file = Some(candidate);
                    }
                }
            }
        }
    }

    if let Some(b) = current {
        blocks.push(b);
    }

    blocks
}

fn parse_ssh_config_block(raw: &str) -> SshConfigBlock {
    parse_ssh_config_blocks(raw)
        .into_iter()
        .next()
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct ColimaListRow {
    name: String,
    status: Option<String>,
    runtime: Option<String>,
    arch: Option<String>,
    cpus: Option<u32>,
    memory: Option<u64>,
    disk: Option<u64>,
}

#[derive(Deserialize)]
struct DockerContextRow {
    #[serde(rename = "Name")]
    name: String,
}

async fn detect_docker_contexts(runner: &dyn CommandRunner) -> HashSet<String> {
    let output = match runner
        .run(
            "docker",
            &[
                "context".into(),
                "ls".into(),
                "--format".into(),
                "json".into(),
            ],
        )
        .await
    {
        Ok(res) if res.success => res.stdout,
        _ => return HashSet::new(),
    };

    output
        .lines()
        .filter_map(|line| serde_json::from_str::<DockerContextRow>(line.trim()).ok())
        .map(|row| row.name)
        .collect()
}

async fn detect_colima(
    runner: &dyn CommandRunner,
    docker_contexts: &HashSet<String>,
) -> Vec<DiscoveredLocalHost> {
    let mut out = Vec::new();
    let list_output = match runner
        .run("colima", &["list".to_string(), "--json".to_string()])
        .await
    {
        Ok(res) if res.success => res.stdout,
        _ => return out,
    };

    for line in list_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: ColimaListRow = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        // entry.name is untrusted `colima list --json` output; a leading `-` would be parsed as
        // a CLI option.
        if !crate::services::validate::is_safe_ssh_identifier(&entry.name) {
            continue;
        }

        let status = entry.status.unwrap_or_else(|| "Unknown".to_string());
        let has_k8s = entry
            .runtime
            .as_deref()
            .map(|r| r.contains("k3s") || r.contains("kubernetes"))
            .unwrap_or(false);

        let mut ssh_args = vec!["ssh-config".to_string()];
        if entry.name != "default" {
            ssh_args.push("--profile".to_string());
            ssh_args.push(entry.name.clone());
        }

        let mut address = "127.0.0.1".to_string();
        let mut port = 22;
        let mut user = "root".to_string();
        let mut identity_file = None;

        if let Ok(ssh_res) = runner.run("colima", &ssh_args).await {
            if ssh_res.success {
                let parsed = parse_ssh_config_block(&ssh_res.stdout);
                if let Some(h) = parsed.hostname {
                    address = h;
                }
                if let Some(p) = parsed.port {
                    port = p;
                }
                if let Some(u) = parsed.user {
                    user = u;
                }
                identity_file = parsed.identity_file;
            }
        }

        let host_name = if entry.name == "default" {
            "colima-vm".to_string()
        } else {
            format!("colima-{}", entry.name)
        };

        let kube_context = if has_k8s {
            if entry.name == "default" {
                Some("colima".to_string())
            } else {
                Some(format!("colima-{}", entry.name))
            }
        } else {
            None
        };

        let kube_remote_path = if has_k8s {
            Some("/etc/rancher/k3s/k3s.yaml".to_string())
        } else {
            None
        };

        let docker_context = {
            let context = if entry.name == "default" {
                "colima".to_string()
            } else {
                format!("colima-{}", entry.name)
            };
            docker_contexts.contains(&context).then_some(context)
        };

        out.push(DiscoveredLocalHost {
            provider: "Colima".to_string(),
            instance_name: entry.name,
            status,
            host_name,
            address,
            port,
            user,
            identity_file,
            runtime: entry.runtime,
            kube_context,
            kube_remote_path,
            arch: entry.arch,
            cpus: entry.cpus,
            memory_bytes: entry.memory,
            disk_bytes: entry.disk,
            docker_context,
        });
    }
    out
}

#[derive(Deserialize)]
struct LimaUser {
    name: Option<String>,
}

#[derive(Deserialize)]
struct LimaConfig {
    user: Option<LimaUser>,
}

#[derive(Deserialize)]
struct LimaListRow {
    name: String,
    status: Option<String>,
    #[serde(rename = "sshAddress")]
    ssh_address: Option<String>,
    #[serde(rename = "sshLocalPort")]
    ssh_local_port: Option<u16>,
    #[serde(rename = "IdentityFile")]
    identity_file: Option<String>,
    config: Option<LimaConfig>,
    arch: Option<String>,
    cpus: Option<u32>,
    memory: Option<u64>,
    disk: Option<u64>,
}

async fn detect_lima(runner: &dyn CommandRunner) -> Vec<DiscoveredLocalHost> {
    let mut out = Vec::new();
    let list_output = match runner
        .run("limactl", &["list".to_string(), "--json".to_string()])
        .await
    {
        Ok(res) if res.success => res.stdout,
        _ => return out,
    };

    for line in list_output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: LimaListRow = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let status = entry.status.unwrap_or_else(|| "Unknown".to_string());
        let address = entry.ssh_address.unwrap_or_else(|| "127.0.0.1".to_string());
        let port = entry.ssh_local_port.unwrap_or(0);
        let user = entry
            .config
            .and_then(|c| c.user)
            .and_then(|u| u.name)
            .unwrap_or_else(|| "root".to_string());

        let host_name = format!("lima-{}", entry.name);

        out.push(DiscoveredLocalHost {
            provider: "Lima".to_string(),
            instance_name: entry.name,
            status,
            host_name,
            address,
            port,
            user,
            identity_file: entry.identity_file,
            runtime: None,
            kube_context: None,
            kube_remote_path: None,
            arch: entry.arch,
            cpus: entry.cpus,
            memory_bytes: entry.memory,
            disk_bytes: entry.disk,
            docker_context: None,
        });
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
struct VagrantEntry {
    id: String,
    name: String,
    provider: String,
    state: String,
    directory: String,
}

fn parse_vagrant_global_status(stdout: &str) -> Vec<VagrantEntry> {
    let mut entries = Vec::new();
    let mut in_table = false;

    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("---") {
            in_table = true;
            continue;
        }
        if !in_table {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("The above") {
            break;
        }

        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() >= 5 {
            let id = tokens[0].to_string();
            let name = tokens[1].to_string();
            let provider = tokens[2].to_string();
            let directory = tokens[tokens.len() - 1].to_string();
            let state = tokens[3..tokens.len() - 1].join(" ");

            entries.push(VagrantEntry {
                id,
                name,
                provider,
                state,
                directory,
            });
        }
    }
    entries
}

pub fn extract_private_network_ip(ip_output: &str, current_address: &str) -> Option<String> {
    let mut candidates: Vec<(u32, String)> = Vec::new();

    for line in ip_output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        if tokens.len() < 3 {
            continue;
        }

        let raw_ifname = if tokens[0].ends_with(':') {
            tokens.get(1).unwrap_or(&"")
        } else {
            tokens[0]
        };
        let ifname = raw_ifname.trim_matches(':').to_lowercase();

        if ifname == "lo"
            || ifname.starts_with("docker")
            || ifname.starts_with("cilium")
            || ifname.starts_with("cni")
            || ifname.starts_with("flannel")
            || ifname.starts_with("calico")
            || ifname.starts_with("veth")
            || ifname.starts_with("tun")
            || ifname.starts_with("tap")
            || ifname.starts_with("br-")
            || ifname.starts_with("virbr")
            || ifname.starts_with("kube")
        {
            continue;
        }

        let inet_pos = match tokens.iter().position(|&t| t == "inet") {
            Some(pos) => pos,
            None => continue,
        };

        let cidr = match tokens.get(inet_pos + 1) {
            Some(c) => *c,
            None => continue,
        };

        let ip_str = cidr.split('/').next().unwrap_or("");
        let ipv4: std::net::Ipv4Addr = match ip_str.parse() {
            Ok(ip) => ip,
            Err(_) => continue,
        };

        if ipv4.is_loopback() || ipv4.is_link_local() || ipv4.is_unspecified() {
            continue;
        }

        let is_current = ip_str == current_address;
        let is_dynamic = trimmed.contains("dynamic");
        let is_global = trimmed.contains("scope global");
        let is_private = ipv4.is_private();

        let mut score: u32 = 0;
        if is_global {
            score += 10;
        }
        if is_private {
            score += 30;
        }
        if !is_dynamic {
            score += 40;
        }
        if !is_current {
            score += 20;
        }

        if is_global && is_private {
            candidates.push((score, ip_str.to_string()));
        }
    }

    candidates.sort_by_key(|a| std::cmp::Reverse(a.0));
    if let Some((best_score, best_ip)) = candidates.first() {
        if best_ip != current_address || *best_score >= 80 {
            return Some(best_ip.clone());
        }
    }

    None
}

fn extract_ip_from_vagrantfile(
    content: &str,
    machine_name: &str,
    env_map: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let subnet = env_map
        .get("SUBNET_PREFIX")
        .cloned()
        .unwrap_or_else(|| "192.168.77".to_string());

    let lines: Vec<&str> = content.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains(&format!("'{machine_name}'"))
            || trimmed.contains(&format!("\"{machine_name}\""))
            || trimmed.contains(&format!(":{machine_name}"))
            || (trimmed.contains(machine_name) && trimmed.contains("name:"))
        {
            for next_line in lines.iter().skip(i).take(6) {
                if let Some(pos) = next_line.find("ip:") {
                    let rest = next_line[pos + 3..].trim();
                    let raw_val = rest
                        .split(&[',', '}', '\n'][..])
                        .next()
                        .unwrap_or("")
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'');
                    let resolved = raw_val.replace("#{subnet}", &subnet);
                    if resolved.parse::<std::net::Ipv4Addr>().is_ok() {
                        return Some(resolved);
                    }
                }
            }
        }
    }

    if machine_name == "master" || machine_name == "master-1" {
        for line in &lines {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("MASTER_IP") {
                if let Some((_, val)) = rest.split_once('=') {
                    let ip = val.trim().trim_matches('"').trim_matches('\'');
                    if ip.parse::<std::net::Ipv4Addr>().is_ok() {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }

    if machine_name.starts_with("worker-") || machine_name.starts_with("worker") {
        let num_str = machine_name
            .trim_start_matches("worker-")
            .trim_start_matches("worker");
        if let Ok(idx) = num_str.parse::<usize>() {
            let zero_based = if idx > 0 { idx - 1 } else { 0 };
            let mut in_worker_ips = false;
            let mut collected_ips = Vec::new();
            for line in &lines {
                let trimmed = line.trim();
                if trimmed.starts_with("WORKER_IPS") {
                    in_worker_ips = true;
                }
                if in_worker_ips {
                    for part in trimmed.split(&[',', '[', ']'][..]) {
                        let candidate = part.trim().trim_matches('"').trim_matches('\'');
                        if candidate.parse::<std::net::Ipv4Addr>().is_ok() {
                            collected_ips.push(candidate.to_string());
                        }
                    }
                    if trimmed.contains(']') {
                        break;
                    }
                }
            }
            if let Some(ip) = collected_ips.get(zero_based) {
                return Some(ip.clone());
            }
        }
    }

    None
}

fn find_static_vagrant_info(vagrant_dir: &Path, machine_name: &str) -> (Option<String>, bool) {
    let mut detected_ip = None;
    let mut has_k3s = false;

    let env_candidates = [
        vagrant_dir.join("configs").join("cluster.env"),
        vagrant_dir.join("cluster.env"),
        vagrant_dir.join(".env"),
    ];

    let mut env_map = std::collections::HashMap::new();
    for env_path in &env_candidates {
        if let Ok(content) = std::fs::read_to_string(env_path) {
            if content.to_ascii_lowercase().contains("k3s") {
                has_k3s = true;
            }
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = trimmed.split_once('=') {
                    let key = k.trim().to_string();
                    let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    env_map.insert(key, val);
                }
            }
        }
    }

    let norm_name = machine_name.to_ascii_uppercase().replace('-', "_");
    let compact_name = machine_name.to_ascii_uppercase().replace('-', "");

    let mut ip_keys = vec![
        format!("{norm_name}_IP"),
        format!("{compact_name}_IP"),
        norm_name.clone(),
    ];

    if machine_name.contains("master") || machine_name.contains("control") {
        ip_keys.push("MASTER_IP".to_string());
        ip_keys.push("MASTER1_IP".to_string());
        ip_keys.push("CONTROL_PLANE_IP".to_string());
        ip_keys.push("CONTROL_IP".to_string());
    }

    for k in &ip_keys {
        if let Some(val) = env_map.get(k) {
            if val.parse::<std::net::Ipv4Addr>().is_ok() {
                detected_ip = Some(val.clone());
                break;
            }
        }
    }

    let vagrantfile_path = vagrant_dir.join("Vagrantfile");
    if let Ok(content) = std::fs::read_to_string(&vagrantfile_path) {
        if content.to_ascii_lowercase().contains("k3s") {
            has_k3s = true;
        }

        if detected_ip.is_none() {
            detected_ip = extract_ip_from_vagrantfile(&content, machine_name, &env_map);
        }
    }

    (detected_ip, has_k3s)
}

async fn detect_vagrant(runner: &dyn CommandRunner) -> Vec<DiscoveredLocalHost> {
    let mut out = Vec::new();
    let status_output = match runner.run("vagrant", &["global-status".to_string()]).await {
        Ok(res) if res.success => res.stdout,
        _ => return out,
    };

    let entries = parse_vagrant_global_status(&status_output);
    let local_contexts =
        crate::services::kube_import::list_local_kube_contexts().unwrap_or_default();

    for entry in entries {
        // entry.id is untrusted `vagrant global-status` output; a leading `-` would be parsed as
        // a CLI option.
        if !crate::services::validate::is_safe_ssh_identifier(&entry.id) {
            continue;
        }

        let is_running = entry.state.eq_ignore_ascii_case("running");
        let project_name = Path::new(&entry.directory)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("vagrant")
            .to_string();

        let (static_ip, static_has_k3s) =
            find_static_vagrant_info(Path::new(&entry.directory), &entry.name);
        let mut has_k3s = static_has_k3s;

        let is_master = entry.name.contains("master") || entry.name.contains("control");
        let kube_ctx_match = local_contexts
            .iter()
            .find(|c| c.context_name == project_name || c.context_name.contains(&project_name))
            .map(|c| c.context_name.clone());

        let kube_context = if is_master {
            kube_ctx_match.or_else(|| Some(project_name.clone()))
        } else {
            None
        };

        let mut address = "127.0.0.1".to_string();
        let mut port = 22;
        let mut user = "vagrant".to_string();
        let mut identity_file = None;

        if is_running {
            if let Ok(ssh_res) = runner
                .run("vagrant", &["ssh-config".to_string(), entry.id.clone()])
                .await
            {
                if ssh_res.success {
                    let blocks = parse_ssh_config_blocks(&ssh_res.stdout);
                    if let Some(b) = blocks.into_iter().next() {
                        if let Some(h) = b.hostname {
                            address = h;
                        }
                        if let Some(p) = b.port {
                            port = p;
                        }
                        if let Some(u) = b.user {
                            user = u;
                        }
                        identity_file = b.identity_file;
                    }
                }
            }
        }

        if identity_file.is_none() {
            let home = std::env::var("HOME").unwrap_or_default();
            let candidate_ed25519 =
                format!("{home}/.vagrant.d/insecure_private_keys/vagrant.key.ed25519");
            let candidate_insecure = format!("{home}/.vagrant.d/insecure_private_key");
            let machine_key = format!(
                "{}/.vagrant/machines/{}/{}/private_key",
                entry.directory, entry.name, entry.provider
            );

            if Path::new(&candidate_ed25519).exists() {
                identity_file = Some(candidate_ed25519);
            } else if Path::new(&machine_key).exists() {
                identity_file = Some(machine_key);
            } else if Path::new(&candidate_insecure).exists() {
                identity_file = Some(candidate_insecure);
            }
        }

        // Live probe running machine for private network IP and k3s
        if is_running
            && crate::services::validate::is_safe_ssh_identifier(&address)
            && crate::services::validate::is_safe_ssh_identifier(&user)
        {
            let safe_key = identity_file
                .as_ref()
                .map(|k| crate::services::validate::is_safe_ssh_identifier(k))
                .unwrap_or(true);

            if safe_key {
                // Throwaway Host: build_ssh_target_args only reads address/port/user/identity_file,
                // so `name` just needs to be present, not meaningful outside this probe call.
                let probe_host = crate::services::config::Host {
                    name: entry.name.clone(),
                    address: address.clone(),
                    port,
                    user: user.clone(),
                    identity_file: identity_file.clone(),
                };
                let probe_args = crate::services::ssh::build_ssh_target_args(
                    &probe_host,
                    None,
                    &["ip -4 -o addr show; test -f /etc/rancher/k3s/k3s.yaml && echo HAS_K3S || true"],
                );

                if let Ok(probe_res) = runner.run("ssh", &probe_args).await {
                    if probe_res.success {
                        if probe_res.stdout.contains("HAS_K3S") {
                            has_k3s = true;
                        }
                        if let Some(priv_ip) =
                            extract_private_network_ip(&probe_res.stdout, &address)
                        {
                            address = priv_ip;
                            port = 22;
                        }
                    }
                }
            }
        }

        // Static fallback if address still appears to be NAT or loopback
        if let Some(sip) = static_ip {
            if address == "127.0.0.1"
                || address.starts_with("172.16.")
                || address.starts_with("10.0.2.")
            {
                address = sip;
                port = 22;
            }
        }

        let kube_remote_path = if is_master {
            if has_k3s {
                Some("/etc/rancher/k3s/k3s.yaml".to_string())
            } else {
                Some("/etc/kubernetes/admin.conf".to_string())
            }
        } else {
            None
        };

        let status = if is_running {
            "Running".to_string()
        } else {
            "Stopped".to_string()
        };

        out.push(DiscoveredLocalHost {
            provider: "Vagrant".to_string(),
            instance_name: format!("{}/{}", project_name, entry.name),
            status,
            host_name: entry.name,
            address,
            port,
            user,
            identity_file,
            runtime: Some(format!("vagrant ({})", entry.provider)),
            kube_context,
            kube_remote_path,
            arch: None,
            cpus: None,
            memory_bytes: None,
            disk_bytes: None,
            docker_context: None,
        });
    }
    out
}

pub async fn detect_local_hosts(
    runner: &dyn CommandRunner,
) -> Result<Vec<DiscoveredLocalHost>, String> {
    // Colima, Lima, and Vagrant detection are independent of each other - run concurrently.
    let docker_contexts = detect_docker_contexts(runner).await;
    let (colima, lima, vagrant) = tokio::join!(
        detect_colima(runner, &docker_contexts),
        detect_lima(runner),
        detect_vagrant(runner)
    );

    let mut results = colima;
    results.extend(lima);
    results.extend(vagrant);
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct FakeRunner {
        colima_list: &'static str,
        colima_ssh: &'static str,
        lima_list: &'static str,
        docker_contexts: &'static str,
        vagrant_status: &'static str,
        vagrant_ssh: &'static str,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    #[async_trait]
    impl CommandRunner for FakeRunner {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            self.calls
                .lock()
                .unwrap()
                .push((bin.to_string(), args.to_vec()));
            if bin == "colima" {
                if args.first().map(|s| s.as_str()) == Some("list") {
                    return Ok(CommandOutput {
                        stdout: self.colima_list.into(),
                        stderr: String::new(),
                        success: true,
                    });
                } else if args.first().map(|s| s.as_str()) == Some("ssh-config") {
                    return Ok(CommandOutput {
                        stdout: self.colima_ssh.into(),
                        stderr: String::new(),
                        success: true,
                    });
                }
            } else if bin == "limactl" && args.first().map(|s| s.as_str()) == Some("list") {
                return Ok(CommandOutput {
                    stdout: self.lima_list.into(),
                    stderr: String::new(),
                    success: true,
                });
            } else if bin == "vagrant" {
                if args.first().map(|s| s.as_str()) == Some("global-status") {
                    return Ok(CommandOutput {
                        stdout: self.vagrant_status.into(),
                        stderr: String::new(),
                        success: true,
                    });
                } else if args.first().map(|s| s.as_str()) == Some("ssh-config") {
                    return Ok(CommandOutput {
                        stdout: self.vagrant_ssh.into(),
                        stderr: String::new(),
                        success: true,
                    });
                }
            } else if bin == "docker" {
                return Ok(CommandOutput {
                    stdout: self.docker_contexts.into(),
                    stderr: String::new(),
                    success: true,
                });
            }
            Err(format!("unknown command {bin}"))
        }
    }

    #[test]
    fn parse_ssh_config_extracts_fields() {
        let raw = r#"
Host colima
  IdentityFile "/Users/test/.colima/_lima/_config/user"
  User testuser
  Hostname 127.0.0.1
  Port 56260
"#;
        let parsed = parse_ssh_config_block(raw);
        assert_eq!(parsed.hostname.as_deref(), Some("127.0.0.1"));
        assert_eq!(parsed.port, Some(56260));
        assert_eq!(parsed.user.as_deref(), Some("testuser"));
        assert_eq!(
            parsed.identity_file.as_deref(),
            Some("/Users/test/.colima/_lima/_config/user")
        );
    }

    #[test]
    fn parse_vagrant_global_status_extracts_rows() {
        let raw = r#"
id       name     provider       state       directory                                             
---------------------------------------------------------------------------------------------------
5171a8b  master   vmware_desktop not running /Users/m/cka-lab    
f4f4176  master-1 vmware_fusion  running     /Users/m/mdp/beluga 
9112176  worker-1 vmware_fusion  running     /Users/m/mdp/beluga 
 
The above shows information...
"#;
        let entries = parse_vagrant_global_status(raw);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id, "5171a8b");
        assert_eq!(entries[0].name, "master");
        assert_eq!(entries[0].provider, "vmware_desktop");
        assert_eq!(entries[0].state, "not running");
        assert_eq!(entries[0].directory, "/Users/m/cka-lab");

        assert_eq!(entries[1].id, "f4f4176");
        assert_eq!(entries[1].name, "master-1");
        assert_eq!(entries[1].state, "running");
    }

    #[tokio::test]
    async fn detect_local_hosts_parses_colima_lima_and_vagrant() {
        let colima_list = r#"{"name":"default","status":"Running","arch":"aarch64","cpus":6,"memory":12884901888,"disk":107374182400,"runtime":"docker+k3s"}
malformed line that must be skipped
{"name":"other","status":"Stopped","runtime":"docker"}"#;

        let colima_ssh = r#"Host colima
  IdentityFile "/path/to/key"
  User testuser
  Hostname 127.0.0.1
  Port 56260"#;

        let lima_list = r#"{"name":"k8s","status":"Running","arch":"aarch64","cpus":4,"memory":4294967296,"disk":21474836480,"sshAddress":"127.0.0.1","sshLocalPort":50326,"IdentityFile":"/path/to/lima/key","config":{"user":{"name":"limauser"}}}"#;
        let docker_contexts = r#"{"Name":"colima"}
{"Name":"other-context"}"#;

        let vagrant_status = r#"
id       name     provider       state   directory
------------------------------------------------------------------
f4f4176  master-1 vmware_fusion  running /Users/m/projects/mycluster
"#;

        let vagrant_ssh = r#"
Host master-1
  HostName 172.16.221.133
  User vagrant
  Port 2222
  IdentityFile /Users/m/.vagrant.d/insecure_private_keys/vagrant.key.ed25519
  IdentityFile /Users/m/.vagrant.d/insecure_private_keys/vagrant.key.rsa
"#;

        let runner = FakeRunner {
            colima_list,
            colima_ssh,
            lima_list,
            docker_contexts,
            vagrant_status,
            vagrant_ssh,
            calls: Mutex::new(Vec::new()),
        };

        let result = detect_local_hosts(&runner).await.unwrap();
        assert_eq!(result.len(), 4);

        // First Colima instance
        assert_eq!(result[0].provider, "Colima");
        assert_eq!(result[0].instance_name, "default");
        assert_eq!(result[0].status, "Running");
        assert_eq!(result[0].host_name, "colima-vm");
        assert_eq!(result[0].port, 56260);
        assert_eq!(result[0].user, "testuser");
        assert_eq!(result[0].kube_context.as_deref(), Some("colima"));
        assert_eq!(result[0].arch.as_deref(), Some("aarch64"));
        assert_eq!(result[0].cpus, Some(6));
        assert_eq!(result[0].memory_bytes, Some(12884901888));
        assert_eq!(result[0].disk_bytes, Some(107374182400));
        assert_eq!(result[0].docker_context.as_deref(), Some("colima"));

        // Second Colima instance: derived context "colima-other" is not in the fixture's
        // discovered docker_contexts set ("colima", "other-context"), so it must gate to None
        // rather than being synthesized.
        assert_eq!(result[1].provider, "Colima");
        assert_eq!(result[1].instance_name, "other");
        assert_eq!(result[1].docker_context, None);

        // Lima instance
        assert_eq!(result[2].provider, "Lima");
        assert_eq!(result[2].instance_name, "k8s");
        assert_eq!(result[2].port, 50326);
        assert_eq!(result[2].memory_bytes, Some(4294967296));
        assert_eq!(result[2].docker_context, None);

        // Vagrant instance
        assert_eq!(result[3].provider, "Vagrant");
        assert_eq!(result[3].instance_name, "mycluster/master-1");
        assert_eq!(result[3].host_name, "master-1");
        assert_eq!(result[3].address, "172.16.221.133");
        assert_eq!(result[3].port, 2222);
        assert_eq!(result[3].user, "vagrant");
        assert_eq!(
            result[3].identity_file.as_deref(),
            Some("/Users/m/.vagrant.d/insecure_private_keys/vagrant.key.ed25519")
        );
        assert_eq!(result[3].status, "Running");
        assert_eq!(
            result[3].runtime.as_deref(),
            Some("vagrant (vmware_fusion)")
        );
        assert_eq!(
            result[3].kube_remote_path.as_deref(),
            Some("/etc/kubernetes/admin.conf")
        );
    }

    #[test]
    fn extract_private_network_ip_selects_correct_private_ip() {
        let sample_output = r#"
1: lo    inet 127.0.0.1/8 scope host lo\       valid_lft forever preferred_lft forever
2: enp2s0    inet 172.16.221.133/24 metric 100 brd 172.16.221.255 scope global dynamic enp2s0\       valid_lft 1468sec preferred_lft 1468sec
3: enp26s0    inet 192.168.77.10/24 brd 192.168.77.255 scope global enp26s0\       valid_lft forever preferred_lft forever
5: cilium_host    inet 10.42.0.39/32 scope global cilium_host\       valid_lft forever preferred_lft forever
HAS_K3S
"#;
        let priv_ip = extract_private_network_ip(sample_output, "172.16.221.133");
        assert_eq!(priv_ip.as_deref(), Some("192.168.77.10"));
    }

    struct FakeRunnerWithSsh {
        vagrant_status: &'static str,
        vagrant_ssh: &'static str,
        ssh_probe_stdout: &'static str,
    }

    #[async_trait]
    impl CommandRunner for FakeRunnerWithSsh {
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            if bin == "vagrant" {
                if args.first().map(|s| s.as_str()) == Some("global-status") {
                    return Ok(CommandOutput {
                        stdout: self.vagrant_status.into(),
                        stderr: String::new(),
                        success: true,
                    });
                } else if args.first().map(|s| s.as_str()) == Some("ssh-config") {
                    return Ok(CommandOutput {
                        stdout: self.vagrant_ssh.into(),
                        stderr: String::new(),
                        success: true,
                    });
                }
            } else if bin == "ssh" {
                return Ok(CommandOutput {
                    stdout: self.ssh_probe_stdout.into(),
                    stderr: String::new(),
                    success: true,
                });
            }
            Err(format!("unknown command {bin}"))
        }
    }

    #[tokio::test]
    async fn detect_vagrant_probes_live_ip_and_k3s() {
        let vagrant_status = r#"
id       name     provider       state   directory
------------------------------------------------------------------
f4f4176  master-1 vmware_fusion  running /Users/m/projects/mycluster
"#;
        let vagrant_ssh = r#"
Host master-1
  HostName 172.16.221.133
  User vagrant
  Port 2222
"#;
        let ssh_probe_stdout = r#"
1: lo    inet 127.0.0.1/8 scope host lo\
2: enp2s0    inet 172.16.221.133/24 scope global dynamic enp2s0\
3: enp26s0    inet 192.168.77.10/24 brd 192.168.77.255 scope global enp26s0\
HAS_K3S
"#;

        let runner = FakeRunnerWithSsh {
            vagrant_status,
            vagrant_ssh,
            ssh_probe_stdout,
        };

        let results = detect_vagrant(&runner).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].address, "192.168.77.10");
        assert_eq!(results[0].port, 22);
        assert_eq!(
            results[0].kube_remote_path.as_deref(),
            Some("/etc/rancher/k3s/k3s.yaml")
        );
    }

    #[test]
    fn find_static_vagrant_info_detects_ip_and_k3s() {
        let temp_dir = std::env::temp_dir().join(format!(
            "cd_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(temp_dir.join("configs")).unwrap();
        let env_content = r#"
MASTER_IP="192.168.77.10"
WORKER1_IP="192.168.77.21"
K8S_VERSION="1.36" # k3s channel
"#;
        std::fs::write(temp_dir.join("configs/cluster.env"), env_content).unwrap();

        let (master_ip, master_k3s) = find_static_vagrant_info(&temp_dir, "master-1");
        assert_eq!(master_ip.as_deref(), Some("192.168.77.10"));
        assert!(master_k3s);

        let (worker_ip, worker_k3s) = find_static_vagrant_info(&temp_dir, "worker-1");
        assert_eq!(worker_ip.as_deref(), Some("192.168.77.21"));
        assert!(worker_k3s);

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[tokio::test]
    async fn detect_colima_skips_row_with_unsafe_profile_name() {
        let colima_list = r#"{"name":"default","status":"Running","runtime":"docker+k3s"}
{"name":"-oProxyCommand=evil","status":"Running","runtime":"docker"}"#;
        let colima_ssh = r#"Host colima
  User testuser
  Hostname 127.0.0.1
  Port 56260"#;

        let runner = FakeRunner {
            colima_list,
            colima_ssh,
            lima_list: "",
            docker_contexts: "",
            vagrant_status: "",
            vagrant_ssh: "",
            calls: Mutex::new(Vec::new()),
        };

        let result = detect_colima(&runner, &HashSet::new()).await;
        // The hostile row is dropped; only "default" survives.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].instance_name, "default");

        let calls = runner.calls.lock().unwrap();
        assert!(!calls.is_empty());
        for (_, args) in calls.iter() {
            for arg in args {
                assert!(!arg.contains("-oProxyCommand=evil"));
            }
        }
    }

    #[tokio::test]
    async fn detect_vagrant_skips_row_with_unsafe_id() {
        // `parse_vagrant_global_status` takes the table's first whitespace-delimited token as
        // `id` with no format check, so a hostile-looking id (here, one shaped like a CLI flag)
        // parses through it exactly like any other id; the defensive guard is the only stop.
        let vagrant_status = r#"
id       name     provider       state   directory
------------------------------------------------------------------
f4f4176  master-1 vmware_fusion  running /Users/m/projects/mycluster
--help   worker-1 vmware_fusion  running /Users/m/projects/mycluster
"#;
        let vagrant_ssh = r#"
Host master-1
  HostName 172.16.221.133
  User vagrant
  Port 2222
"#;

        let runner = FakeRunner {
            colima_list: "",
            colima_ssh: "",
            lima_list: "",
            docker_contexts: "",
            vagrant_status,
            vagrant_ssh,
            calls: Mutex::new(Vec::new()),
        };

        let result = detect_vagrant(&runner).await;
        // The hostile row (id "--help") is dropped; only "master-1" survives.
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].host_name, "master-1");

        let calls = runner.calls.lock().unwrap();
        assert!(!calls.is_empty());
        for (_, args) in calls.iter() {
            for arg in args {
                assert!(!arg.contains("--help"));
            }
        }
    }
}
