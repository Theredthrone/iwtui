# iwtui

> An nmtui-style TUI for [iwd](https://iwd.wiki.kernel.org/) — browse,
> connect, and forget Wi-Fi networks from the terminal, with a list that
> updates itself.

`iwctl` works, but it's a REPL. `iwtui` is for people with `nmtui`
muscle memory: a main menu, centered dialogs, arrow-key navigation, a
button column next to the list — except the backend is iwd, and nothing
waits for a keypress to update. When iwd finishes a scan, when a network
appears, when association completes, the screen reflects it within a
quarter of a second.

## Features

- **Live auto-refresh** — a D-Bus signal watcher turns every interesting
  iwd event into a debounced reload. Scan from another TTY, toggle the
  radio with rfkill, connect from another client: the list, state line,
  and `*` connected marker follow along.
- **Real passphrase flow** — registers as an iwd *Agent*, so connecting
  to an unknown encrypted network pops a passphrase dialog; iwd waits
  for the answer, exactly like `iwctl`.
- **Stacked windows** — network list → network details → forget
  confirmation → error popups. Each layer dims the ones below it, and
  `Esc` unwinds one layer at a time.
- **nmtui look-and-feel** — `┤ Title ├` border titles, `<Button>` rows
  and columns, newt-style `↑ ▒ ▮ ▒ ↓` scrollbars, underscore-filled
  entry fields, boxed `┌ OK ┐` buttons.
- **Direct and small** — pure async D-Bus via zbus; no NetworkManager,
  no shelling out to `iwctl`, no polling loops besides one gentle
  signal-strength poll.

## Requirements

| Requirement | Notes |
|---|---|
- Linux with iwd running (developed against iwd 3.12)
- Rust (stable) — build-time only
- D-Bus access to `net.connman.iwd` — the same policy that lets `iwctl` run

## Build & run

```sh
cargo build --release
./target/release/iwtui
```

This usually works as a regular user. If every action fails with
`AccessDenied`, run it with `sudo`, or widen iwd's D-Bus policy for your
user (distros ship it as `net.connman.iwd.conf` under
`/usr/share/dbus-1/system.d/`).

## Interface tour

```
Main menu ──► Wi-Fi networks ──Enter──► Network details ──Forget──► Confirmation
                  │  ▲                      │
                  │  └── live updates arrive on their own (D-Bus signals)
                  └──── connecting to an unknown network pops a Passphrase dialog
```

## Keybindings

**Main menu**

| Key | Action |
|---|---|
| `Up` / `Down` | move |
| `Enter` / `Space` | select |
| `q` / `Esc` | quit |

**Wi-Fi networks**

| Key | Action |
|---|---|
| `Up` / `Down` | move in the network list |
| `Right` / `Tab` | focus the button column |
| `Left` | back to the list |
| `Enter` / `Space` | list: open the network's details · buttons: press |
| `r` | rescan |
| `Esc` / `<Back>` | return to the menu |
| `q` | quit |

**Network details** — a 2×2 button grid (`Connect`, `Disconnect`,
`Forget`, `Back`): arrows move, `Enter`/`Space` presses, `Esc` closes.

**Passphrase dialog** — type / `Backspace` edits, `Enter` accepts,
`Esc` cancels (aborts the connection attempt).

**Confirmations & error popups** — `Enter`, `Space`, or `Esc` dismisses.

`Ctrl+C` quits from anywhere.

## How the auto-refresh works

```
crossterm EventStream ─┐
iwd signal watcher ────┼──► mpsc channel ──► event loop ──► draw (every pass)
iwd Agent (passphrases)┘         ▲                │
                                 │                └── spawn: scan / connect /
        250 ms timer: debounced reload              disconnect / forget
        3 s timer: signal-strength poll
```

Three background tasks feed one event loop; the screen is redrawn on
every iteration, not just after keypresses. Commands that can block
(including `Network.Connect`, which may sit waiting on the passphrase
dialog) are always spawned, never awaited inline.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `AccessDenied` popups | insufficient D-Bus privileges — see *Build & run* |
| "No Wi-Fi station" | radio off (`rfkill`), iwd not running, or no Wi-Fi adapter |
| Empty list right after start | first scan still running — the state line shows `(scanning)` |
| Garbled terminal after a crash | run `reset`; the release profile uses `panic = abort`, so the cleanup guard can't run on panic |

## Development

| File | Responsibility |
|---|---|
| `src/main.rs` | terminal setup, background tasks, event loop |
| `src/app.rs` | state and key handling (the window stack) |
| `src/ui.rs` | rendering, styled after nmtui |
| `src/iwd.rs` | everything D-Bus: state, commands, signals, agent |

The whole codebase can be regenerated with `bash make-iwtui.sh` (the
original scaffolding script). Design notes, D-Bus gotchas, and a
verification checklist live in `MEMORY.md`.



- Radio power screen (nmtui `Radio` parity)
- Known-networks browser (list, forget, autoconnect toggle)
- Hidden-network connect
- `SignalLevelAgent` for instant signal bars (replaces the 3 s poll)
- 802.1X / private-key credentials in the agent

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).
