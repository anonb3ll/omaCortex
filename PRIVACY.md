# What omaCortex records, and what it never does

## Recorded (once you enable it)

| Sensor | Stored | Source |
|---|---|---|
| Windows | App class, window title, workspace name, start and end time | Hyprland event socket |
| Media | Player name, track or video title, artist, page URL without its query string, start and end time | MPRIS on the session bus |
| Pages (only if you connect a browser) | Page title, site name, author, whether it is an article or a video, the address without its query string, and time in the foreground | The omaCortex browser extension, over native messaging |
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
- The text of web pages, what you type into them, form fields, cookies or
  anything from private windows (the extension cannot be enabled there).
- Pages on hosts matching `privacy.exclude_hosts`: by default account,
  sign-in and single-sign-on hosts (`accounts.*`, `login.*`, `signin.*`,
  `auth.*`, `sso.*`, `id.*`), anything with `bank` in its name, PayPal, and
  password-manager and identity-provider sites (1Password, Bitwarden,
  LastPass, Okta, Auth0). Pages whose title matches the title rules above
  are dropped too, and anything that is not `http` or `https`.
- Page visits shorter than ten seconds.
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

Page addresses get the same treatment with two differences. A few query
parameters that are the page itself are kept, listed in
`privacy.keep_url_params` (by default the video id on YouTube and the item
id on Hacker News); every other parameter is dropped, so search terms and
tracking tokens never reach the database. And a path segment that is one
long unbroken run of letters and digits, as in a share link, becomes
`[redacted]`, while a long readable slug is kept. The extension strips
query strings before sending, and omaCortex strips them again on arrival,
because it does not trust what a browser extension sends.

## Where it lives

- `~/.local/share/omacortex/cortex.db`: the only copy, mode `0600`.
- `~/.local/state/omacortex/`: consent, pause state, the daemon lock and a
  control socket, all `0600` in a `0700` directory.
- If you ran `omacortex browser install`: one small file per browser,
  `NativeMessagingHosts/io.github.anonb3ll.omacortex.json` in that
  browser's config directory. It names the omacortex program and the
  extension ids allowed to start it, and holds nothing else.
  `omacortex browser uninstall` removes it.

Nothing is synced, uploaded or sent anywhere. The only network traffic is to
a model server, and only to one on this machine unless you set
`model.allow_remote = true`. The browser extension has no network
permission at all: it hands pages to omaCortex through a pipe the browser
opens to a local program, and only the extension ids in that file may use it.

## Deleting

- `omacortex forget --last 15m`, `--since 2026-11-19T09:00 --until …`,
  `--app NAME`, or `--all`.
- `omacortex forget --app youtube.com` deletes every page recorded from
  that site (a page's app is its host name).
- A time window removes a page you looked at during it, even if the tab
  was opened earlier: each page keeps the time of its latest report as
  well as how long it was in front.
- After any forget, no browser visit that started before it is written
  again: it cannot create a new entry or extend one that was kept. The
  extension starts a fresh visit instead.
- Forgetting an app or site that had entries also drops the cached day
  summaries, which may quote it.
- Retention runs hourly in two tiers. Window focus history, the bulk of
  what is recorded, is deleted after `privacy.retention_days` (default 30).
  What you watched, listened to, read, opened and noted is deleted after
  `privacy.library_days` (default 365), so "what was that video last
  month?" still has an answer. Set either to a smaller number, or to 0 to
  keep that tier until you delete it. Versions before 0.3 deleted
  everything after `retention_days`.
- SQLite `secure_delete` overwrites deleted rows; `forget --all` also
  vacuums the database file.
- Removing the plugin keeps your history. Delete it with
  `rm -rf ~/.local/share/omacortex ~/.local/state/omacortex`.

## Agents

The MCP server reads the same database. It can add notes (unless
`mcp.allow_notes = false`) but cannot delete anything, change settings or
resume a paused recorder. Tool descriptions warn agents that recorded titles
are untrusted data, since a web page or document can put any text in a
window title. The same goes for page titles and author names, which the
page itself chooses.
