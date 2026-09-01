# iwtui

**iwtui** is a terminal Wi-Fi manager for [iwd](https://git.kernel.org/pub/scm/network/wireless/iwd.git/)
(Intel Wireless Daemon), with the classic look and feel of `nmtui` — the familiar
blue screen, centered dialogs with drop shadows, and highlights that fill behind
the text.

```
        Activate a connection
 Name                          Sig     %  Security   Status
  HomeNet                     ▆▆▆▆   92%      psk*  Active
  CoffeeShop                  ▆▆▆    74%     open
  Neighbour_5G                ▆▆      48%      psk*
  TP-LINK_A73F                          12%      psk

                    < Rescan > < Disconnect > < Quit >
```

## What it does

- **Activate a connection** — scan list with signal bars and percentages,
  connect and disconnect with one key.
- **Edit a connection** — saved networks, auto-connect on/off, forget, and
  connecting to hidden networks.
- **Set system hostname** — just like nmtui: as root it applies instantly;
  otherwise it asks for the root password and applies it through `sudo`.
- **Password dialogs** — iwd's credential prompts (passphrase, enterprise
  user/password, private key) appear inside the app; `Ctrl+R` shows what you
  typed.

## Requirements

- Linux with iwd running (`systemctl start iwd`)
- Permission to talk to iwd: root, or membership in the `netdev` group —
  same as `iwctl`

## Install

```sh
cargo install --path .
iwtui
```

## Keys

| Key                | Action                        |
| ------------------ | ----------------------------- |
| `↑`/`↓` or `k`/`j` | Move in a list                |
| `←`/`→` or `h`/`l` | Move between buttons          |
| `Tab`              | Switch list ↔ buttons         |
| `Enter`            | Activate                      |
| `Esc` / `q`        | Back / quit                   |
| `?`                | Help overlay                  |
| `r`                | Rescan (Activate screen)      |
| `n`                | Hidden network (Activate)     |
| `p`                | Wi-Fi power on/off (Activate) |
| `Delete`           | Forget saved network          |
| `Ctrl+R`           | Show/hide password            |

## License

GPL-3.0-or-later — see [LICENSE](LICENSE).
