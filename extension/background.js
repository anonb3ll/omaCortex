// omaCortex browser extension: tells omaCortex which page is in front of you.
//
// It sends the address (without its query string), the title, the site, the
// author and how long the page was in the foreground to the omaCortex program
// on this computer, over Chrome's native messaging. It makes no network
// requests, reads no page text and does not run in private windows.
import * as Visit from "./visit.js"

const HOST = "io.github.anonb3ll.omacortex"
const ALARM = "omacortex-tick"

// Events arrive in bursts (a tab switch fires several); handle them one at a time.
let queue = Promise.resolve()
function serial(work) {
  queue = queue.then(work).catch(() => {})
  return queue
}

async function loadState() {
  const stored = await chrome.storage.session.get("state")
  return stored.state || Visit.emptyState()
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

async function deliver(messages) {
  for (const message of messages) {
    let result
    try {
      const reply = await chrome.runtime.sendNativeMessage(HOST, Object.assign({ v: 1, type: "page", source: "companion" }, message))
      result = { ok: !!(reply && reply.ok), reply: reply || null, at: Date.now() }
    } catch (e) {
      result = { ok: false, error: String((e && e.message) || e), at: Date.now() }
    }
    await chrome.storage.session.set({ host: result })
  }
}

async function update(step) {
  const now = Date.now()
  if (!(await isEnabled())) {
    await chrome.storage.session.set({ state: Visit.emptyState() })
    return
  }
  let state = await loadState()
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
  await chrome.storage.session.set({ state: state })
  await deliver(send)
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
