const { test } = require("node:test")
const assert = require("node:assert/strict")
const Model = require("../Model.js")

const NOW = 1_800_000_000
const base = (status, extra) =>
  Object.assign(
    { status: Object.assign({ consent: true, paused: false, paused_until: null, daemon: true, away: false }, status) },
    extra || {}
  )

test("parse rejects junk and accepts panel JSON", () => {
  assert.equal(Model.parse("not json"), null)
  assert.equal(Model.parse("{}"), null)
  assert.deepEqual(Model.parse(JSON.stringify(base({}))).status.consent, true)
})

test("durations and app names read naturally", () => {
  assert.equal(Model.duration(42), "42s")
  assert.equal(Model.duration(600), "10m")
  assert.equal(Model.duration(3720), "1h 02m")
  assert.equal(Model.duration(7200), "2h")
  assert.equal(Model.appName("com.mitchellh.ghostty"), "Ghostty")
  assert.equal(Model.appName("org.gnome.Nautilus"), "Nautilus")
  assert.equal(Model.appName("brave-browser"), "Brave browser")
  assert.equal(Model.appName(""), "Unknown")
})

test("titles lose the trailing app name and stay short", () => {
  assert.equal(Model.shortTitle("Pull request #3 - GitHub - Chromium", "chromium"), "Pull request #3 - GitHub")
  assert.equal(Model.shortTitle("nvim notes.md", "kitty"), "nvim notes.md")
  assert.equal(Model.shortTitle("x".repeat(100), "kitty", 10).length, 10)
})

test("headline covers every state", () => {
  assert.equal(Model.headline(null, NOW).key, "loading")
  assert.equal(Model.headline(base({ consent: false }), NOW).key, "off")
  assert.equal(Model.headline(base({ paused: true }), NOW).key, "paused")
  assert.equal(Model.headline(base({ paused: true, paused_until: NOW + 600 }), NOW).key, "paused-until")
  assert.equal(Model.headline(base({ daemon: false }), NOW).key, "stopped")
  assert.equal(Model.headline(base({ away: true }), NOW).key, "away")
  assert.equal(Model.headline(base({}), NOW).key, "recording")
})

test("rows are bounded and markup-shaped text passes through as plain data", () => {
  const evil = '<img src="https://example.invalid/t">'
  const data = base({}, {
    current: { app: "chromium", title: evil, seconds: 90 },
    sessions: Array.from({ length: 20 }, (_, i) => ({ app: "kitty", title: evil, started_at: NOW + i, active_seconds: 60 })),
    media: [{ app: "spotify", title: "Song", started_at: NOW, seconds: 200, ongoing: true }],
    files: [{ title: "plan.md", started_at: NOW }],
    notes: [{ title: "Agent: done", started_at: NOW }],
    usage: [{ app: "kitty", seconds: 100 }, { app: "chromium", seconds: 50 }]
  })
  const rows = Model.sessionRows(data)
  assert.equal(rows.length, 8)
  assert.equal(rows[0].title, evil) // rendered with Text.PlainText in the panel
  assert.equal(Model.mediaRows(data)[0].ongoing, true)
  assert.equal(Model.fileRows(data)[0].title, "plan.md")
  assert.equal(Model.noteRows(data)[0].title, "Agent: done")
  assert.equal(Model.totalFocus(data), 150)
  assert.match(Model.tooltip(data, NOW), /Chromium for 1m/)
})
