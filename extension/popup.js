const HOST = "io.github.anonb3ll.omacortex"
const state = document.getElementById("state")
const detail = document.getElementById("detail")
const enabled = document.getElementById("enabled")

function show(headline, more) {
  state.textContent = headline
  detail.textContent = more || ""
}

async function check() {
  const stored = await chrome.storage.local.get("enabled")
  enabled.checked = stored.enabled !== false
  if (!enabled.checked) return show("Off in this browser", "Tick the box to remember pages again.")
  try {
    const reply = await chrome.runtime.sendNativeMessage(HOST, { v: 1, type: "status" })
    if (!reply || !reply.ok) return show("omaCortex answered with an error", reply && reply.error)
    if (!reply.capture) return show("Browser pages are turned off", "capture.browser is false in the omaCortex config.")
    if (!reply.consent) return show("omaCortex is not recording yet", "Start it from the bar panel or run: omacortex enable")
    if (reply.paused) return show("omaCortex is paused", "Pages are not remembered until you resume.")
    return show("Remembering pages", "omaCortex " + reply.version + ". Visits under " + reply.min_seconds + " seconds are skipped.")
  } catch (e) {
    return show("Not connected to omaCortex", "Run in a terminal: omacortex browser install")
  }
}

enabled.addEventListener("change", async () => {
  await chrome.storage.local.set({ enabled: enabled.checked })
  check()
})
check()
