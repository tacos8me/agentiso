# agentiso Product Assessment

Written 2026-02-18. Frank evaluation, not marketing.

## The Core Bet

agentiso gives AI agents a real Linux VM (QEMU microvm with KVM) exposed as MCP tools. Each workspace is a ZFS clone of a base Alpine image, which means creation is instant (CoW clone, not a copy), snapshots are free until the workspace diverges, and forking a workspace into two independent copies is a metadata operation that takes milliseconds. The isolation boundary is a hardware VM, not a container namespace -- a compromised guest cannot escape to the host without a KVM exploit. This is "local E2B": the self-hosted version of cloud execution sandboxes, running on your own hardware, with no per-minute billing and no internet dependency. The tradeoff is setup complexity.

## When It's Worth It (Be Specific)

**Running untrusted or generated code.** If an agent is executing code it wrote, code from the internet, or user-supplied scripts, the VM boundary matters. A `rm -rf /` inside the workspace destroys a ZFS clone, not your host. Docker provides namespace isolation but shares the host kernel; a container escape is a realistic attack vector. A KVM escape is orders of magnitude harder.

**Agent doing filesystem-wide operations.** Mass renames, recursive deletes, reorganizing directory trees, installing system packages -- operations where a mistake is expensive and hard to reverse. Inside an agentiso workspace, the agent can snapshot before the operation and rollback in under a second if it goes wrong. This is faster than git stash and works on non-git content (databases, binary files, system configs).

**Iterative experimentation with rollback.** The snapshot/restore cycle is the killer feature for agent workflows. Create a checkpoint, try an approach, evaluate the result, roll back if it fails, try a different approach. This is what git stash wishes it was -- it works at the block level, includes file permissions, installed packages, running processes (with memory snapshots), everything.

**Parallel approach testing via fork.** This is genuinely unique among local tools. Snapshot a workspace, fork it into N independent copies, run different approaches in parallel, compare results, keep the winner. No other local sandbox offers this. Docker can do it but requires rebuilding images; agentiso forks are instant ZFS clones.

**Containing weaker local models.** Devstral, MiniMax M2.5, Step 3.5 Flash -- these models are capable but make mistakes. They will occasionally run destructive commands, write to wrong paths, or misunderstand file operations. Inside a VM, those mistakes are contained. The blast radius of any single bad command is one disposable ZFS clone. This alone justifies the setup cost if you're running local models on code tasks.

## When It's Overkill

**Reading, analyzing, or reviewing code.** If the agent is only reading files and producing text output, there is nothing to isolate against. Bare filesystem access (or even just piping files into the model) is simpler and faster.

**Generating text, writing docs, answering questions.** No execution means no risk. No sandbox needed.

**Working in a git repo where you trust the model.** If you're using Claude Sonnet on a git-tracked project and you trust it not to run `rm -rf /`, git's own revert/reset is sufficient rollback. The VM overhead buys you little.

**One-off safe commands.** `ls`, `cat`, `grep`, `find`, `wc` -- read-only commands that cannot damage anything. Running these through a VM adds latency for zero safety benefit.

**Anything where boot latency is unacceptable.** Cold boot is 5-30 seconds. If your workflow needs sub-second tool response on the first call, agentiso without a warm pool is not the right choice. (The warm pool feature exists but is not battle-tested yet.)

## The Local Model Reality Check

The models in this deployment (Devstral, MiniMax M2.5, Step 3.5 Flash) are capable but not Claude Sonnet. Honest assessment of how they'll interact with agentiso:

**24 tools is a large surface area.** A model that struggles with tool selection will call the wrong tool, pass wrong parameters, or get confused by tools it doesn't need. The full tool list includes lifecycle management, file ops, snapshots, networking, background jobs, and port forwarding. A weaker model does not need most of these.

**The 7 tools that cover 90% of use cases:**

| Tool | What it does |
|------|-------------|
| `workspace_create` | Boot a fresh VM |
| `exec` | Run a shell command |
| `file_write` | Write a file |
| `file_read` | Read a file |
| `file_list` | List directory contents |
| `snapshot_create` | Checkpoint current state |
| `workspace_destroy` | Clean up when done |

Recommendation: expose only these 7 initially. Add `snapshot_restore` and `workspace_fork` once the model demonstrates it can track workspace IDs reliably. The remaining tools (port_forward, network_policy, exec_background/exec_poll, file_upload/file_download, file_edit) are power-user features that a weaker model will misuse more often than it uses correctly.

**exec_background/exec_poll is especially hard.** The model must: (1) call exec_background, (2) remember the returned job_id, (3) call exec_poll with that job_id, possibly multiple times, (4) interpret the running/finished status. This is a multi-turn stateful workflow. Models that struggle with tool chaining will lose track of the job_id or poll forever. Just use `exec` with a longer timeout instead.

**The isolation value persists even with bad tool usage.** If a model calls the wrong tool or passes bad parameters, the worst case inside the VM is a failed command or a corrupted workspace. The host is untouched. This is the fundamental value proposition -- you don't need the model to be perfect, you need its mistakes to be cheap.

**System prompt matters more than tool count.** For local models, write a system prompt that says: "You have a Linux VM. Use `workspace_create` to start it. Use `exec` to run commands. Use `file_write` and `file_read` for files. Use `snapshot_create` before risky operations. Use `workspace_destroy` when done." This is more effective than relying on the model to discover tool semantics from JSON schemas.

