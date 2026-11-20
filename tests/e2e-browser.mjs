// End-to-end check of the browser path with a real Chromium: the extension in
// extension/ reports a page through native messaging to a freshly built
// omacortex, and the visit shows up in `omacortex recall`.
//
// Not part of `cargo test`: it needs chromium and takes about a minute.
//   cargo build && node tests/e2e-browser.mjs
// CI runs it with the release binary. CHROMIUM_ARGS adds runner-only flags.
// Everything lives in a temporary directory; your own omaCortex data, browser
// profile and native-messaging hosts are not touched. Chromium runs headless.
import { spawn, execFileSync } from "node:child_process"
import { createServer } from "node:http"
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, chmodSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve, dirname } from "node:path"
import { fileURLToPath } from "node:url"

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const binary = process.env.OMACORTEX_BIN || join(root, "target/debug/omacortex")
const chromium = process.env.CHROMIUM || "chromium"
const chromiumArgs = (process.env.CHROMIUM_ARGS || "").split(/\s+/).filter(Boolean)
const tmp = mkdtempSync(join(tmpdir(), "omacortex-e2e-"))
const env = Object.assign({}, process.env, {
  XDG_CONFIG_HOME: join(tmp, "config"),
  XDG_DATA_HOME: join(tmp, "data"),
  XDG_STATE_HOME: join(tmp, "state")
})
const profile = join(env.XDG_CONFIG_HOME, "chromium")
const cli = (...args) => execFileSync(binary, args, { env, encoding: "utf8" })
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))
let browser = null
let server = null

function fail(message) {
  console.error("FAIL: " + message)
  cleanup()
  process.exit(1)
}

function cleanup() {
  if (browser) browser.kill("SIGKILL")
  if (server) server.close()
  rmSync(tmp, { recursive: true, force: true })
}

const article = `<!doctype html><html><head><title>Why small models win</title>
<meta property="og:site_name" content="The Writer"><meta property="og:type" content="article">
<meta name="author" content="A. Writer"></head><body><article><h1>Why small models win</h1><p>Body text that must never be stored.</p></article></body></html>`
const other = `<!doctype html><html><head><title>Something else</title></head><body>Another page</body></html>`

async function main() {
  // A wrapper so the host the browser starts uses the temporary directories.
  mkdirSync(profile, { recursive: true })
  const wrapper = join(tmp, "omacortex-host")
  writeFileSync(
    wrapper,
    `#!/bin/sh\nexport XDG_CONFIG_HOME='${env.XDG_CONFIG_HOME}' XDG_DATA_HOME='${env.XDG_DATA_HOME}' XDG_STATE_HOME='${env.XDG_STATE_HOME}'\nexec '${binary}' "$@"\n`
  )
  chmodSync(wrapper, 0o755)
  console.log(cli("browser", "install", "--browser", "chromium").trim())
  const manifestPath = join(profile, "NativeMessagingHosts", "io.github.anonb3ll.omacortex.json")
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"))
  manifest.path = wrapper
  writeFileSync(manifestPath, JSON.stringify(manifest))
  cli("enable")

  server = createServer((req, res) => {
    res.writeHead(200, { "content-type": "text/html" })
    res.end(req.url.startsWith("/p/") ? article : other)
  })
  await new Promise((r) => server.listen(0, "127.0.0.1", r))
  const site = "http://127.0.0.1:" + server.address().port

  const extension = join(root, "extension")
  browser = spawn(
    chromium,
    [
      "--headless=new",
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-gpu",
      "--user-data-dir=" + profile,
      "--load-extension=" + extension,
      "--disable-extensions-except=" + extension,
      "--disable-features=DisableLoadExtensionCommandLineSwitch",
      "--remote-debugging-port=0",
      ...chromiumArgs,
      "about:blank"
    ],
    { env, stdio: ["ignore", "ignore", "pipe"] }
  )
  let stderr = ""
  browser.stderr.on("data", (d) => (stderr += d))
  let port = 0
  for (let i = 0; i < 100 && !port; i++) {
    await sleep(100)
    const m = stderr.match(/DevTools listening on ws:\/\/127\.0\.0\.1:(\d+)\//)
    if (m) port = Number(m[1])
  }
  if (!port) fail("chromium did not start: " + stderr.slice(0, 400))
  const devtools = "http://127.0.0.1:" + port
  const open = (url) => fetch(devtools + "/json/new?" + encodeURIComponent(url), { method: "PUT" }).then((r) => r.json())

  await sleep(1500)
  await open(site + "/p/small-models?utm_source=newsletter&token=SECRET#comments")
  console.log("reading the article for 14 seconds...")
  await sleep(14000)
  await open(site + "/elsewhere")

  // Leaving the page reports it; allow for the worker and the host to finish.
  let rows = []
  for (let i = 0; i < 20 && rows.length === 0; i++) {
    await sleep(500)
    rows = JSON.parse(cli("timeline", "--since", "1h", "--kind", "page", "--json"))
  }
  if (rows.length === 0) {
    const targets = await fetch(devtools + "/json/list").then((r) => r.json())
    fail("no page was recorded. Targets: " + targets.map((t) => t.type + " " + t.url).join(", "))
  }
  const row = rows[0]
  console.log(JSON.stringify(row))
  const expect = (ok, what) => (ok ? console.log("ok   " + what) : fail(what + " (got " + JSON.stringify(row) + ")"))
  expect(row.title === "Why small models win", "title is the page title")
  expect(row.detail === "The Writer · A. Writer · " + site + "/p/small-models", "site, author and the address without query or fragment")
  expect(!JSON.stringify(rows).includes("SECRET") && !JSON.stringify(rows).includes("Body text"), "no query string and no page text")
  expect(row.seconds >= 10 && row.seconds <= 30, "foreground time is about 14 seconds")
  expect(row.source === "browser:companion", "source names the extension")
  const recalled = JSON.parse(cli("recall", "--json", "that article about small models today"))
  expect(recalled.items.length === 1 && recalled.items[0].content === "article" && recalled.items[0].creator === "A. Writer", "recall finds it as an article by its author")
  cleanup()
  console.log("PASS")
}

main().catch((e) => fail(String((e && e.stack) || e)))
