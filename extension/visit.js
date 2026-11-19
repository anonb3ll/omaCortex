// Pure logic for the omaCortex browser extension: which page is in front of
// you and for how long. No browser APIs and no clock reads (callers pass the
// time in milliseconds), so it runs unchanged in the service worker and in
// node.
//
// A visit is one stretch with a page: it starts when the page comes to the
// front, pauses when you look away, and continues if you come back to the
// same tab and address. The extension reports a visit several times with a
// growing number of seconds; omaCortex recognises it by its address and start
// time and lengthens the same row.

/** Visits shorter than this are never sent. omaCortex applies its own minimum too. */
export const MIN_SECONDS = 10

/**
 * The longest stretch counted in one go. The background worker folds time in
 * every minute; a longer gap means the computer slept or the worker was
 * stopped, and that time was not spent reading.
 */
export const MAX_RUN_MS = 150000

/** Query parameters that are part of what a page is, per host. */
const KEEP_PARAMS = { "youtube.com": ["v"], "news.ycombinator.com": ["id"] }

/**
 * An address safe to send: http(s) only, no credentials, no fragment, and no
 * query string apart from the few parameters that name the page itself.
 * Returns "" for anything that is not a web page.
 */
export function minimizeUrl(raw) {
  let url
  try {
    url = new URL(String(raw || ""))
  } catch (e) {
    return ""
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return ""
  const host = url.hostname.replace(/^www\./, "")
  const keep = []
  for (const site of Object.keys(KEEP_PARAMS)) {
    if (host !== site && !host.endsWith("." + site)) continue
    for (const name of KEEP_PARAMS[site]) {
      const value = url.searchParams.get(name)
      if (value && value.length <= 64) keep.push(name + "=" + encodeURIComponent(value))
    }
  }
  return url.protocol + "//" + url.host + url.pathname + (keep.length ? "?" + keep.join("&") : "")
}

/** "(3) A talk - YouTube" becomes "A talk". */
export function cleanTitle(title, url) {
  let t = String(title || "").replace(/\s+/g, " ").trim()
  t = t.replace(/^\(\d+\+?\)\s+/, "")
  if (/(^|\.)youtube\.com\//.test(String(url || "").replace(/^https?:\/\//, ""))) t = t.replace(/\s+-\s+YouTube$/, "")
  return t.slice(0, 300)
}

export function emptyState() {
  return { visit: null }
}

function fold(visit, nowMs) {
  if (visit.since === null) return visit
  const run = Math.max(0, nowMs - visit.since)
  return Object.assign({}, visit, { activeMs: visit.activeMs + Math.min(run, MAX_RUN_MS), since: nowMs })
}

/** The message for a visit, or null if there is nothing new worth sending. */
function report(visit) {
  const seconds = Math.floor(visit.activeMs / 1000)
  if (seconds < MIN_SECONDS || visit.activeMs <= visit.sentMs || !visit.title) return null
  const meta = visit.meta || {}
  return {
    url: visit.url,
    title: visit.title,
    site: meta.site || "",
    author: meta.author || "",
    content: meta.content || "page",
    seconds: seconds,
    at: visit.at
  }
}

function sent(visit, message) {
  return message ? Object.assign({}, visit, { sentMs: visit.activeMs }) : visit
}

/**
 * A page is in front of you: `page` is { tabId, url, title }. Continues the
 * current visit if it is the same tab and address, otherwise ends that one
 * (reporting it) and starts a new one.
 */
export function show(state, page, nowMs) {
  const url = minimizeUrl(page.url)
  const send = []
  let visit = state.visit
  if (visit && (visit.tabId !== page.tabId || visit.url !== url)) {
    visit = fold(visit, nowMs)
    const message = report(visit)
    if (message) send.push(message)
    visit = null
  }
  if (!url) return { state: { visit: null }, send: send }
  const title = cleanTitle(page.title, url)
  if (!visit) {
    visit = { tabId: page.tabId, url: url, title: title, at: nowMs / 1000, activeMs: 0, sentMs: 0, since: nowMs, meta: null }
  } else {
    visit = fold(visit, nowMs)
    visit = Object.assign({}, visit, { title: title || visit.title, since: nowMs })
  }
  return { state: { visit: visit }, send: send }
}

/** You looked away (another app, a locked screen, no page). Keeps the visit so it can continue. */
export function pause(state, nowMs) {
  if (!state.visit) return { state: state, send: [] }
  let visit = fold(state.visit, nowMs)
  const message = report(visit)
  visit = Object.assign({}, sent(visit, message), { since: null })
  return { state: { visit: visit }, send: message ? [message] : [] }
}

/** Called every minute: counts the time so far and reports a long visit while it is still going. */
export function tick(state, nowMs) {
  if (!state.visit || state.visit.since === null) return { state: state, send: [] }
  let visit = fold(state.visit, nowMs)
  const message = report(visit)
  visit = sent(visit, message)
  return { state: { visit: visit }, send: message ? [message] : [] }
}

/** Attach what the page says about itself (site, author, kind) to the current visit. */
export function setMeta(state, tabId, meta) {
  const visit = state.visit
  if (!visit || visit.tabId !== tabId || !meta) return state
  return { visit: Object.assign({}, visit, { meta: { site: meta.site || "", author: meta.author || "", content: meta.content || "page" } }) }
}
