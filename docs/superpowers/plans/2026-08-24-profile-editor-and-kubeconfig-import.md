# Profile Editor, Bootstrap Password Input, kubeconfig Import Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. **Dispatch note:** this project's workers are `agy` lanes, not native Claude subagents.

**Goal:** Close three real usability gaps confirmed with the user: (1) the Connect screen has no way to supply an SSH bootstrap password, so the already-implemented password-bootstrap backend path can never be triggered from the GUI; (2) there is no way to add/edit a profile's hosts/bastion/kubeconfig/bootstrap settings from the GUI — `profiles.yaml` must be hand-edited; (3) there is no way to pull context/server metadata out of an existing local `~/.kube/config` to help fill in a new profile's kubeconfig section.

**Architecture:** A new read-only backend service (`services/kube_import.rs`) parses the user's local `~/.kube/config` (never copies credential material into ClusterDeck's own storage — only context/cluster/user *names* and the cluster `server` URL are returned). A new `ProfileEditor` React component becomes the single place that creates and edits `Profile` objects, wired to the already-existing `saveProfile`/`deleteProfile` commands. The Connect screen gets one optional password field wired to `connect_profile`'s already-existing `bootstrap_password` parameter — no backend change needed there, this is a pure frontend gap.

**Tech Stack:** Same Rust/Tauri/React/TypeScript stack. No new Rust dependency (`serde_yaml` is already present). No new frontend dependency (react/lucide-react already present).

