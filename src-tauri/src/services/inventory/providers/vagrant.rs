use std::path::{Path, PathBuf};

use crate::services::inventory::details::{self, parse_vmx};
use crate::services::inventory::model::Machine;
use crate::services::inventory::run_command;
use crate::services::process::{CommandLimits, CommandRunner};
use crate::services::validate;

/// SSH endpoint reported by `vagrant ssh-config`. Every field has passed validate.rs, so it may
/// flow into a Profile (and from there into SSH argv / ssh config) without a further check.
#[derive(Debug, Clone, PartialEq)]
pub struct VagrantSsh {
    pub address: String,
    pub port: u16,
    pub user: String,
    pub identity_file: Option<String>,
}

pub fn find_vmx(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "vmx"))
        .map(|p| {
            std::fs::canonicalize(&p)
                .unwrap_or(p)
                .to_string_lossy()
                .into_owned()
        })
}

/// Absolute, no `..`, no control characters: the only shape of project dir we hand to the
/// filesystem or to `VAGRANT_CWD`. Existence is checked separately (a vanished project is
/// reported "stale", not dropped).
fn is_plain_project_dir(dir: &Path) -> bool {
    dir.is_absolute()
        && !dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && !dir.to_string_lossy().chars().any(char::is_control)
}

/// `vagrant ssh-config <machine>` for the right project: the runner has no cwd support, so the
/// project is selected via `VAGRANT_CWD`. Bounded (timeout + output cap) like every other
/// inventory command. `None` on any failure or on values that do not validate.
pub async fn vagrant_ssh_config(
    runner: &dyn CommandRunner,
    project_dir: &Path,
    machine: &str,
) -> Option<VagrantSsh> {
    if !validate::is_safe_path_segment(machine) || !is_plain_project_dir(project_dir) {
        return None;
    }
    let env = [(
        "VAGRANT_CWD".to_string(),
        project_dir.to_string_lossy().into_owned(),
    )];
    let out = runner
        .run_bounded_with_env(
            "vagrant",
            &["ssh-config".to_string(), machine.to_string()],
            &env,
            CommandLimits::default(),
        )
        .await
        .ok()
        .filter(|o| o.success)?;
    let block = crate::services::local_runtime::parse_ssh_config_blocks(&out.stdout)
        .into_iter()
        .next()?;
    let address = block
        .hostname
        .filter(|h| validate::is_safe_ip_address(h) || validate::is_safe_host_domain(h))?;
    let user = block.user.filter(|u| validate::is_safe_ssh_identifier(u))?;
    let identity_file = block
        .identity_file
        .filter(|i| Path::new(i).is_absolute() && validate::is_safe_ssh_identifier(i));
    Some(VagrantSsh {
        address,
        port: block.port.unwrap_or(22),
        user,
        identity_file,
    })
}

const SUPPORTED: [&str; 3] = ["virtualbox", "vmware_desktop", "vmware_fusion"];

pub(super) async fn discover(runner: &dyn CommandRunner) -> Result<Vec<Machine>, String> {
    let listing = run_command(runner, "vagrant", &["global-status"]).await?;
    let entries = crate::services::local_runtime::parse_vagrant_global_status(&listing);
    let mut result = vec![];
    for entry in entries {
        if !SUPPORTED.contains(&entry.provider.as_str()) {
            continue;
        }
        // R5: Validate machine name, provider, and id before joining into filesystem paths
        if !validate::is_safe_path_segment(&entry.name)
            || !validate::is_safe_path_segment(&entry.provider)
            || !validate::is_safe_path_segment(&entry.id)
        {
            continue;
        }
        let runtime = if entry.provider == "virtualbox" {
            "virtualbox"
        } else {
            "vmware"
        };
        let mut m = Machine::new(runtime, &entry.id, &entry.name);
        m.id = format!("vagrant:{}", entry.id);
        m.orchestrator = Some("vagrant".into());
        m.environment = entry.directory.clone();
        m.state = entry.state.clone();

        // A project that no longer exists (or is not a plain absolute path) keeps its entry
        // with no runtime_id and no file access: reconcile then reports it "stale" instead of
        // the machine silently vanishing from the inventory.
        let project_dir = PathBuf::from(&entry.directory);
        let exists = is_plain_project_dir(&project_dir)
            && tokio::fs::metadata(&project_dir)
                .await
                .is_ok_and(|md| md.is_dir());
        if !exists {
            m.runtime_id = None;
            result.push(m);
            continue;
        }
        fill_from_project(runner, &mut m, &entry, &project_dir).await;
        result.push(m);
    }
    Ok(result)
}

