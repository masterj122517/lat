<div align="center">

<p><img src="assets/logo.png" alt="lat's lazy penguin logo" width="180"></p>

# lat

**Your agents. One view. The exact pane.**

A lazygit-style terminal dashboard for **omp**, **Codex**, and **OpenCode**.<br>
Find what's running, inspect its output, and jump straight to its tmux pane.

[Quick start](#quick-start) · [tmux shortcut](#one-shortcut-from-any-pane) · [Keybindings](#keybindings) · [How status works](#honest-status-not-guesswork)

</div>

![lat showing three local agents, selected-agent details, and live tmux pane output](assets/lat-demo.png)

<sub>Actual `lat` terminal capture, rendered with local fixture processes. Demo output—not model responses. No agent credentials or model requests were used.</sub>

## Stop hunting through tmux windows

Running agents across projects is easy. Remembering which pane owns which one is not.

`lat` gives you one place to look:

- **Find your agents automatically.** Start them as usual; no hooks, wrappers, or agent configuration changes.
- **See the project and location.** Product, PID, activity, working directory, and tmux location stay together.
- **Read before you jump.** Preview the selected pane's last 200 grid rows without switching away.
- **Jump to the agent.** Press **Enter** for its tmux pane or supported native terminal. `lat` revalidates process identity before navigating.
- **Keep your hands on the keyboard.** Vim-style movement, focus switching, scrolling, and filtering.
- **Keep independent agents independent.** Two agents in one directory—or even one pane—remain separate rows.
- **Know when input is needed.** Recognizable approval/question dialogs trigger macOS or Linux desktop notifications, including for unselected panes.

No model calls. No transcript scraping. No daemon to install.

## Quick start

You need **Rust 1.88+** to build and a terminal to run the UI. **tmux 3.8** is the runtime-verified version. Without tmux, process discovery and supported native-terminal jumps still work; pane previews and input-prompt monitoring require tmux.

Install directly from this repository:

```sh
cargo install --git https://github.com/masterj122517/lat.git --locked --root "$HOME/.local"
~/.local/bin/lat
```

If `~/.local/bin` is on your `PATH`, just run `lat`.

Already have supported agents running? They appear automatically. Select one with `j/k`, inspect its output, and press **Enter** to jump.

<details>
<summary>Build from a local checkout instead</summary>

```sh
git clone https://github.com/masterj122517/lat.git
cd lat
cargo build --release --locked
./target/release/lat
```

To install that checkout into `~/.local/bin`:

```sh
cargo install --path . --locked --root "$HOME/.local"
```

</details>

`lat --help`, `lat --version`, and `lat --watch` work without a terminal. Run `lat` without arguments for the interactive UI.

**Platform note:** runtime-verified on macOS arm64. The implementation targets Unix/macOS/Linux, but Linux has not been runtime-verified; its relevant process and protocol boundaries have deterministic fixture coverage.

## One shortcut from any pane

Put this in your tmux config to open `lat` with **prefix + t**:

```tmux
bind-key -N "Track local agents" t run-shell -b 'tmux display-popup -E -c #{q:client_name} -w 95% -h 90% -d #{q:pane_current_path} -e TMUX_PANE=#{pane_id} -T "Local agents" "$HOME/.local/bin/lat"'
```

Reload the file you edited—for example:

```sh
tmux source-file ~/.config/tmux/tmux.conf
```

Use `~/.tmux.conf` instead if that's your configuration file. Your prefix stays unchanged; `t` replaces tmux's default clock binding.

Now: **prefix + t → select an agent → Enter**. The popup closes after a successful jump. `q` closes it without navigating.

> **Why the `run-shell` and `TMUX_PANE`?** Popup jobs do not set `TMUX_PANE` automatically, and `display-popup -e` does not expand tmux formats. `run-shell` expands the invoking client and pane first, so `lat` can validate the correct client. Keep these parts of the binding.

## A familiar three-pane workflow

**Agents** on the left. **Details** at the upper right. **Pane output** below.

The list shows product, root PID, activity, project basename, and tmux location. Details add process age, OS state, full working directory, session/window names, and exact pane IDs.

The layout stacks vertically below 100 columns. Minimum terminal size: **60 columns × 16 rows**. Colors use the terminal's default background; status also has text labels.

### Keybindings

| Key | Action |
|---|---|
| `j` / `k`, ↓ / ↑ | Move through agents; scroll when preview is focused |
| `g` / `G` | First / last agent; top / bottom of preview |
| Ctrl-D / Ctrl-U | Half-page down / up |
| `h` / `l`, Tab / Shift-Tab | Cycle list, details, and preview focus |
| `/` | Filter by product, working directory, or session name |
| Enter while filtering | Accept the filter |
| Escape while filtering | Restore the previous filter |
| **Enter** outside filtering | Jump to the selected agent, regardless of focus |
| `r` | Scan immediately and refresh the selected preview |
| `?` | Show help |
| Escape | Close help or the client chooser |
| `q` / Ctrl-C | Quit; `q` is text while filtering, Ctrl-C always quits |

Filtering is case-insensitive. Selection follows process identity through refreshes. A new selection starts at the bottom of its preview; new output follows the bottom only if you're already there.

## Desktop notifications

The dashboard automatically monitors **all discovered agents mapped to tmux panes**, not just the selected or filtered rows. To keep monitoring without an open dashboard:

```sh
lat --watch
```

This is a foreground, non-interactive monitor: keep it running in a shell, tmux window, or your existing service manager; Ctrl-C stops it. No daemon or agent hooks are installed. Monitoring stops when that `lat` process exits. Run only one monitor/dashboard if you want to avoid notifications from multiple independent instances.

- **macOS:** uses `osascript` to submit native notifications. Allow notifications for the sender in System Settings; Focus/Do Not Disturb can suppress banners.
- **Linux:** requires `notify-send` (usually the `libnotify-bin` / `libnotify` package), a desktop notification service, and access to the graphical session's D-Bus environment. A headless SSH session alone cannot show a desktop banner.
- Notifications contain product, project basename, tmux location, and the detected prompt category—not the question, tool command, or captured output. Arguments are passed as data, never interpolated into shell or AppleScript code.
- Each observed waiting episode alerts once, including a dialog already visible when monitoring starts. Output repainting, selection, and refresh do not repeat it. The detector rearms after observing the dialog disappear. A restart starts a new monitoring episode. Dialogs replaced without an intervening observed non-dialog screen may be coalesced.
- Missing commands, failed delivery, inspection errors, and three-second native-command timeouts appear in the dashboard footer or on `--watch` stderr. A failed alert is not retried continuously; fix the diagnostic and restart monitoring or wait for the next episode. Successful submission does not prove the desktop displayed a banner.

Detection is **conservative screen matching, not an agent-state protocol**. It checks the current visible screen (never scrollback) for product-specific dialog chrome: omp tool approval / Ask dialogs, Codex approval / structured questions, and OpenCode permission / form dialogs. English/default key hints are supported. Ordinary quiet periods, idle composers, completion messages, and prose questions are not treated as requests for input. Customized keybindings, narrow layouts, different versions/languages, very short-lived prompts, hidden dialogs, and agents outside tmux can be missed. Shared panes remain a pane-level signal; prompt text that imitates the same chrome can also match.

The upstream prompt layouts are documented in [omp's dialog implementation](https://github.com/can1357/oh-my-pi/tree/main/packages/tui/src/overlays), [Codex's approval overlay](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/approval_overlay.rs), and OpenCode's [permissions](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/routes/session/permission.tsx) / [forms](https://github.com/anomalyco/opencode/blob/dev/packages/tui/src/routes/session/form.tsx). Changes to these layouts need matching detector coverage.

## Agents outside tmux

Select an **outside tmux** row and press **Enter**. `lat` focuses the agent's existing terminal without restarting it, forwarding keystrokes, or taking over its PTY; successful navigation closes the dashboard.

| Platform | Supported focus target | Requirements and limits |
|---|---|---|
| macOS Terminal | Exact window and tab, matched by TTY | Allow macOS Automation access to Terminal. Launcher ancestors may have exited; TTY ownership is checked against running terminal apps. |
| macOS iTerm2 | Exact window, tab, and split session, matched by TTY | iTerm2 scripting and Automation permission. |
| macOS Ghostty | Exact terminal surface, matched by TTY | Enable `macos-applescript` and use a Ghostty build exposing terminal TTYs in its scripting dictionary. Older builds, including the locally installed 1.3.1 dictionary, cannot be mapped safely. |
| Linux X11 | Existing terminal window | `wmctrl`, `DISPLAY`, and a window owned by an agent ancestor. `WINDOWID` is honored only when ownership matches; otherwise there must be exactly one matching window. This does not select tabs/splits inside that window. |

Native Wayland focus, other terminal emulators, detached processes without a controlling TTY, missing permissions, and ambiguous windows return a diagnostic instead of guessing. Previews and input-prompt notifications remain tmux-only. A process still belonging to tmux with an unavailable pane is not redirected to an arbitrary GUI terminal.

## Honest status, not guesswork

`lat` knows a process is alive. With tmux, it can also observe when its pane last produced output. It does **not** know what the agent is thinking.

| Status | What it means |
|---|---|
| **output active** | The mapped pane produced output within the last three seconds. |
| **quiet** | The pane's last output is older, or tmux has recorded no output. |
| **running** | The process is alive, but pane-output activity is unavailable. |
| **approval prompt detected** / **question prompt detected** | A recognizable interactive dialog is visible in the mapped pane; this temporarily replaces the activity label in the list. |

Activity belongs to the **shared pane**, including terminal control traffic. If several agents share a pane, they share that activity signal. Quiet is not “waiting for approval”; active is not “making progress.” Neither proves completion, success, or failure.

OS run/sleep/stopped state is shown separately. Stopped agents remain visible; dead processes disappear on the next completed scan. `lat` is a live view, not a completed-task history.

## What gets tracked?

Local CLI processes for **omp**, **Codex**, and **OpenCode**, using exact native entrypoints and the supported installed Bun/Node launcher forms.

- Internal omp workers are excluded, so a launcher plus its broker does not become duplicate agents.
- A matched Codex launcher and its direct native child are coalesced when their application arguments agree.
- Codex `exec` remains a task row; protocol/infrastructure hosts such as `exec-server` and `app-server` are excluded.
- OpenCode `serve` and `acp` hosts are excluded.
- Similar names such as `codex-helper`, product words in prompts, and arbitrary JavaScript scripts do not count.
- Supported agents outside tmux remain visible as **outside tmux**, with native-terminal navigation where supported rather than a fabricated tmux target.

This is not a remote-machine tracker or an arbitrary process-name search. It does not infer agents from desktop renderer processes or enumerate every tmux socket.

<details>
<summary><strong>Navigation safety and tmux behavior</strong></summary>

Enter requests a fresh scan. Navigation proceeds only if the original product/PID/start-time identity and mapped pane root/target still match (or both the selected and refreshed row remain outside tmux). Native focus revalidates process identity again and resolves the terminal. A gone process, changed mapping, or unavailable terminal produces an error instead of jumping somewhere else.

- **Tracker inside tmux, target in tmux:** switch an attached client viewing the invoking pane and session. Multiple matches open a client chooser; `lat` does not guess which client you meant.
- **Tracker outside tmux, target in tmux:** attach to the selected pane without detaching other clients.
- **Target outside tmux:** use the native-terminal focus behavior above, regardless of where the tracker runs.
- **Zoom:** preserved when switching to a destination pane.
- **Server selection:** use the current launch environment's tmux server, or the default server outside tmux. Custom sockets must be selected through the launch environment.
- **Unavailable tmux:** keep process rows, clear jump targets, and show the diagnostic. Discovery never creates a server.

**tmux shares active window/pane state within sessions and windows.** Targeting one client does not give clients independent selections when they share that state.

No arbitrary input is forwarded to agent panes. Preview text is decoded, bounded to 200 lines / 256 KiB, and sanitized rather than executed as terminal controls. Trailing empty grid rows are hidden so recent text remains visible.

</details>

<details>
<summary><strong>Small by design: collection and measured overhead</strong></summary>

One collector thread reuses sysinfo process state. Scans run once per second or on request, never overlap, and publish to a replaceable latest-value slot rather than an unbounded queue. Input and rendering stay on the main thread.

Each periodic scan performs one tmux inventory call, regardless of agent count. Current screens for mapped agent panes are captured on first observation, changed output timestamps/dimensions/pane roots, or explicit refresh, and cached per pane. Only the selected pane also gets a bounded scrollback preview. On tmux versions without output timestamps, captures refresh once per scan and activity remains unavailable. Desktop delivery and native focus run on the collector, not the input/render thread, with a three-second timeout per native command.

Rendering occurs on input, resize, or changed visible data—not a continuous idle redraw loop.

Historical baseline **before input-prompt monitoring was added**, measured on macOS arm64 using a release build, over **30 seconds per case** (not a measurement of current capture/notification overhead):

| Owned fixture agents | Median process + inventory scan | Maximum scan | Final RSS |
|---:|---:|---:|---:|
| 0 | 26.6 ms | 28.2 ms | 9.5 MiB |
| 3 | 27.0 ms | 28.6 ms | 9.6 MiB |
| 20 | 27.3 ms | 30.4 ms | 9.8 MiB |

Two pre-existing agents remained detected in every case. These are reference measurements, not a cross-platform performance guarantee. Sixty selection/refresh cycles held RSS steady at 9,872 KiB after initial cache growth. Temporary instrumentation and fixture processes were removed afterward.

</details>

## Contributing

Bug reports, better discovery coverage, and platform verification are welcome. For a discovery or navigation bug, include the agent/version, OS, tmux version, invocation form, and what `lat` displayed. **Redact prompts, paths, credentials, and captured output before sharing.**

From a checkout:

```sh
cargo fmt --check
cargo test --locked
cargo build --release --locked
```

Process classification, tmux framing/escaping, ancestry mapping, activity boundaries, stale-target refusal, and selection behavior have deterministic regression coverage. Real-PTY smoke checks also exercised installed omp discovery, popup navigation, cross-session and split-pane jumps, zoom preservation, client choice, disappearance, and terminal restoration.

Notification coverage includes approval/question fixtures for all three products, ordinary-output negatives, stale prompt footers, per-episode suppression/rearming, native-command failure/timeout, and Linux markup escaping. Navigation coverage includes changed process/pane identities, outside-to-inside mapping changes, absent TTYs, and ambiguous or stale X11 window IDs. macOS smoke checks exercised a real Terminal-hosted fixture outside tmux, selected it through the actual TUI, and verified the target TTY became frontmost among two Terminal windows. Headless monitoring exercised quiet → permission → sustained wait → active → permission transitions against a real tmux pane with a recording delivery executable. Real `osascript` submission and missing-command diagnostics were also exercised; banner presentation, iTerm2/Ghostty focus, and a Linux desktop remain unverified.

Latest macOS arm64 verification (2026-10-17): all three commands above passed, including **35 tests**. The notification-transition and actual-TUI Terminal-focus smoke checks above also passed again; the listed platform/display limitations remain unchanged.

Keep the tool boring to operate: precise discovery, truthful labels, safe navigation, and no extra setup for agents.

## License

[MIT](LICENSE).

---

Built with [Rust](https://www.rust-lang.org/) and [Ratatui](https://ratatui.rs/). Inspired by the focused, keyboard-first workflow of lazygit.
