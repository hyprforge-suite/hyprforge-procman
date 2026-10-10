# hyprforge-procman — design

Windows Task Manager is the floor: everything it does, this does. The
aim is to do it better, in the ways Linux and Hyprland make possible
and Windows cannot follow. This file is the plan. Each phase is a round
of work ending in something usable, and each one records what was
checked on a real machine before it was built.

It also takes in the suite's planned Service Manager
(`hyprforge-servicemgr` in `docs/design/vision.md`) as its Services tab,
the way Task Manager carries one. One app makes sense because a service
*is* processes. Its row can show live CPU and memory from the same
refresh, and a process's row can name the unit it runs under. Two apps
would each read half of that.

## Privilege: a helper, never a root window

Decided 2026-10-10: this is a privileged application, and it gets its
privileges the way Files does for "Open as administrator". The window
always runs as the user. The privileged part is a small separate
program, never the GUI.

**`hyprforge-procman-helper`** is a system D-Bus service
(`dev.hyprforge.ProcMan1`), started on demand by D-Bus activation and
exiting when idle. Every method checks polkit for the caller before
doing anything. It does two jobs:

- **It reads what only root can read**, for every process:
  - I/O counters, open files and sockets, and GPU `fdinfo`;
  - per-process network traffic, through an eBPF program it loads and
    owns;
  - package power from RAPL.
  This is polkit action `org.hyprforge.procman.read-all`, which defaults
  to `auth_admin_keep`. The window shows a single **Show everything**
  control, the same thing as Task Manager's "Show processes from all
  users", which elevates through UAC there. One prompt, through
  hyprforge-polkit, and it stays unlocked for polkit's few minutes.
- **It acts on what the user cannot**:
  - signals to another user's process (`signal-any`);
  - raising a priority or changing another user's (`renice`);
  - affinity on another user's process.
  Each is its own action, defaulting to `auth_admin_keep`. Ending a
  root process asks; ending your own never does, because the window
  does that itself (`signal.rs`).

The helper takes only typed arguments: a pid with its start time, a
signal number from a fixed set, a nice value in range. It re-checks the
start time through its own pidfd, as `signal.rs` does. It never takes a
path, a command line or a string to run.

**Services need no helper.** systemd checks polkit itself on every
`StartUnit`/`StopUnit`/`EnableUnitFiles`, so the window calls systemd
directly and the prompt comes from hyprforge-polkit, already drawn as
the lock card. Users and sessions are the same, through logind.

Without the helper installed, or with the prompt declined, the app is
complete for your own processes. Each value it cannot read says "not
yours to see — Show everything", never 0.

## What the machine gives without root

Checked on 2026-10-10 (Arch, kernel 7.2, amdgpu + NVIDIA, Hyprland 0.56
under uwsm):

