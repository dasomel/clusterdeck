//! Local identity of a Vagrant machine that is about to be destroyed (ADR-0012 D14/D15).
//!
//! `vagrant destroy -f <name>` with `VAGRANT_CWD` acts on whatever machine state lives under
//! `<project>/.vagrant`, and Vagrant's resolver lets a *global index id prefix* beat the name.
//! Two hazards follow, both closed here by failing closed:
//!
//! - D14: the machine name must not be a (case-insensitive) prefix of any other index id.
//! - D15: the plan is bound to the machine's own `index_uuid` and provider `id` files, so a project
//!   or `.vagrant` directory replaced at the same path after the confirm no longer matches.

use std::path::{Path, PathBuf};

use crate::services::validate;

const MAX_ID_FILE: u64 = 4096;
const MAX_INDEX_FILE: u64 = 16 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    /// Replaces the Vagrant home directory (the parent of `data/machine-index/index`).
    pub(super) static HOME_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// `$VAGRANT_HOME` (default `~/.vagrant.d`). The destroy env pins this same directory (D17), so
/// the index validated here is the index the child uses.
fn vagrant_home() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(p) = HOME_OVERRIDE.with(|o| o.borrow().clone()) {
        return Ok(p);
    }
    let home = match std::env::var_os("VAGRANT_HOME").filter(|v| !v.is_empty()) {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("cannot locate the Vagrant home")?)
            .join(".vagrant.d"),
    };
    if !home.is_absolute()
        || home
            .to_str()
            .is_none_or(|h| h.chars().any(char::is_control))
    {
        return Err("the Vagrant home is not an absolute path".to_string());
    }
    Ok(home)
}

fn index_path() -> Result<PathBuf, String> {
    Ok(vagrant_home()?
        .join("data")
        .join("machine-index")
        .join("index"))
}

/// The complete Vagrant environment for a destroy (D17): every path Vagrant could take from the
/// environment is pinned to the one this module reads. `project_dir` must already be canonical.
/// The runner drops any other inherited `VAGRANT_*` variable.
pub(super) fn destroy_env(project_dir: &str) -> Result<Vec<(String, String)>, String> {
    let home = vagrant_home()?
        .to_str()
        .ok_or("the Vagrant home is not valid UTF-8")?
        .to_string();
    Ok(vec![
        ("VAGRANT_CWD".to_string(), project_dir.to_string()),
        (
            "VAGRANT_DOTFILE_PATH".to_string(),
            format!("{project_dir}/.vagrant"),
        ),
        ("VAGRANT_VAGRANTFILE".to_string(), "Vagrantfile".to_string()),
        ("VAGRANT_HOME".to_string(), home),
    ])
}

async fn read_capped(path: &Path, cap: u64, what: &str) -> Result<String, String> {
    let md = tokio::fs::metadata(path)
        .await
        .map_err(|_| format!("cannot read {what}"))?;
    if !md.is_file() || md.len() > cap {
        return Err(format!("cannot read {what}"));
    }
    tokio::fs::read_to_string(path)
        .await
        .map_err(|_| format!("cannot read {what}"))
}

/// Reads one identity file, refusing a symlink that leaves `root` (the canonical machine dir).
async fn read_identity_file(
    root: &Path,
    path: &Path,
    what: &str,
    strict: bool,
) -> Result<String, String> {
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| format!("missing {what}"))?;
    if !canonical.starts_with(root) {
        return Err(format!("{what} points outside the project"));
    }
    let value = read_capped(&canonical, MAX_ID_FILE, what).await?;
    let value = value.trim();
    // `index_uuid` is a plain id; the provider id is only compared, never used as argv, and may be
    // a path (VMware stores the .vmx path), so it only has to be printable.
    let charset_ok = if strict {
        value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    } else {
        !value.chars().any(char::is_control)
    };
    if value.is_empty() || !charset_ok {
        return Err(format!("unusable {what}"));
    }
    Ok(value.to_string())
}

/// Ids in Vagrant's global machine index (`machines` keys) and the entries' fields we compare.
async fn read_index() -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let text = read_capped(&index_path()?, MAX_INDEX_FILE, "the Vagrant machine index").await?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| "the Vagrant machine index is not valid JSON".to_string())?;
    match json.get("machines") {
        Some(serde_json::Value::Object(m)) => Ok(m.clone()),
        _ => Err("the Vagrant machine index has no machines map".to_string()),
    }
}

