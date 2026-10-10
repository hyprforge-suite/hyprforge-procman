# Changelog

This component is released with the Hyprforge suite and shares its version: see the suite's `CHANGELOG.md`.

## [Unreleased]

### Added

- The window: Processes grouped by window, Performance, Startup, Users, Details, Services, App history, the Advanced view, and the Recorded tab.
- The helper, `dev.hyprforge.ProcMan1`: polkit-checked reads of what only root can see, ending and re-prioritising another user's process, a day of the recorder's history handed over as an open file, and network per process.
- The recorder: the whole machine every ten seconds into a root-only journal, one file a day, pruned by age and size.
- `hyprforge-top`: atop's screen live or replayed, and `report` for atopsar's summaries, as text or JSON.
- Network per process, counted by an eBPF program at the kernel's socket tracepoints.