| Source | Gives | To whom |
|---|---|---|
| `/proc/<pid>/stat`, `statm`, `cmdline` | CPU ticks, state, threads, private memory, command | every process |
| `/proc/<pid>/io` | bytes read and written | own processes only (others: EACCES) |
| `/proc/<pid>/fdinfo/*` `drm-engine-*`, `drm-memory-*` | GPU time per engine, VRAM/GTT per process | own processes (amdgpu, i915/xe; NVIDIA's open driver) |
| `/proc/<pid>/fd`, `/proc/net/{tcp,udp}{,6}` | open files; which socket inodes are whose | own processes |
| `/proc/<pid>/cgroup` | the systemd unit it runs in | every process |
| cgroup `cpu.stat`, `memory.current`, `io.stat`, `cgroup.freeze` | exact per-unit use; freezing a whole unit | every unit to read; own units to freeze |
| `/proc/pressure/{cpu,memory,io}` | PSI: how long things *waited* for each | everyone |
| `/proc/stat`, `/proc/meminfo`, `/proc/diskstats`, `/proc/net/dev` | machine-wide CPU, memory, disks, networks | everyone |
| `/sys/class/hwmon/*` | temperatures, fans, power (`k10temp`, `amdgpu`, `nvme`, `BAT1`) | everyone |
| `/sys/class/drm/card*/device/gpu_busy_percent`, `mem_info_vram_*` | whole-GPU busy and VRAM (amdgpu) | everyone |
| `cpufreq/` | per-core clock | everyone |
| `systemd-analyze --user blame` | how long each user unit took to start | the user |
| systemd over D-Bus | units, states, start/stop/restart/enable | read: everyone; change system units: polkit (our own agent asks) |
| logind over D-Bus | sessions and users; terminate a session | read: everyone; others' sessions: polkit |
| `hyprctl clients -j` | which process owns which window, and where it is | the session |

Apps launched from Hyprland share the compositor's cgroup
(`wayland-wm@hyprland.desktop.service`), so an *app* is found by window
and process tree, as it is now. A *service* is found by its own cgroup.

**Root only, so through the helper.** Per-process network traffic
needs eBPF (or packet capture). Power has only RAPL, which is root-only
since 2020, and RAPL is per package, not per process. So "power" is
shown for the machine, and for a process only as an estimate labelled
as one, from its CPU and GPU share of the measured whole. An estimate
shown as a fact is worse than no number.

## Task Manager, tab by tab, and what is better here

**Processes.** Apps (by window) then background, CPU, memory, disk,
network, GPU, power, status, grouped and expandable, End task.
- Here: apps by Hyprland window, with their process tree folded in
  (built). CPU as a share of the whole machine, memory as private
  memory (built). Disk per process from `/proc/<pid>/io` (own processes).
  GPU per process from DRM `fdinfo`, which Windows has and most Linux
  tools do not.
- Better:
  - an app row can **jump to its window** (focus, or bring it to this
    workspace);
  - the row says which **workspace and monitor** it is on;
  - **search with chips** like Files' (`user:root`, `cpu:>5`, `mem:>1G`,
    `port:8080`, `file:~/x.db`);
  - **End** goes through a pidfd, so it can never hit a reused pid
    (built);
  - **Freeze** (SIGSTOP, or `cgroup.freeze` for a unit) and **Resume**,
    which Windows only has as the softer "efficiency mode";
  - a row with no reading yet says so rather than showing 0%.