/// Verifies a destroy target and returns its identity string (`index_uuid:provider:provider_id`).
/// `project_dir` must already be canonical; `listed_id` is the (short) id from `global-status`.
pub(super) async fn verify_destroy_identity(
    project_dir: &str,
    machine: &str,
    listed_id: &str,
) -> Result<String, String> {
    let project = Path::new(project_dir);
    let machine_dir = tokio::fs::canonicalize(project.join(".vagrant/machines").join(machine))
        .await
        .map_err(|_| format!("no local Vagrant state for {machine}"))?;
    if !machine_dir.starts_with(project) {
        return Err("local Vagrant state points outside the project".to_string());
    }

    // Vagrant 2.4 treats any provider directory with an `id` file as active, whether or not it
    // has an `index_uuid`, and follows symlinked provider entries. So: a symlink anywhere here is
    // refused, and exactly one directory may carry an `id`.
    let mut providers = Vec::new();
    let mut rd = tokio::fs::read_dir(&machine_dir)
        .await
        .map_err(|_| format!("cannot read local Vagrant state for {machine}"))?;
    while let Ok(Some(e)) = rd.next_entry().await {
        let ft = e
            .file_type()
            .await
            .map_err(|_| format!("cannot read local Vagrant state for {machine}"))?;
        if ft.is_symlink() {
            return Err(format!(
                "symlinked provider entry in the state of {machine}"
            ));
        }
        if ft.is_dir()
            && tokio::fs::symlink_metadata(e.path().join("id"))
                .await
                .is_ok()
        {
            providers.push(e.file_name());
        }
    }
    let [provider] = providers.as_slice() else {
        return Err(format!(
            "expected exactly one active provider for {machine}, found {}",
            providers.len()
        ));
    };
    let provider = provider
        .to_str()
        .filter(|p| validate::is_safe_local_runtime_instance_name(p))
        .ok_or("unsafe provider directory name")?;
    let pdir = machine_dir.join(provider);
    let index_uuid =
        read_identity_file(&machine_dir, &pdir.join("index_uuid"), "index_uuid", true).await?;
    let provider_id =
        read_identity_file(&machine_dir, &pdir.join("id"), "provider machine id", false).await?;

    let index = read_index().await?;
    let entry = index
        .get(&index_uuid)
        .ok_or("the machine's index_uuid is not in the Vagrant machine index")?;
    if !index_uuid.starts_with(listed_id) {
        return Err("the machine's index_uuid does not match the listed Vagrant id".to_string());
    }
    if let Some(name) = entry.get("name").and_then(|n| n.as_str()) {
        if name != machine {
            return Err("the Vagrant machine index names a different machine".to_string());
        }
    }
    if let Some(local) = entry.get("local_data_path").and_then(|n| n.as_str()) {
        let expected = tokio::fs::canonicalize(project.join(".vagrant")).await.ok();
        if tokio::fs::canonicalize(local).await.ok() != expected || expected.is_none() {
            return Err("the Vagrant machine index points at another project".to_string());
        }
    }
    // D14: Vagrant resolves an argument that prefixes a global id as that id, ahead of VAGRANT_CWD.
    let lower = machine.to_lowercase();
    if index
        .keys()
        .any(|id| id != &index_uuid && id.to_lowercase().starts_with(&lower))
    {
        return Err(format!(
            "machine name {machine} is a prefix of another Vagrant machine id; \
             destroy by name could hit the wrong VM — refusing"
        ));
    }
    Ok(format!("{index_uuid}:{provider}:{provider_id}"))
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::collections::BTreeMap;

    thread_local! {
        static ENTRIES: std::cell::RefCell<BTreeMap<String, (String, String)>> =
            const { std::cell::RefCell::new(BTreeMap::new()) };
    }

    /// Full id for a short `global-status` id (Vagrant ids are 32 chars; the list shows 7).
    pub fn full_id(short: &str) -> String {
        format!("{short}{}", "0".repeat(32usize.saturating_sub(short.len())))
    }

    /// Writes `<dir>/.vagrant/machines/<machine>/virtualbox/{index_uuid,id}` and registers the id
    /// in this test thread's machine index (rewritten on disk each time).
    pub fn register(dir: &str, machine: &str, short_id: &str) {
        let full = full_id(short_id);
        let p = Path::new(dir)
            .join(".vagrant/machines")
            .join(machine)
            .join("virtualbox");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("index_uuid"), &full).unwrap();
        std::fs::write(p.join("id"), format!("vm-{short_id}")).unwrap();
        add_index_entry(&full, machine, Some(dir));
    }

    pub fn add_index_entry(full: &str, name: &str, dir: Option<&str>) {
        ENTRIES.with(|e| {
            e.borrow_mut().insert(
                full.to_string(),
                (
                    name.to_string(),
                    dir.map(|d| format!("{d}/.vagrant")).unwrap_or_default(),
                ),
            )
        });
        write_index();
    }

    pub fn index_file() -> PathBuf {
        HOME_OVERRIDE
            .with(|o| o.borrow().clone().unwrap())
            .join("data/machine-index/index")
    }

    pub fn write_index() {
        let home = std::env::temp_dir().join(format!(
            "cd-vhome-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = home.join("data/machine-index/index");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let machines: serde_json::Map<String, serde_json::Value> = ENTRIES.with(|e| {
            e.borrow()
                .iter()
                .map(|(id, (name, local))| {
                    let mut v = serde_json::json!({ "name": name });
                    if !local.is_empty() {
                        v["local_data_path"] = local.clone().into();
                    }
                    (id.clone(), v)
                })
                .collect()
        });
        std::fs::write(
            &path,
            serde_json::json!({ "version": 1, "machines": machines }).to_string(),
        )
        .unwrap();
        HOME_OVERRIDE.with(|o| *o.borrow_mut() = Some(home));
    }
}
