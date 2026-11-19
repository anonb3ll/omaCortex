// Pure display logic for the omaCortex panel: no QML, no clock reads (callers
// pass `now` in epoch seconds), so it runs unchanged in the shell and in node.

function parse(text) {
  try {
    var data = JSON.parse(String(text || ""))
    return data && typeof data === "object" && data.status ? data : null
  } catch (e) {
    return null
  }
}

function duration(seconds) {
  var s = Math.max(0, Math.floor(Number(seconds) || 0))
  if (s < 60) return s + "s"
  var m = Math.floor(s / 60)
  if (m < 60) return m + "m"
  var h = Math.floor(m / 60)
  m = m % 60
  return m ? h + "h " + (m < 10 ? "0" : "") + m + "m" : h + "h"
}

function clock(ts) {
  var d = new Date(Number(ts) * 1000)
  var hh = d.getHours()
  var mm = d.getMinutes()
  return (hh < 10 ? "0" : "") + hh + ":" + (mm < 10 ? "0" : "") + mm
}

// "com.mitchellh.ghostty" -> "Ghostty", "org.gnome.Nautilus" -> "Nautilus", "chromium" -> "Chromium"
function appName(app) {
  var raw = String(app || "").trim()
  if (!raw) return "Unknown"
  var parts = raw.split(".")
  var last = parts[parts.length - 1] || raw
  if (/^(desktop|app|bin)$/i.test(last) && parts.length > 1) last = parts[parts.length - 2]
  last = last.replace(/[-_]+/g, " ")
  return last.charAt(0).toUpperCase() + last.slice(1)
}

// Window titles often end in " - Chromium" / " — Mozilla Firefox": drop the app suffix.
function shortTitle(title, app, limit) {
  var t = String(title || "").trim()
  var name = appName(app).toLowerCase()
  var m = t.match(/^(.*\S)\s+[-—–|]\s+([^-—–|]+)$/)
  if (m && m[2].toLowerCase().indexOf(name) !== -1) t = m[1]
  limit = limit || 60
  return t.length > limit ? t.slice(0, limit - 1) + "…" : t
}

// One headline for the panel and the bar tooltip.
function headline(data, now, missing) {
  if (missing) return { key: "missing", text: "Recorder not installed", urgent: true }
  if (!data) return { key: "loading", text: "Loading…", urgent: false }
  var s = data.status || {}
  if (!s.consent) return { key: "off", text: "Not recording", urgent: false }
  if (s.paused) {
    if (s.paused_until) return { key: "paused-until", text: "Paused until " + clock(s.paused_until), urgent: true }
    return { key: "paused", text: "Paused", urgent: true }
  }
  if (!s.daemon) return { key: "stopped", text: "Recorder not running", urgent: true }
  if (s.away) return { key: "away", text: "Recording · you are away", urgent: false }
  return { key: "recording", text: "Recording", urgent: false }
}

function tooltip(data, now) {
  var h = headline(data, now)
  var cur = data && data.current
  if (h.key === "recording" && cur) {
    return "omaCortex · " + appName(cur.app) + " for " + duration(cur.seconds)
  }
  return "omaCortex · " + h.text
}

function sessionRows(data) {
  var rows = (data && data.sessions) || []
  var out = []
  for (var i = 0; i < rows.length && out.length < 8; i++) {
    var s = rows[i]
    out.push({
      time: clock(s.started_at),
      length: duration(s.active_seconds),
      app: appName(s.app),
      title: shortTitle(s.title, s.app, 56),
      ongoing: !!s.ongoing
    })
  }
  return out
}

function mediaRows(data) {
  var rows = (data && data.media) || []
  var out = []
  for (var i = 0; i < rows.length && out.length < 4; i++) {
    var m = rows[i]
    out.push({
      time: clock(m.started_at),
      length: duration(m.seconds),
      title: shortTitle(m.title, "", 56),
      source: appName(m.app),
      ongoing: !!m.ongoing
    })
  }
  return out
}

function fileRows(data) {
  var rows = (data && data.files) || []
  var out = []
  for (var i = 0; i < rows.length && out.length < 4; i++) {
    out.push({ time: clock(rows[i].started_at), title: shortTitle(rows[i].title, "", 56) })
  }
  return out
}

function noteRows(data) {
  var rows = (data && data.notes) || []
  var out = []
  for (var i = 0; i < rows.length && out.length < 3; i++) {
    out.push({ time: clock(rows[i].started_at), title: shortTitle(rows[i].title, "", 72) })
  }
  return out
}

function totalFocus(data) {
  var rows = (data && data.usage) || []
  var total = 0
  for (var i = 0; i < rows.length; i++) total += Number(rows[i].seconds) || 0
  return total
}

if (typeof module !== "undefined") {
  module.exports = {
    parse: parse,
    duration: duration,
    clock: clock,
    appName: appName,
    shortTitle: shortTitle,
    headline: headline,
    tooltip: tooltip,
    sessionRows: sessionRows,
    mediaRows: mediaRows,
    fileRows: fileRows,
    noteRows: noteRows,
    totalFocus: totalFocus
  }
}
