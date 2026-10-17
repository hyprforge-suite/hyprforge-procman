# hyprforge-procman

A process manager for Hyprland: what is running, what it costs, and which
window it belongs to — with a terminal twin that does what atop does, and
a recorder that keeps the history atop keeps.

Part of [Hyprforge](https://github.com/hyprforge-suite/hyprforge), a suite
of native Hyprland desktop apps — but it runs alone. Installing this gets
you the window, its helper, `hyprforge-top` and the recorder, and needs no
other Hyprforge component to start.

## What it does

**The window** (`hyprforge-procman`) is Windows Task Manager as the floor:

- **Processes**, grouped by the window they belong to (asked of Hyprland),
  then background processes and kernel threads. CPU, memory, disk,
  network and GPU per process, with the card it uses. End, Kill, Freeze,
  Resume, priority, End process tree — and a warning before ending
  anything the session itself runs on.
- **Performance**: CPU and every core, memory and swap, every disk, every
  network, every GPU (an NVIDIA card is read only while it is awake, so
  the window never wakes it), the battery, temperatures, fans, pressure.
  The Battery page says **what is using power**: each program's
  estimated watts on battery (the battery's draw above the quietest it
  was seen, shared by CPU and GPU time), or its share of the activity
  plugged in, and how often it wakes a core — every thread's, averaged
  over ten seconds.
- **Startup**: XDG autostart, user units and Hyprland's own startup
  programs, with how long each took to start.
- **Services**: system and user units, their CPU and memory from their
  cgroups, start, stop, restart, enable and disable, and the journal.
- **Users**, **Details** (`port:8080` and `file:~/notes.txt` find who has
  them), **App history** per app per day, and an **Advanced** view
  (Ctrl+.) with btop's layout and atop's detail.
- **Crashes**: every program that crashed, with the crashing thread and
  its stack trace (Copy stack trace makes a bug report), and every one
  killed for memory — by the kernel because the machine ran out, at a
  cgroup's own limit, or by systemd-oomd — with what it was using. Read
  from the journal, asking only for the fields it shows: a coredump
  record also holds the dead process's environment, which never leaves
  the journal.
- **Recorded**: the recorder's history in the window. A day drawn as CPU,
  memory and I/O pressure, strained stretches marked; pick a moment and
  see what was busiest, and what the hour around it added up to.

**`hyprforge-top`** is atop's screen in a terminal — its header lines,
its views (`g m d n s v c u p j`, and `y` for threads and `e` for GPU use live), its keys and sorts — live, or replaying
the recorder's history with `-r DAY -b HH:MM`. `hyprforge-top report
cpu|mem|disk|net|psi|top` is atopsar: what a day added up to, as text or
`--json`.

**The recorder** (`hyprforge-procman-recorder.service`) samples the whole
machine every ten seconds into a journal of one file a day — about 30 MB
a day and under half a percent of one core on a desktop. It is not
enabled by installing the package; Hyprforge Settings' Set up item
"Record system history" switches it on, or `systemctl enable --now
hyprforge-procman-recorder`. atop can keep running beside it.

## Privilege

The window always runs as you. What only root can read — another user's
I/O and open files, network per process, the recorder's history — comes
from **`hyprforge-procman-helper`**, a system D-Bus service started on
demand, which asks polkit before every call and exits when idle. A
password answers it for a few minutes (`auth_admin_keep`), as for any
administrator action; Show everything says so when that has lapsed.

The recorder runs as root because every process's figures and every exit
are root's to read, and then it is fenced in by its unit: it reads `/proc`
and `/sys`, writes `/var/log/hyprforge/procman`, speaks netlink for exits,
loads one eBPF program and nothing else. **Its journal is readable by root
alone** — command lines can hold secrets, and atop's logs are
world-readable; these deliberately are not. The window and `hyprforge-top`
read it through the helper, after polkit.

Network per process is counted by a small eBPF program at the kernel's
`sock_send_length` and `sock_recv_length` tracepoints (Linux 6.3), so UDP
counts as well as TCP — QUIC included, which a TCP-only figure misses.
Where nothing was counting, every view shows a dash, never a zero.

## Building

```
cargo build --release --features tui,netbpf
```

The window needs Wayland and xkbcommon; the `netbpf` feature needs clang
and libbpf's headers at build time (nothing at run time). Without
features, `cargo build --bin hyprforge-procman-recorder` builds the
recorder alone, which links no GUI toolkit and no D-Bus stack. The
helper's D-Bus activation file, its bus policy, the polkit actions, the
recorder's unit and its config are in `packaging/`.

`DESIGN.md` is the plan and where it stands.

## Licence

MIT, except `bpf/net.bpf.c`, which is MIT or GPL-2.0 — a tracing eBPF
program has to declare a GPL-compatible licence for the kernel to load it.
