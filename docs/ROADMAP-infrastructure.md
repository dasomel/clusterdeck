# Infrastructure Roadmap

This roadmap preserves the development milestones from InfraDeck following its absorption into ClusterDeck ([ADR-0008](adr/0008-vm-inventory-environment-source.md)). Status against the acceptance criteria in the table below, checked against the code:

| Order | Status | Done | Remaining |
| --- | --- | --- | --- |
| 1 | Partial | Identity resolution (Vagrant/backing VM appears once; unresolved cases left without a runtime ID and marked stale) with unit tests | Validation on Apple Silicon macOS against real providers is not recorded |
| 2 | Partial | Disk capacity/usage and creation time (VirtualBox, VMware VMX, Colima data disk); guest IPs where the guest reports them; unknown values stay unset; Vagrant-managed stopped VMs are listed | Stopped standalone Fusion VMs (discovery uses `vmrun list`, which lists running VMs only); per-field source and freshness (only an inventory-level timestamp exists) |
| 5 | Done | Kubernetes detection from kubeconfig server hosts plus a TCP probe of port 6443; no credential extraction; "not detected" and "unknown" kept distinct | Health checks are out of scope |
| 6 | Partial | In-app profile creation from an observed environment (no deep links, no credentials written) | Versioned, credential-free manifest import/export agreed with ClusterDeck is not implemented |

Items 3, 4, 7, and 8 (extended lifecycle actions and safe terminal SSH launch, resource and network conflict analysis, signed native distribution, and expanded provider support for Lima, UTM, Multipass, OrbStack, and Rancher Desktop) remain on ClusterDeck's post-merge infrastructure roadmap.

| Order | Work | Completion criteria |
| --- | --- | --- |
| 1 | Real provider fixtures and identity resolution | Validate on Apple Silicon macOS; Vagrant/backing VM appears once for supported ID formats; unresolved cases explicitly marked |
| 2 | Complete inventory | Stopped Fusion VMs, disk metadata and IPs; source and freshness recorded; unknown values preserved |
| 3 | Lifecycle and SSH | Start/stop/restart capability checks, explicit confirmation, per-machine operation locks, surfaced command failures; SSH opens a supported terminal safely |
| 4 | Resource and network analysis | Unknown allocations reported; start-time memory estimate; subnet/port warnings require observed ownership |
| 5 | Kubernetes discovery | Separate configuration detection from reachability/health; no credential extraction by default |
| 6 | Clusterdeck handoff | Agree and test versioned manifest import with Clusterdeck; credential-free export; no invented deep links |
| 7 | Native distribution | Tauri app signing/notarization and reproducible macOS builds; then document verified installer/Homebrew path |
| 8 | Provider expansion | Lima, UTM, Multipass, OrbStack and Rancher Desktop after the first four adapters are stable |

Before `v0.1.0`: macOS end-to-end acceptance, reliable identities, lifecycle
operation safety, provider installation diagnostics and documented limitations.
Do not publish a release that implies these are already complete.