**Spec:** User decisions confirmed 2026-08-24: (1) add an SSH password field to the Connect screen; (2) build a GUI Profile Editor for the host list (create/edit/delete a profile's hosts, bastion, bootstrap policy, kubeconfig source, `manage_hosts_file`); (3) `kube/config` extraction means importing existing local kubeconfig *context metadata* to help pre-fill the Profile Editor's kubeconfig section — it does not mean auto-creating a fully working SSH-based profile from a local kubeconfig, since a local kubeconfig carries no SSH host/credential information and ClusterDeck's whole model is SSH-based remote access. Import is a form-filling convenience, not profile auto-generation.

## Global Constraints

- Never write certificate/token/key material from the user's real `~/.kube/config` into any ClusterDeck-owned file. The import feature returns only `context name`, `cluster name`, `user name`, and the cluster's `server` URL string — nothing else from that file ever leaves the read.
- Never persist a bootstrap password to disk anywhere (not `profiles.yaml`, not `state.json`, not `localStorage`). It lives only in React component state for the duration of one Connect action and is discarded after.
- Reuse the existing "Patch Panel" design tokens/classes from `src/styles.css` (`--bg-elevated`, `--border`, `.panel-card`, `.primary-button`, `.secondary-button`, `.mono`, etc.) for all new UI — do not introduce a second visual language. Read `src/styles.css` and `src/App.tsx` in full before writing any new component.
- The `ProfileEditor` must produce a `Profile` object with every field the existing `Profile` TypeScript type in `src/api/tauri.ts` requires (`id`, `name`, `hosts`, `bastion`, `bootstrap`, `kubeconfig`, `manage_hosts_file`) — check that file's current shape first, it may have evolved since this plan was written.
- `pnpm exec tsc --noEmit` and `pnpm build` must pass. `cargo fmt --all -- --check` / `cargo clippy --all-targets --all-features -- -D warnings` / `cargo test --all-targets --all-features` (i.e. `make verify`'s Rust steps) must pass for the backend task.

---

## File Structure

```text
src-tauri/src/
  services/
    kube_import.rs          # NEW: list_local_kube_contexts (pure parse of ~/.kube/config)
    mod.rs                   # MODIFY: + pub mod kube_import;
  commands/
    kube_import.rs           # NEW: list_local_kube_contexts Tauri command
    mod.rs                    # MODIFY: + pub mod kube_import;
  lib.rs                      # MODIFY: register the new command
src/
  api/tauri.ts                 # MODIFY: LocalKubeContext type + listLocalKubeContexts()
  components/
    ProfileEditor.tsx           # NEW: create/edit modal for a full Profile
  App.tsx                        # MODIFY: wire "Add profile" + a per-card "Edit" affordance to ProfileEditor; add Connect-screen password field
  styles.css                      # MODIFY: a handful of new rules for the editor modal/form, reusing existing tokens
```

## Global Interfaces

```rust
// services/kube_import.rs
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocalKubeContext {
    pub context_name: String,
    pub cluster_name: String,
    pub user_name: String,
    pub server: String,
}

pub fn parse_kube_contexts(raw_yaml: &str) -> Result<Vec<LocalKubeContext>, String>;
// Pure function: parses raw_yaml as serde_yaml::Value, iterates `contexts[]`, and for each
// entry resolves `context.cluster`/`context.user` by name against `clusters[]`/`users[]`,
// producing one LocalKubeContext per context entry. A context whose referenced cluster/user
// name isn't found is skipped (not an error for the whole call -- one broken entry in a
// large personal kubeconfig must not block importing the rest). Returns Err only if the
// top-level YAML doesn't parse at all or `contexts` isn't a sequence.

pub fn read_local_kubeconfig_path() -> std::path::PathBuf;
// Returns $KUBECONFIG if set (first path if it's a colon-separated list, matching kubectl's
// own convention of using the first file for reads), else $HOME/.kube/config.

pub fn list_local_kube_contexts() -> Result<Vec<LocalKubeContext>, String>;
// Reads read_local_kubeconfig_path() via std::fs::read_to_string (Ok(vec![]) if the file
// doesn't exist -- this is a normal, common case, not an error) and calls parse_kube_contexts.
```

```rust
// commands/kube_import.rs
#[tauri::command]
pub fn list_local_kube_contexts_cmd() -> Result<Vec<crate::services::kube_import::LocalKubeContext>, String>;
```

```typescript
// src/api/tauri.ts additions
export type LocalKubeContext = {
  context_name: string;
  cluster_name: string;
  user_name: string;
  server: string;
};
// api.listLocalKubeContexts: () => invoke<LocalKubeContext[]>('list_local_kube_contexts_cmd')
```

---

### Task 1: `kube_import` backend service + command

**Files:**
- Create: `src-tauri/src/services/kube_import.rs`
- Create: `src-tauri/src/commands/kube_import.rs`
- Modify: `src-tauri/src/services/mod.rs`, `src-tauri/src/commands/mod.rs`, `src-tauri/src/lib.rs`
- Test: `#[cfg(test)]` in `services/kube_import.rs`

**Interfaces:** produces `LocalKubeContext`, `parse_kube_contexts`, `read_local_kubeconfig_path`, `list_local_kube_contexts` (services), `list_local_kube_contexts_cmd` (command) — see Global Interfaces.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: colima
    cluster:
      server: https://127.0.0.1:56993
  - name: prod
    cluster:
      server: https://prod.example.invalid:6443
contexts:
  - name: colima
    context:
      cluster: colima
      user: colima
  - name: prod-ctx
    context:
      cluster: prod
      user: prod-user
  - name: broken-ctx
    context:
      cluster: does-not-exist
      user: nobody
current-context: colima
users:
  - name: colima
    user: {}
  - name: prod-user
    user: {}
"#;

    #[test]
    fn parse_kube_contexts_resolves_cluster_and_user_by_name() {
        let contexts = parse_kube_contexts(SAMPLE).unwrap();
        let colima = contexts.iter().find(|c| c.context_name == "colima").unwrap();
        assert_eq!(colima.cluster_name, "colima");
        assert_eq!(colima.user_name, "colima");
        assert_eq!(colima.server, "https://127.0.0.1:56993");
    }

    #[test]
    fn parse_kube_contexts_skips_context_with_unresolvable_cluster() {
        let contexts = parse_kube_contexts(SAMPLE).unwrap();
        assert!(!contexts.iter().any(|c| c.context_name == "broken-ctx"));
        // the two resolvable contexts must still come through
        assert_eq!(contexts.len(), 2);
    }

    #[test]
    fn parse_kube_contexts_errors_on_garbage_input() {
        assert!(parse_kube_contexts("not: [valid, yaml: at all: :::").is_err());
    }

    #[test]
    fn read_local_kubeconfig_path_defaults_to_home_dot_kube_config() {
        // Only assert the fallback shape, don't depend on real $HOME contents.
        let path = read_local_kubeconfig_path();
        assert!(path.to_string_lossy().ends_with(".kube/config") || std::env::var("KUBECONFIG").is_ok());
    }
}
```

- [ ] **Step 2: Run to verify failure.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::kube_import` — Expected: FAIL.
- [ ] **Step 3: Implement `parse_kube_contexts`** using `serde_yaml::Value` (mirror the pattern already used in `services/kubeconfig.rs::normalize` for navigating `clusters[]`/`contexts[]`/`users[]` — read that file first for the idiom this codebase already uses). For each `contexts[i]`, read `.context.cluster` and `.context.user` names, find the matching entries in `clusters[]`/`users[]` by `.name`, and if both resolve, read `clusters[j].cluster.server` as a string and push a `LocalKubeContext`. Skip (don't error) any context that fails to resolve.
- [ ] **Step 4: Implement `read_local_kubeconfig_path`** and `list_local_kube_contexts` per the Global Interfaces spec.
- [ ] **Step 5: Add `pub mod kube_import;` to `services/mod.rs`.**
- [ ] **Step 6: Run to verify pass.** Run: `cargo test --manifest-path src-tauri/Cargo.toml services::kube_import` — Expected: PASS.
- [ ] **Step 7: Implement `commands/kube_import.rs`**

```rust
#[tauri::command]
pub fn list_local_kube_contexts_cmd(
) -> Result<Vec<crate::services::kube_import::LocalKubeContext>, String> {
    crate::services::kube_import::list_local_kube_contexts()
}
```

Add `pub mod kube_import;` to `commands/mod.rs`, register `commands::kube_import::list_local_kube_contexts_cmd` in `lib.rs`'s `generate_handler!` list (read the current real list first, it has grown since this plan was written -- add to it, don't replace it).

- [ ] **Step 8: Full verification**

```bash
cargo test --manifest-path src-tauri/Cargo.toml
cargo check --manifest-path src-tauri/Cargo.toml
cd src-tauri && cargo fmt --all && cd ..
cd src-tauri && cargo clippy --all-targets --all-features -- -D warnings && cd ..
```

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/services/kube_import.rs src-tauri/src/commands/kube_import.rs src-tauri/src/services/mod.rs src-tauri/src/commands/mod.rs src-tauri/src/lib.rs
git commit -m "feat(backend): read-only import of local ~/.kube/config context metadata"
```

---

### Task 2: frontend API client additions

**Files:**
- Modify: `src/api/tauri.ts`

**Interfaces:** consumes Task 1's `list_local_kube_contexts_cmd`; produces `LocalKubeContext` type + `api.listLocalKubeContexts` (see Global Interfaces) for Task 3 to consume. Also verify (do not necessarily change unless missing) that `api.connectProfile` already accepts an optional password argument matching `connect_profile`'s `bootstrap_password: Option<String>` Rust parameter -- read the current file first; if it's already there from earlier work, this task only adds the kubeconfig-import pieces.

- [ ] **Step 1: Add the `LocalKubeContext` type and `listLocalKubeContexts` method** to `src/api/tauri.ts` per Global Interfaces.
- [ ] **Step 2: Confirm `Profile` type already has `manage_hosts_file: boolean`** (added in an earlier plan) -- if for any reason it's missing, add it now, since Task 4's ProfileEditor depends on it.
- [ ] **Step 3: Type-check.** Run: `pnpm exec tsc --noEmit -p tsconfig.json` — Expected: PASS (App.tsx not yet using the new exports is fine, no unused-export errors from a plain `tsc --noEmit` run).
- [ ] **Step 4: Commit**

```bash
git add src/api/tauri.ts
git commit -m "feat(frontend): add kubeconfig-import API client method"
```

---

### Task 3: `ProfileEditor` component (create/edit a full Profile)

**Files:**
- Create: `src/components/ProfileEditor.tsx`
- Modify: `src/styles.css` (editor-specific rules only, reusing existing tokens)

**Interfaces:**
- Consumes: `Profile`, `Host`, `Bastion`, `BootstrapPolicy`, `KubeconfigSource`, `LocalKubeContext`, `api.saveProfile`, `api.listLocalKubeContexts` (Tasks 1–2).
- Produces:
  ```typescript
  // src/components/ProfileEditor.tsx
  export type ProfileEditorProps = {
    initial: Profile | null; // null = creating a new profile; non-null = editing an existing one
    onClose: () => void;      // called after a successful save OR an explicit cancel
    onSaved: (profile: Profile) => void; // called with the saved Profile right after api.saveProfile resolves
  };
  export default function ProfileEditor(props: ProfileEditorProps): JSX.Element;
  ```
  This is the exact component shape Task 4 imports and renders.

Read `src/App.tsx` and `src/styles.css` in full before starting -- this component must look and feel like it belongs in the existing Patch Panel design (card backgrounds, borders, monospace for technical fields, focus-visible rings), not like a bolted-on generic form.

- [ ] **Step 1: Build the form state and field layout.**

Local component state: a mutable draft `Profile`-shaped object seeded from `props.initial` when editing, or a sensible empty default when creating (empty `id`/`name`, `hosts: []`, `bastion: null`, `bootstrap: { enabled: false, retries: 3, retry_delay_secs: 5 }`, `kubeconfig: null`, `manage_hosts_file: false`). Render as a modal overlay (a fixed-position full-viewport dim backdrop + a centered `.panel-card`-styled panel, new CSS rules for `.modal-backdrop`/`.modal-panel` reusing `var(--bg-elevated)`/`var(--border)`/`var(--shadow-card)`) containing:
  - Profile name (text input) and, only when creating (`props.initial === null`), an id field (text input, helper text: "lowercase letters, numbers, `-`, `_` only" -- matches the backend's `is_safe_profile_id` rule from `services/validate.rs`; when editing, show the id read-only since it's the YAML map key and renaming it is out of scope for this task).
  - A dynamic **Hosts** list: one row per host with inputs for name/address/port/user/identity_file, an inline "Remove" button per row, and an "Add host" button that appends a blank `Host` row (`{ name: '', address: '', port: 22, user: '', identity_file: null }`).
  - A **Bastion** section: a checkbox/toggle "Use a bastion host" that shows/hides one row of bastion fields (name/address/port/user/identity_file) when checked, and sets `bastion: null` when unchecked.
  - A **Bootstrap** section: a checkbox "Enable password bootstrap" bound to `bootstrap.enabled`, and (only when checked) two number inputs for `retries`/`retry_delay_secs`.
  - A **Kubeconfig** section: a checkbox "Fetch kubeconfig from this profile" that toggles between `kubeconfig: null` and a populated `KubeconfigSource`; when checked, inputs for `remote_path` (default placeholder `/etc/kubernetes/admin.conf`), a `control_plane` `<select>` populated from the current draft's `hosts` names (so it can only ever reference a host that's actually in the list), `local_path` (auto-computed display-only text showing `~/.clusterdeck/kubeconfigs/<id>.yaml`, not directly editable -- this mirrors how the backend actually resolves it via `ClusterDeckPaths`, so let the derived value be authoritative rather than user-typed), and `context` (text input, default to the profile id when the field is first shown and still empty). Below these, an **Import from local kubeconfig** control: a button "Load contexts" that calls `api.listLocalKubeContexts()` and renders the results as a `<select>` of `"<context_name> (<server>)"` options; picking one sets the `context` field to the picked `context_name` (does NOT touch `remote_path`/`control_plane`, which remain the user's own SSH-based values -- per Global Constraints, a local kubeconfig has no SSH/host information to import beyond the context name and server string shown for reference).
  - A **`manage_hosts_file`** checkbox, with the exact `/etc/hosts` marker-block explanation from `docs/ARCHITECTURE.md` §7.1 as its helper text (one sentence, not the whole doc section).
  - Footer buttons: "Cancel" (calls `props.onClose()` without saving) and "Save" (see Step 2).

- [ ] **Step 2: Implement Save.**

```typescript
const handleSave = async () => {
  // client-side guard before calling the backend: non-empty name, non-empty id (create mode),
  // at least one host with non-empty name+address+user. Show a simple inline error string
  // (a <p> under the footer) rather than blocking silently -- do not use window.alert/confirm.
  try {
    await api.saveProfile(draft);
    props.onSaved(draft);
    props.onClose();
  } catch (err) {
    setSaveError(String(err));
  }
};
```

- [ ] **Step 3: Type-check in isolation.** Run: `pnpm exec tsc --noEmit -p tsconfig.json` — Expected: no errors referencing `ProfileEditor.tsx` (App.tsx not yet importing it is fine, addressed in Task 4).
- [ ] **Step 4: Add the editor-specific CSS rules to `styles.css`** (`.modal-backdrop`, `.modal-panel`, `.form-row`, `.form-label`, `.host-row-editable`, `.remove-row-button`, etc.) -- reuse `var(--bg)`, `var(--bg-elevated)`, `var(--border)`, `var(--text-primary)`, `var(--text-secondary)`, `var(--accent)`, `var(--font-mono)` throughout; do not introduce new hardcoded colors.
- [ ] **Step 5: Commit**

```bash
git add src/components/ProfileEditor.tsx src/styles.css
git commit -m "feat(frontend): add ProfileEditor for creating and editing profiles"
```

---

### Task 4: wire `ProfileEditor` and the Connect-screen password field into `App.tsx`

**Files:**
- Modify: `src/App.tsx`

**Interfaces:** consumes `ProfileEditor` (Task 3), `api.connectProfile` (already exists, confirm its exact current signature by reading the file before use).

- [ ] **Step 1: Read the current real `App.tsx`** (it has evolved through several prior fixes -- concurrent host probing awareness doesn't affect the frontend, but the exact current state shape/handlers do) before editing.
- [ ] **Step 2: Add editor open/close state**

```typescript
const [editorState, setEditorState] = useState<{ open: boolean; profile: Profile | null }>({ open: false, profile: null });
```

- [ ] **Step 3: Wire the "Add profile" button** (currently `disabled` with a deferral tooltip, per an earlier commit) to open the editor in create mode: remove the `disabled`/`title` attributes added earlier, add `onClick={() => setEditorState({ open: true, profile: null })}`.
- [ ] **Step 4: Add a small "Edit" affordance per profile card** (e.g. a `Pencil` icon from `lucide-react`, `size={14}`, placed in `.profile-card-top` next to the existing status icon; `onClick` must call `event.stopPropagation()` before opening the editor, since the card's own `onClick` selects the profile -- these are two different actions on the same row and must not fire both) that calls `setEditorState({ open: true, profile })`.
- [ ] **Step 5: Render `ProfileEditor` conditionally**

```tsx
{editorState.open && (
  <ProfileEditor
    initial={editorState.profile}
    onClose={() => setEditorState({ open: false, profile: null })}
    onSaved={() => { loadProfiles(); }}
  />
)}
```

- [ ] **Step 6: Add the bootstrap password field to the Connect screen.**

In the `hero-card` section, only when `selected?.bootstrap.enabled` is true, render a password `<input type="password">` bound to a new `const [bootstrapPassword, setBootstrapPassword] = useState('')` right above (or beside) the existing "Connect / Sync" button, with a short label ("SSH bootstrap password" or similar, matching existing label styling). Clear `bootstrapPassword` back to `''` immediately after `connect()` resolves (success or failure) -- it must never linger in state longer than one connect action, per Global Constraints. Update the `connect` function:

```typescript
const connect = async () => {
  if (!selected) return;
  setConnecting(true);
  try {
    const result = await api.connectProfile(selected.id, bootstrapPassword || undefined);
    setLastResult(result);
  } catch (err) {
    setLoadError(String(err));
  } finally {
    setConnecting(false);
    setBootstrapPassword('');
  }
};
```

(Adjust the exact `api.connectProfile` call shape to whatever its real current signature is, confirmed in Step 1 -- this plan's earlier work already added a `bootstrapPassword` parameter to it, so this should be a straightforward wire-up, not a new API method.)

- [ ] **Step 7: Full verification**

```bash
pnpm exec tsc --noEmit -p tsconfig.json
pnpm build
```

- [ ] **Step 8: Commit**

```bash
git add src/App.tsx
git commit -m "feat(frontend): wire ProfileEditor (add/edit) and bootstrap password input into the main screen"
```

---

### Task 5: manual verification + docs note (not delegated to agy — requires visual/GUI judgment)

- [ ] Launch the real app (`pnpm tauri dev`), screenshot: (a) the profile list with the new Edit icon visible, (b) the ProfileEditor open in create mode, (c) the ProfileEditor open in edit mode for an existing profile with hosts populated, (d) the "Import from local kubeconfig" dropdown populated with real contexts from the developer machine's actual `~/.kube/config`, (e) the Connect screen showing the bootstrap password field for a profile with `bootstrap.enabled: true`.
- [ ] Confirm saving a new profile through the editor actually appears in `~/.clusterdeck/profiles.yaml` with the right shape (`cat` the file after saving).
- [ ] Confirm editing an existing profile's host list and saving updates that same YAML entry rather than creating a duplicate.
- [ ] Report explicitly which of the above were visually confirmed vs. not, per this project's evidence rules -- do not claim UI behavior "works" from `tsc`/`pnpm build` passing alone.

## Self-Review Notes

- **Spec coverage:** bootstrap password input — Task 4 Step 6. Profile/host GUI CRUD — Task 3 + Task 4 Steps 3–5. kubeconfig import as a form-filling helper (not full profile auto-generation, per the user's clarified scope) — Task 1 (backend) + Task 3's "Import from local kubeconfig" control.
- **Deferred / explicitly out of scope:** renaming an existing profile's `id` (would require migrating its `~/.clusterdeck/ssh/<id>.conf` and `kubeconfigs/<id>.yaml` file names too — a bigger change than this plan's scope); multi-context batch import; editing `manage_hosts_file`'s live `/etc/hosts` state from the editor (the checkbox only sets the flag for the next `connect_profile` run, it does not itself trigger a write).
- **Type consistency check performed:** `ProfileEditorProps` (Task 3) is the exact shape Task 4 renders `<ProfileEditor ... />` with. `LocalKubeContext` (Task 1 Rust struct, Task 2 TS type) field names match exactly (`context_name`, `cluster_name`, `user_name`, `server`) since Tauri serializes struct field names as-is with no case conversion for return values (only command *arguments* get camelCase-to-snake_case handling, not return payloads) — verify this assumption holds by checking how an existing struct like `ConnectionResult` is already consumed in `App.tsx` (its fields are read in `snake_case` there, e.g. `lastResult.verification.ssh`, confirming return payloads keep Rust's field names verbatim).
