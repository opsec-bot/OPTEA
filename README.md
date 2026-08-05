# OPTEA

Rainbow Six Siege system tuning for Windows — **measured rather than assumed**.

Most Windows "gaming tweak" tools apply a pile of registry edits and declare
victory. OPTEA is built the other way round: the measurement harness came first,
every change is captured before it is applied, and a tweak only earns the label
`measured` after an A/B benchmark on *your* machine says so. Most entries in the
catalog are expected to land on **"no detectable effect"** — that is the tool
working correctly, not failing.

## Status

| Phase | State |
|---|---|
| 0 — Win32 layer + `doctor` | done |
| 1 — statistics | done |
| 1 — PresentMon capture | written, **not yet validated against a live service** |
| 2 — transactional apply/revert | done |
| 3 — tweak catalog | initial set |
| 4 — Siege `GameSettings.ini` | not started |
| 5 — A/B benchmark automation | not started |
| 6 — GUI | not started |
| 7 — live process tuning | not started |

## Commands

```
optea doctor              # read-only system report; changes nothing
optea list                # catalog with each entry's current state
optea apply --dry-run     # what would change
optea apply               # apply safe tweaks (snapshot captured first)
optea revert              # undo the most recent transaction
optea history             # recorded transactions
```

Add `--json` to `doctor`, `list`, and `history` for machine-readable output.
`--risk moderate|deep` widens the selection; `deep` additionally requires
`--i-understand`. Applying and reverting need an elevated terminal.

## Design rules

**Nothing is applied that cannot be reversed.** `Tweak::capture` runs and is
persisted to disk *before* `Tweak::apply`, so an interruption between the two
still leaves a usable record. Registry capture distinguishes *absent* from
*zero*, so reverting a tweak that created a value deletes it rather than writing
`0` — and if the apply had to create the *key* as well (common under
`SOFTWARE\Policies\...`), revert removes that too, but only when it was recorded
as absent beforehand and is still empty. A mid-profile failure rolls back every
change already made in that run.

This is verified end to end against real `HKLM` values, not only in unit tests:
apply the moderate tier, diff every touched value, revert, and require the result
to be byte-identical to the starting state — including key existence.

**Inapplicable is not the same as ineffective.** Tweaks are gated on real system
facts and say why they are skipped. Example: system-wide timer resolution has
been per-process since Windows 10 2004, and the `GlobalTimerResolutionRequests`
override exists *only on Windows 11* — so on Windows 10 it is exposed as
report-only with the reason stated, never as a toggle that silently does nothing.

**Evidence is earned.** Everything ships as `unverified`. Only the benchmark
harness promotes an entry, using paired trials and a bootstrap confidence
interval that is allowed to conclude nothing happened.

**The anti-cheat boundary is hard.** Siege runs BattlEye. OPTEA never reads or
writes game memory, injects, hooks, or attaches a debugger. System-wide settings
and pre-launch configuration only. Live process tuning (phase 7) is opt-in, off
by default, requests the narrowest possible access right, and prefers a
suspended-launch path that opens no handle against a running protected process.

## Build

```
cargo build --release
cargo test --workspace
```

Rust 1.82+, MSVC toolchain, Windows 10/11.

## Measurement

Frame capture wraps Intel [PresentMon](https://github.com/GameTechDev/PresentMon)
(service + SDK required; `doctor` reports whether it is present). The FFI in
`crates/optea-metrics/src/ffi.rs` was transcribed from the public header and has
**not yet been exercised against a running service**. `PM_METRIC` is a positional
C enum, so a stale table would read the wrong field rather than fail loudly.
Three guards are in place: an API version check at session open, a single
name-keyed ordinal table, and a plausibility range on captured frame times that
rejects a capture whose values could not be real frames.
