# Changelog

This component is released with the Hyprforge suite and shares its version: see the suite's `CHANGELOG.md`.

## [0.1.15] - 2026-10-16

<!-- Listed from this component's commit subjects by tools/changelog.py. -->

- Journal format 3: say what a frame could not see

## [0.1.14] - 2026-10-11

<!-- Drafted by tools/changelog.py; sort into Added / Changed / Fixed before releasing. -->

- First release as a component of the suite.

## [Unreleased]

### Added

- The window: Processes grouped by window, Performance, Startup, Users, Details, Services, App history, the Advanced view, and the Recorded tab.
- The helper, `dev.hyprforge.ProcMan1`: polkit-checked reads of what only root can see, ending and re-prioritising another user's process, a day of the recorder's history handed over as an open file, and network per process.
- The recorder: the whole machine every ten seconds into a root-only journal, one file a day, pruned by age and size.
- `hyprforge-top`: atop's screen live or replayed, and `report` for atopsar's summaries, as text or JSON.
- Network per process, counted by an eBPF program at the kernel's socket tracepoints.
- Right-click menus on processes and services: end, kill, freeze, priority, open file location, a service's actions, and copying a process ID or a unit's name.
- `port:` and `file:` search every user's processes while Show everything is on.
