# What omaCortex records, and what it never does

## Recorded (once you enable it)

| Sensor | Stored | Source |
|---|---|---|
| Windows | App class, window title, workspace name, start and end time | Hyprland event socket |
| Media | Player name, track or video title, artist, page URL without its query string, start and end time | MPRIS on the session bus |
| Files | File name and path, the app that opened it, time | `~/.local/share/recently-used.xbel` |
| Away/back, pause/resume | A marker with a time and no content | The shell's idle monitor, your own actions |
| Notes | The text you (or an agent you connected) wrote | `omacortex note`, MCP `omacortex_add_note` |
| Summaries | The text your local model wrote about a day | `omacortex summarize` |

## Never recorded

- Screenshots, window contents, keystrokes, the clipboard, microphone or camera.
- Anything while recording is off, paused, or before you gave consent.
- Windows of password managers and authentication prompts:
  `1password`, `bitwarden`, `keepassxc`, `proton pass`, `*pinentry*`,
  `*polkit*`, `seahorse`, `gcr-prompter`.
- Windows whose title matches: incognito, private browsing, InPrivate,
  password, sign in / log in, bank / banking, 2FA / two-factor /
  authenticator.
- Workspaces listed in `privacy.exclude_workspaces`.
- Files under `~/.ssh`, `~/.gnupg`, `~/.password-store`,
  `~/.local/share/keyrings`, `~/.config/omacortex`, and files named like keys
  or secrets (`*.key`, `*.pem`, `*.kdbx`, `*.gpg`, `*.asc`, `.env`,
  `id_rsa`, `id_ed25519`, `.netrc`).

Excluded windows are dropped, not renamed: the timeline shows a gap.

## Scrubbed from everything stored

E-mail addresses become `[email]`. API keys and tokens (OpenAI, GitHub,
Slack, AWS, Google, JWTs) and any run of 40+ opaque characters become
`[redacted]`. URLs keep scheme, host and path; user names, passwords, query
strings and fragments are removed. Text is capped at 300 characters.

## Where it lives

- `~/.local/share/omacortex/cortex.db`: the only copy, mode `0600`.
- `~/.local/state/omacortex/`: consent, pause state, the daemon lock and a
  control socket, all `0600` in a `0700` directory.

Nothing is synced, uploaded or sent anywhere. The only network traffic is to
a model server, and only to one on this machine unless you set
`model.allow_remote = true`.

## Deleting

- `omacortex forget --last 15m`, `--since 2026-11-19T09:00 --until …`,
  `--app NAME`, or `--all`.
- Retention (default 30 days) deletes older rows hourly.
- SQLite `secure_delete` overwrites deleted rows; `forget --all` also
  vacuums the database file.
- Removing the plugin keeps your history. Delete it with
  `rm -rf ~/.local/share/omacortex ~/.local/state/omacortex`.

## Agents

The MCP server reads the same database. It can add notes (unless
`mcp.allow_notes = false`) but cannot delete anything, change settings or
resume a paused recorder. Tool descriptions warn agents that recorded titles
are untrusted data, since a web page or document can put any text in a
window title.