async fn fill_from_project(
    runner: &dyn CommandRunner,
    m: &mut Machine,
    entry: &crate::services::local_runtime::VagrantEntry,
    project_dir: &Path,
) {
    let runtime = m.runtime.clone();
    let machine_dir = project_dir
        .join(".vagrant/machines")
        .join(&entry.name)
        .join(&entry.provider);

    let raw_id = tokio::fs::read_to_string(machine_dir.join("id"))
        .await
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    // Validate id content with path-segment validator before use
    m.runtime_id = raw_id.filter(|id| validate::is_safe_path_segment(id));

    if runtime == "vmware" {
        if let Some(id) = &m.runtime_id {
            let id_dir = machine_dir.join(id);
            m.runtime_id = tokio::task::spawn_blocking(move || find_vmx(&id_dir))
                .await
                .ok()
                .flatten();
        }
    }
    let id_file = machine_dir.join("id");
    m.created_at = tokio::task::spawn_blocking(move || details::created_ms(&id_file))
        .await
        .ok()
        .flatten();

    if let Some(vmx) = m.runtime_id.clone().filter(|_| runtime == "vmware") {
        if let Ok(text) = tokio::fs::read_to_string(&vmx).await {
            let parsed = parse_vmx(&text, &vmx);
            m.cpu = parsed.cpu;
            m.memory_gib = parsed.memory_gib;
            let mut m_clone = m.clone();
            let vmx_buf = PathBuf::from(&vmx);
            if let Ok(done) = tokio::task::spawn_blocking(move || {
                details::apply_vmx(&mut m_clone, &text, &vmx_buf);
                m_clone
            })
            .await
            {
                *m = done;
            }
        }
    }

    // D3 / Item 7: Fallback IP from local_runtime Vagrant static IP or ssh-config
    if m.ips.is_empty() {
        let pdir = project_dir.to_path_buf();
        let mname = entry.name.clone();
        let (static_ip, _) = tokio::task::spawn_blocking(move || {
            crate::services::local_runtime::find_static_vagrant_info(&pdir, &mname)
        })
        .await
        .unwrap_or((None, false));

        if let Some(ip) = static_ip {
            m.ips.push(ip);
        } else if entry.state.eq_ignore_ascii_case("running") {
            if let Some(ssh) = vagrant_ssh_config(runner, project_dir, &entry.name).await {
                // Port is not carried by `ips`; build_target re-reads ssh-config for it.
                if crate::services::inventory::identity::is_reachable_address(&ssh.address) {
                    m.ips.push(ssh.address);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::inventory::model::reconcile;
    use crate::services::process::CommandOutput;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// (args, env) of one bounded ssh-config call.
    type SshCall = (Vec<String>, Vec<(String, String)>);

    #[derive(Default)]
    struct FakeVagrantRunner {
        global_status: String,
        ssh_config: String,
        ssh_calls: Mutex<Vec<SshCall>>,
    }

    fn ok(stdout: &str) -> Result<CommandOutput, String> {
        Ok(CommandOutput {
            stdout: stdout.into(),
            stderr: String::new(),
            success: true,
        })
    }

    #[async_trait]
    impl CommandRunner for FakeVagrantRunner {
        // Anything inventory runs must be bounded; the unbounded entry point is a test failure.
        async fn run(&self, bin: &str, args: &[String]) -> Result<CommandOutput, String> {
            Err(format!("unbounded run: {bin} {args:?}"))
        }
        async fn run_bounded(
            &self,
            bin: &str,
            args: &[String],
            _limits: CommandLimits,
        ) -> Result<CommandOutput, String> {
            if bin == "vagrant" && args.first().map(String::as_str) == Some("global-status") {
                return ok(&self.global_status);
            }
            Err(format!("unexpected call: {bin} {args:?}"))
        }
        async fn run_bounded_with_env(
            &self,
            bin: &str,
            args: &[String],
            env: &[(String, String)],
            _limits: CommandLimits,
        ) -> Result<CommandOutput, String> {
            if bin == "vagrant" && args.first().map(String::as_str) == Some("ssh-config") {
                self.ssh_calls
                    .lock()
                    .unwrap()
                    .push((args.to_vec(), env.to_vec()));
                return ok(&self.ssh_config);
            }
            Err(format!("unexpected env call: {bin} {args:?}"))
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cd-vagrant-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn status_for(dir: &Path) -> String {
        format!(
            "id       name    provider   state   directory\n\
             ------------------------------------------------\n\
             1234567  node-1  virtualbox running {}\n",
            dir.display()
        )
    }

    #[tokio::test]
    async fn vagrant_fallback_ip_from_vagrantfile_when_provider_gives_none() {
        let dir = temp_dir("ip");
        std::fs::write(
            dir.join("Vagrantfile"),
            "Vagrant.configure(\"2\") do |config|\n  config.vm.define \"node-1\" do |node|\n    node.vm.network \"private_network\", ip: \"192.168.56.99\"\n  end\nend\n",
        )
        .unwrap();
        let runner = FakeVagrantRunner {
            global_status: status_for(&dir),
            ..Default::default()
        };

        let machines = discover(&runner).await.unwrap();
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].ips, vec!["192.168.56.99"]);
        assert!(runner.ssh_calls.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ssh_config_fallback_runs_in_project_via_vagrant_cwd_and_keeps_all_fields() {
        let dir = temp_dir("cwd");
        let runner = FakeVagrantRunner {
            ssh_config: "Host node-1\n  HostName 192.168.56.7\n  User ops\n  Port 2200\n  IdentityFile \"/keys/node-1\"\n".into(),
            ..Default::default()
        };

        let ssh = vagrant_ssh_config(&runner, &dir, "node-1").await.unwrap();
        assert_eq!(
            ssh,
            VagrantSsh {
                address: "192.168.56.7".into(),
                port: 2200,
                user: "ops".into(),
                identity_file: Some("/keys/node-1".into()),
            }
        );
        {
            let calls = runner.ssh_calls.lock().unwrap();
            assert_eq!(calls[0].0, vec!["ssh-config", "node-1"]);
            assert_eq!(
                calls[0].1,
                vec![(
                    "VAGRANT_CWD".to_string(),
                    dir.to_string_lossy().into_owned()
                )]
            );
        }

        // Hostile output / inputs never validate.
        let hostile = FakeVagrantRunner {
            ssh_config: "Host x\n  HostName -oProxyCommand=x\n  User ops\n".into(),
            ..Default::default()
        };
        assert!(vagrant_ssh_config(&hostile, &dir, "node-1").await.is_none());
        assert!(vagrant_ssh_config(&runner, &dir, "../x").await.is_none());
        assert!(vagrant_ssh_config(&runner, Path::new("rel/dir"), "node-1")
            .await
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_project_dir_keeps_machine_and_reconcile_marks_it_stale() {
        let gone = std::env::temp_dir().join("clusterdeck_nonexistent_project_12345");
        let runner = FakeVagrantRunner {
            global_status: status_for(&gone),
            ..Default::default()
        };

        let machines = discover(&runner).await.unwrap();
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].state, "running");
        let reconciled = reconcile(machines, &["virtualbox"]);
        assert_eq!(reconciled.len(), 1);
        assert_eq!(reconciled[0].state, "stale");
    }

    #[test]
    fn unsafe_ids_and_dirs_are_rejected() {
        for bad in ["../../etc", "/abs", "a/b", "safe\0evil"] {
            assert!(!validate::is_safe_path_segment(bad), "{bad}");
        }
        assert!(validate::is_safe_path_segment("abc-123_456"));
        assert!(!is_plain_project_dir(Path::new("relative/dir")));
        assert!(!is_plain_project_dir(Path::new("/var/log/../etc")));
        assert!(is_plain_project_dir(Path::new("/var/log")));
    }
}