## Overhead Reality

| Resource | Cost | Notes |
|----------|------|-------|
| Cold boot | 5-30s | Depends on host load. OpenRC init adds ~3s over a bare kernel. Warm pool (if enabled) cuts first workspace to ~1s. |
| RAM per workspace | 512 MB default | Configurable 128MB-8GB. 5 concurrent workspaces = 2.5 GB. On a 64GB+ GPU box, this is negligible. |
| Disk per workspace | ~20 MB initially | ZFS CoW -- the clone only stores deltas from the base image. A workspace that installs 500MB of packages costs 500MB. One that runs `echo hello` costs nearly nothing. |
| Snapshot cost | Near-zero | ZFS snapshots are metadata operations. Cost grows only as the workspace diverges from the snapshot point. |
| Fork cost | Near-zero + boot time | ZFS clone is instant. Booting the new VM takes the same 5-30s. |
| Per-exec latency | ~10-50ms | vsock RTT over virtio-socket. Negligible compared to model inference time. |
| CPU per workspace | 2 vCPUs default | These are shared with the host via KVM. No dedicated cores unless you pin them. |
| Host disk (base) | ~200 MB | Alpine base image with dev tools. |
| Host RAM (daemon) | ~10-30 MB | The agentiso server process itself. |

**The real cost is setup, not runtime.** You need ZFS, QEMU, KVM, a bridge interface, nftables, and the Rust toolchain. The `setup-e2e.sh` script automates most of it, but it's still a non-trivial host configuration. Once set up, the marginal cost of each workspace is minimal.

## Versus Alternatives

| | Bare bash | Docker | agentiso | E2B / Modal |
|---|---|---|---|---|
| **Isolation** | None | Namespace (shared kernel) | KVM (hardware VM) | KVM / gVisor |
| **Escape difficulty** | N/A | Container escape (known CVEs yearly) | KVM escape (rare, high-value) | Same as agentiso |
| **Rollback** | `git reset` (code only) | Rebuild image (slow) | ZFS snapshot (instant, includes everything) | Paid feature / rebuild |
| **Fork/clone** | No | `docker commit` + `run` (slow, ~seconds) | ZFS clone (instant, ~milliseconds) | Paid / slow |
| **Works offline** | Yes | Yes | Yes | No |
| **Per-minute cost** | Free | Free | Free | $0.16-0.50/min |
| **Setup effort** | Zero | `apt install docker.io` | ZFS + QEMU + KVM + bridge + nftables + build | `pip install e2b` |
| **MCP integration** | Write your own | Write your own | Built-in (24 tools) | Official SDK |
| **Local models** | Yes | Yes | Yes | API-only (no local) |
| **Guest OS** | Host OS | Any Linux (shared kernel) | Any Linux (own kernel) | Ubuntu |
| **Networking** | Host network | Bridge/overlay | Per-VM TAP + nftables isolation | Cloud networking |
| **Startup time** | 0 | ~1s | 5-30s (cold) / ~1s (warm pool) | 3-10s |

**Docker is the closest practical alternative.** If you already use Docker and don't need instant rollback, snapshot trees, or fork-based parallelism, Docker is simpler. The isolation is weaker (container escapes happen) but adequate for most local development. agentiso's advantage is the ZFS snapshot/fork/rollback primitive and the stronger VM boundary.

**E2B is the closest feature-equivalent.** It offers similar isolation with zero setup, but costs money per minute, requires internet, and doesn't work with local models. If you're already paying for cloud inference, E2B might be simpler. If you're running local models on your own GPU box, E2B is not an option.

## What's Not Ready

Being honest about gaps (from the readiness eval):

- **9 of 24 tools are untested end-to-end.** Most critically, `snapshot_restore` -- the restore half of the checkpoint/rollback workflow -- has never been exercised in integration tests. It probably works, but "probably" is not "tested."
- **No orphan cleanup on restart.** If the server crashes, stale QEMU processes, ZFS datasets, and TAP devices are left behind. You have to clean them up manually.
- **No state recovery across restart.** Workspaces are tracked in memory. Restart the server, lose all session tracking. The VMs still exist but the server doesn't know about them.
- **Warm pool is not battle-tested.** The config exists, the code exists, but it hasn't been stress-tested under real workloads.
- **Sequential VM shutdown.** Stopping 20 workspaces takes up to 180 seconds (9s timeout each, sequentially). This is a problem at scale but irrelevant for 1-5 concurrent workspaces.

None of these are blockers for single-user local use. All of them matter for production or multi-tenant deployment.

## Bottom Line

agentiso solves a real problem: giving local AI models a sandbox where their mistakes are cheap and reversible, with snapshot/fork primitives that don't exist in any other local tool. The setup cost is high (ZFS, QEMU, KVM, bridge networking), but if you're already running local LLM inference on a Linux box with a GPU, you have the hardware and the admin skills. For the use case described -- giving Devstral/MiniMax/Step models isolated workspaces via MCP -- this is the right tool, provided you limit the exposed tool surface to the 7 core tools and write a clear system prompt. It is not overkill; it is exactly the right level of isolation for models you don't fully trust.
