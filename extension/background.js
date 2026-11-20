// omaCortex browser extension: tells omaCortex which page is in front of you.
//
// It sends the address (without its query string), the title, the site, the
// author and how long the page was in the foreground to the omaCortex program
// on this computer, over Chrome's native messaging. It makes no network
// requests, reads no page text and does not run in private windows.
import * as Visit from "./visit.js"

const HOST = "io.github.anonb3ll.omacortex"
const ALARM = "omacortex-tick"
const REPORT_TTL_MS = 120000
const MAX_PENDING_REPORTS = 16
const RETRY_DELAY_MS = 5000

// Events arrive in bursts (a tab switch fires several); handle them one at a time.
let queue = Promise.resolve()
function serial(work) {
  queue = queue.then(work).catch(() => {})
  return queue
}

async function isEnabled() {
  const stored = await chrome.storage.local.get("enabled")
  return stored.enabled !== false
}

/** The page to count: the active tab of the focused window, or else a tab that is playing sound. */
async function currentPage() {
  let tab = null
  try {
    const win = await chrome.windows.getLastFocused()
    if (win && win.focused) [tab] = await chrome.tabs.query({ active: true, windowId: win.id })
  } catch (e) {
    tab = null
  }
  if (!tab) {
    // A video playing while another app has focus is still being watched.
    const playing = await chrome.tabs.query({ active: true, audible: true })
    tab = playing[0] || null
  }
  if (!tab || tab.incognito || !tab.url) return null
  return { tabId: tab.id, url: tab.url, title: tab.title || "", audible: !!tab.audible }
}

async function pageMeta(tabId) {
  try {
    return await chrome.tabs.sendMessage(tabId, { type: "omacortex-meta" })
  } catch (e) {
    return null // pages opened before the extension was loaded have no content script
  }
}

function sameVisit(left, right) {
  return !!right && left.url === right.url && left.at === right.at
}

async function deliver(state, pending) {
  while (pending.length) {
    const { message } = pending[0]
    let reply = null
    let error
    try {
      reply = await chrome.runtime.sendNativeMessage(HOST, { v: 1, type: "page", source: "companion", ...message })
    } catch (e) {
      error = String((e && e.message) || e)
    }
    const at = Date.now()
    const host = { ok: !!reply?.ok, reply, error: error || "", at }
    if (!reply || reply.retryable === true) {
      await chrome.storage.session.set({ host, retryAt: at + RETRY_DELAY_MS })
      return
    }
    pending.shift()
    if (reply.recording === false) {
      state = Visit.emptyState()
      pending.length = 0
    } else if (reply.reason === "before recording resumed" || reply.reason === "visit forgotten") {
      const stale = visit => sameVisit(message, visit) ||
        (reply.reason === "before recording resumed" && visit?.at < reply.recording_since)
      if (stale(state.visit)) state = Visit.emptyState()
      pending = pending.filter(item => !stale(item.message))
    }
    await chrome.storage.session.set({ state, pending, host, retryAt: 0 })
  }
}

async function update(step) {
  const now = Date.now()
  if (!(await isEnabled())) {
    await chrome.storage.session.set({ state: Visit.emptyState(), pending: [], retryAt: 0 })
    return
  }
  const stored = await chrome.storage.session.get(["state", "pending", "retryAt"])
  let state = stored.state || Visit.emptyState()
  let send = []
  if (step === "tick") {
    // Ask again what the page says about itself: a page that was still
    // loading, or one that changed without reloading, has caught up by now.
    if (state.visit && state.visit.since !== null) {
      state = Visit.setMeta(state, state.visit.tabId, await pageMeta(state.visit.tabId))
    }
    ;({ state, send } = Visit.tick(state, now))
  } else {
    const page = await currentPage()
    const idle = await chrome.idle.queryState(300)
    const away = !page || idle === "locked" || (idle === "idle" && !page.audible)
    if (!away && state.visit && state.visit.tabId === page.tabId && state.visit.url === Visit.minimizeUrl(page.url)) {
      state = Visit.setMeta(state, page.tabId, await pageMeta(page.tabId))
    }
    ;({ state, send } = away ? Visit.pause(state, now) : Visit.show(state, page, now))
    if (!away && state.visit && !state.visit.meta) {
      state = Visit.setMeta(state, page.tabId, await pageMeta(page.tabId))
    }
  }
  let pending = (stored.pending || []).filter(item => now - item.queuedAt < REPORT_TTL_MS)
  for (const message of send) {
    const retry = pending.find(item => sameVisit(message, item.message))
    if (retry) retry.message = message
    else pending.push({ message, queuedAt: now })
  }
  pending = pending.slice(-MAX_PENDING_REPORTS)
  await chrome.storage.session.set({ state, pending })
  if (now >= (stored.retryAt || 0)) await deliver(state, pending)
}

function refresh() {
  return serial(() => update("refresh"))
}

chrome.tabs.onActivated.addListener(refresh)
chrome.tabs.onRemoved.addListener(refresh)
chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.url || change.title || change.audible !== undefined || change.status === "complete") refresh()
})
chrome.windows.onFocusChanged.addListener(refresh)
chrome.idle.onStateChanged.addListener(refresh)
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.enabled) refresh()
})
chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name !== ALARM) return
  // Re-check what is in front first (focus changes are sometimes missed), then count the minute.
  refresh()
  serial(() => update("tick"))
})

async function start() {
  if (!(await chrome.alarms.get(ALARM))) chrome.alarms.create(ALARM, { periodInMinutes: 1 })
  refresh()
}
chrome.runtime.onInstalled.addListener(start)
chrome.runtime.onStartup.addListener(start)
start()
