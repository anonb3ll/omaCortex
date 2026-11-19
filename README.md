# omaCortex

A private, local memory of your day for [Omarchy](https://omarchy.org).

omaCortex keeps track of what you were doing on your computer: which windows
you worked in, what you watched and listened to, and which files you opened.
It shows that in a bar panel ("Where was I?"), summarizes your day with a
**local** model, and gives your AI agents the same memory over **MCP**, so
they stop asking you to repeat yourself.

It is the operating-system counterpart of a browser memory extension: one
timeline across every app, kept on your machine.

<p align="center"><img src="preview.png" alt="The omaCortex panel: today's sessions, media, files, notes and a summary" width="420"></p>

## Privacy first

- **Off until you say so.** Nothing is recorded until you press *Start
  recording* or run `omacortex enable`.
- **Local only.** One SQLite database in `~/.local/share/omacortex/`, mode
  `0600`, directory `0700`. omaCortex makes no network connections except to
  a model server on `127.0.0.1`; any other model endpoint is refused unless
  you set `model.allow_remote = true`.
- **Metadata, not content.** It records the focused window's app and title,
  the title of what is playing, and the paths of files you open. It never
  takes screenshots, reads window contents, logs keystrokes or reads the
  clipboard.
- **Never recorded:** password managers (1Password, Bitwarden, KeePassXC,
  Proton Pass), pinentry and polkit prompts, private/incognito browser
  windows, sign-in, banking and 2FA pages, workspaces you exclude, and files
  under `~/.ssh`, `~/.gnupg`, `~/.password-store` or with key-like names. An
  excluded window leaves a gap; it is not stored under another name.
- **Scrubbed:** e-mail addresses, API tokens and long secrets, and URL query
  strings are removed from every title before it is stored.
- **You stay in control.** Pause from the bar (right-click), for an hour, or
  until you resume. *Forget last 15 min* in the panel, or
  `omacortex forget --app signal`, `--since/--until`, `--all`. History older
  than 30 days is deleted automatically. Deleted rows are overwritten
  (`secure_delete`), and `forget --all` also vacuums the file.
- **Agents can read, not erase or resume.** The MCP server is read-only apart
  from adding notes (which you can turn off). It cannot delete history,
  change privacy settings or resume a paused recorder.

See [PRIVACY.md](PRIVACY.md) for the full list.

## Install

### As an Omarchy plugin

```bash
omarchy plugin add https://github.com/anonb3ll/omaCortex --enable
```

This adds the bar icon and starts the recorder with the shell. Open the
panel, read what it records, and press **Start recording**.

The plugin needs nothing beyond what Omarchy ships: Python 3.11+ and
`busctl` (systemd). Optional keybindings for `~/.config/hypr/bindings.lua`:

```lua
o.bind("SUPER + ALT + O", "omaCortex", { ipc = "io.github.anonb3ll.omacortex.toggle" })
o.bind("SUPER + ALT + P", "Pause omaCortex", { ipc = "omacortex.toggle" })
```

IPC methods on `omacortex`: `toggle`, `pause`, `pauseHour`, `resume`,
`forgetRecent`, `summarize`. Try one with `omarchy-shell omacortex pauseHour`.

### The command, and agents

```bash
~/.config/omarchy/plugins/io.github.anonb3ll.omacortex/install.sh
```

links `omacortex` into `~/.local/bin`. Use `install.sh --systemd` instead if
you want the recorder as a systemd user service without the shell plugin
(the two never run twice: a lock file keeps one recorder per user).

## Use it

| Command | Does |
|---|---|
| `omacortex enable` / `disable` | Start or stop recording (history is kept) |
| `omacortex pause [30m]` / `resume` / `toggle` | Pause for a while or until resumed |
| `omacortex now` | What you are doing and were just doing |
| `omacortex sessions --since today` | Your day as sessions per app, with the main titles |
| `omacortex usage --since 1w` | Time per app |
| `omacortex search "invoice"` | Full-text search over titles, media, files and notes |
| `omacortex timeline --since 2h --kind media` | Raw rows |
| `omacortex note "left off at the auth refactor"` | Leave yourself a checkpoint |
| `omacortex summarize [yesterday]` | Summary from your local model |
| `omacortex ask "when did I last look at the Q3 budget?"` | Ask your local model |
| `omacortex forget --last 15m` | Delete recent activity |
| `omacortex export > history.jsonl` | Everything, as JSON lines |
| `omacortex doctor` | Check sensors, model and permissions |

Every read command takes `--json`.

## Local model

`summarize`, `ask` and the panel's *Summarize today* use an
OpenAI-compatible server on this machine. With `endpoint = "auto"` (the
default) omaCortex looks for, in order:

1. a model from `omarchy agent local` (`omarchy agent local use best`),
2. Ollama on `127.0.0.1:11434`,
3. llama.cpp's `llama-server` on `127.0.0.1:8080`,
4. LM Studio on `127.0.0.1:1234`.

Without a model, everything else still works; summaries fall back to a
structured digest. The prompt tells the model that the log is data, not
instructions, since window titles can contain text written by others.

## MCP for agents

```bash
claude mcp add omacortex -- ~/.local/bin/omacortex mcp
codex mcp add omacortex -- ~/.local/bin/omacortex mcp
```

or in any client's JSON config:

```json
{ "mcpServers": { "omacortex": { "command": "/home/YOU/.local/bin/omacortex", "args": ["mcp"] } } }
```

| Tool | Returns |
|---|---|
| `omacortex_now` | Focused window, recent sessions, what is playing, recent notes |
| `omacortex_sessions` | Sessions per app for a range (default today) |
| `omacortex_timeline` | Raw rows between two times, filtered by kind |
| `omacortex_search` | Full-text search, newest first |
| `omacortex_app_usage` | Time per app |
| `omacortex_day_summary` | Cached local-model summary, or a digest to summarize yourself |
| `omacortex_add_note` | Adds a note to your timeline (turn off with `mcp.allow_notes = false`) |
| `omacortex_status` | Recording state and storage |

Times accept `today`, `yesterday`, `2h`, `30m` or ISO dates. Results are
capped (`mcp.max_results`, default 200).

## Configure

`omacortex config --init` writes a commented
`~/.config/omacortex/config.toml`; every key is optional. Turn sensors off,
add apps, title patterns, workspaces or paths to never record, change
retention, or point at a specific model. A config file that fails to parse
is reported by `status` and `doctor`, and the defaults stay in force.

## How it works

```
Hyprland .socket2.sock ─┐
MPRIS via busctl ───────┼─▶ privacy guard ─▶ recorder ─▶ SQLite + FTS5 ◀── CLI · panel · MCP
recently-used.xbel ─────┘                    ▲                               │
                    shell IdleMonitor ── away/back                   local model (summaries)
```

- **Windows**: Hyprland's event socket (`activewindow`, `windowtitlev2`,
  `workspacev2`). A title change in the focused window is a new span, so
  browser tabs and editor buffers show up separately. Spans under 2 s
  (alt-tab noise) are dropped.
- **Media**: MPRIS players over the session bus, polled every 5 s.
- **Files**: the freedesktop recently-used list that GTK and Qt apps already
  keep. omaCortex never watches the filesystem.
- **Away**: the shell's idle monitor marks you away after 5 minutes. Window
  focus stops counting; media keeps counting, because you may be watching.
- **Crash safety**: a heartbeat every 30 s closes any span a crash left open.

Stdlib-only Python (no pip installs), QML for the panel.

## Develop

```bash
uvx ruff check . && uvx ruff format --check .
uvx pytest
node --test tests/model.test.js
omarchy plugin validate .
omarchy agent look plugin .
```

## License

MIT