**Performance.** CPU, memory, disks, network adapters, GPU: a graph
each, with figures beside it.
- Here: all of those (`/proc/stat` with per-core and per-core clock,
  meminfo, diskstats, net/dev, amdgpu's `gpu_busy_percent` and VRAM).
- Better:
  - **temperatures and fans** from hwmon beside each graph (Windows
    shows none for the CPU);
  - **pressure** (PSI) for CPU, memory and I/O, which answers "is the
    machine actually *waiting*", not just "is it busy";
  - **battery** draw and time left (`BAT1`, UPower, as the tray reads
    it);
  - a hover on any point says what used the most at that moment, so the
    graph explains its own spike.

**App history.** Cumulative CPU and network per app over 30 days.
- Here: per-app CPU time and peak memory, recorded only while
  hyprforge-procman runs, plus per-unit totals from cgroups, which the
  kernel keeps whether or not anything was watching. Network is
  deferred (see above). It is honest about the window it covers: "since
  you opened this at 14:02", never a total it does not have.

**Startup apps.** Enable or disable what starts at sign-in, with a
"startup impact".
- Here, three sources in one list, each switched through the module
  that owns it:
  - systemd user units wanted by `graphical-session.target` (enable or
    disable);
  - XDG autostart `.desktop` files (`Hidden=true` in an override in
    `~/.config/autostart`, the spec's own off switch);
  - `hl.exec_cmd` lines in the Hyprland config, through
    `hyprforge-core`, read-only at first.
- Better: impact is **measured**, not rated. `systemd-analyze --user
  blame` gives each unit's real start time, so the list says "1.3 s",
  where Windows says "High".

**Users.** Per-user use; disconnect or sign out.
- Here: logind's sessions and the users behind them (seat, TTY, type,
  idle), with per-user CPU and memory summed from the process table.
  Lock or end a session through logind; another user's goes through
  polkit.

**Details.** Every process, every column: PID, status, user, CPU,
memory, architecture, priority, affinity, end process tree, set
priority, create dump file, open file location, properties.
- Here: a tree view (parent → children) beside the flat one. Columns
  chosen from: PID, PPID, state, user, CPU, CPU time, private and
  resident memory, threads, open files, nice, I/O, GPU, unit (cgroup),
  start time, command line, executable.
- Actions:
  - End, Kill and **End tree**, the descendants first and each one
    through its own pidfd;
  - **Set priority** (nice), raising a priority asking through polkit;
  - **Affinity** (`sched_setaffinity`);
  - **Open file location** (the exe, in Files through FileManager1);
  - **Open files and ports** (`/proc/<pid>/fd` and the socket table);
  - **Create a core dump** (`gcore` where installed, said when not).
- Better: "**Who has this file / port?**" works in reverse, from the
  search field. That is lsof and ss in one place, which Windows splits
  across Resource Monitor.

**Services.** List with start/stop/restart, and open Services.
- Here (the Service Manager): system and user units over systemd's
  D-Bus API.
  - The list: state, sub-state, description, enabled or not, and live
    CPU and memory from the unit's cgroup.
  - Actions: Start, Stop, Restart, Reload, Enable and Disable, plus
    Mask and Unmask behind a confirm.
  - Detail: the unit file, its dependencies, and the **journal** for
    that unit (`journalctl -u`, bounded).
  - Filters: failed, running, enabled-but-dead.
- System units change through polkit, so the request lands in our own
  agent's prompt.
- Better: a failed unit is the first thing you see, with its last log
  lines beside it. Windows makes you open Event Viewer for that.

**Run new task** (File → Run). A field that runs a command, optionally
as administrator through pkexec.

## Across every tab

- Keyboard first, through `hyprforge-keys`:
  - Ctrl+F searches, Delete ends, Ctrl+Delete kills, Enter opens
    details;
  - Space pauses refreshing, the one thing that makes picking a row in
    a moving list possible.
- Set up offers **Ctrl+Shift+Escape** to open it, as on Windows.
- Refresh interval: 0.5, 1, 2 or 5 s, or paused. Reading is off the
  UI thread. A refresh never waits on Hyprland: a slow `hyprctl` only
  delays the window names.
- Nothing is logged that a process put in its command line or
  environment. A command line can hold a token.
- Every component runs alone:
  - no Hyprland means no Apps group (said once, at the top);
  - no systemd means no Services tab (said in the tab);
  - another user's details read as "not yours to see", never as zero.

## Phases

1. **Processes and Performance (CPU, memory), as the user.** The list (built
   underneath), the window, End/Kill with confirm, Freeze/Resume, jump
   to the window, the search, and the CPU, per-core and memory graphs.
2. **The helper.** The D-Bus service, its polkit actions, Show
   everything, signals and priority for any process. The window and the
   helper share `proc.rs`; the helper only adds what root can read.
3. **Performance, complete.** Disks, networks, GPU (whole and per
   process), temperatures, PSI, battery.
4. **Details.** Tree, columns, End tree, priority, affinity, open file
   location, open files and ports, reverse lookup.
5. **Services.** The systemd unit list and actions, the journal pane,
   unit cgroup use.
6. **Startup apps and Users.**
7. **App history, and per-process network** (the helper's eBPF). The recorder and its view.
8. Set up's keybind, packaging (the helper's D-Bus service file and
   polkit policy go in the package; a source install puts them in
   place with sudo, as the lock's PAM file is), the site page, the
   twelfth component.
