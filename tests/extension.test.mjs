import { test } from "node:test"
import assert from "node:assert/strict"
import * as Visit from "../extension/visit.js"

globalThis.__omacortexTest = {}
await import("../extension/meta.js")
const { readMeta } = globalThis.__omacortexTest

const T0 = 1_800_000_000_000
const page = (tabId, url, title) => ({ tabId, url, title })

test("addresses lose queries, fragments and credentials", () => {
  assert.equal(Visit.minimizeUrl("https://user:pw@example.com:8443/a/b?utm=1&q=secret#frag"), "https://example.com:8443/a/b")
  assert.equal(Visit.minimizeUrl("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s&list=PL1"), "https://www.youtube.com/watch?v=dQw4w9WgXcQ")
  assert.equal(Visit.minimizeUrl("https://evil.example/watch?v=dQw4w9WgXcQ"), "https://evil.example/watch")
  assert.equal(Visit.minimizeUrl("https://notyoutube.com/watch?v=x"), "https://notyoutube.com/watch")
  for (const raw of ["chrome://extensions", "file:///home/u/a.html", "about:blank", "", null, "not a url"]) {
    assert.equal(Visit.minimizeUrl(raw), "")
  }
})

test("titles lose notification counts and the YouTube suffix", () => {
  assert.equal(Visit.cleanTitle("(3) Async Rust in practice - YouTube", "https://www.youtube.com/watch?v=1"), "Async Rust in practice")
  assert.equal(Visit.cleanTitle("Notes - YouTube", "https://example.com/"), "Notes - YouTube")
  assert.equal(Visit.cleanTitle("x".repeat(500), "").length, 300)
})

test("a short visit is never sent; a long one is sent when you leave", () => {
  let { state, send } = Visit.show(Visit.emptyState(), page(1, "https://a.example/one", "One"), T0)
  assert.deepEqual(send, [])
  ;({ state, send } = Visit.show(state, page(2, "https://b.example/two?x=1", "Two"), T0 + 4000))
  assert.deepEqual(send, [], "4 seconds on a page is not a visit")
  ;({ state, send } = Visit.show(state, page(1, "https://a.example/one", "One"), T0 + 49000))
  assert.deepEqual(send, [
    { url: "https://b.example/two", title: "Two", site: "", author: "", content: "page", seconds: 45, at: (T0 + 4000) / 1000 }
  ])
})

test("a long visit is reported while it lasts, with the same start", () => {
  let { state, send } = Visit.show(Visit.emptyState(), page(7, "https://www.youtube.com/watch?v=abc", "A talk - YouTube"), T0)
  state = Visit.setMeta(state, 7, { site: "YouTube", author: "A Channel", content: "video" })
  const sends = []
  for (let minute = 1; minute <= 3; minute++) {
    ;({ state, send } = Visit.tick(state, T0 + minute * 60000))
    sends.push(...send)
  }
  assert.deepEqual(sends.map((m) => m.seconds), [60, 120, 180])
  assert.ok(sends.every((m) => m.at === T0 / 1000 && m.title === "A talk" && m.author === "A Channel" && m.content === "video"))
  // Nothing new to say: no message.
  ;({ state, send } = Visit.pause(state, T0 + 180000))
  assert.deepEqual(send, [])
})

test("looking away pauses the visit and coming back continues it", () => {
  let { state, send } = Visit.show(Visit.emptyState(), page(1, "https://a.example/long-read", "Long read"), T0)
  ;({ state, send } = Visit.pause(state, T0 + 30000))
  assert.equal(send[0].seconds, 30)
  // Ten minutes in another app do not count.
  ;({ state, send } = Visit.tick(state, T0 + 400000))
  assert.deepEqual(send, [])
  ;({ state, send } = Visit.show(state, page(1, "https://a.example/long-read#part-2", "Long read"), T0 + 630000))
  ;({ state, send } = Visit.pause(state, T0 + 650000))
  assert.equal(send[0].seconds, 50)
  assert.equal(send[0].at, T0 / 1000)
})

test("time the computer slept is not counted", () => {
  let { state, send } = Visit.show(Visit.emptyState(), page(1, "https://a.example/", "A"), T0)
  ;({ state, send } = Visit.tick(state, T0 + 8 * 3600 * 1000))
  assert.equal(send[0].seconds, Visit.MAX_RUN_MS / 1000)
})

test("a page that is not a web page ends the visit and starts none", () => {
  let { state, send } = Visit.show(Visit.emptyState(), page(1, "https://a.example/", "A"), T0)
  ;({ state, send } = Visit.show(state, page(1, "chrome://settings", "Settings"), T0 + 20000))
  assert.equal(send.length, 1)
  assert.equal(state.visit, null)
  assert.deepEqual(Visit.tick(state, T0 + 90000).send, [])
})

// A stand-in for a document: answers the few selectors meta.js uses.
function fakeDoc({ meta = {}, ld = [], channel = "" }) {
  return {
    querySelector(selector) {
      if (selector.indexOf("ytd-video-owner-renderer") === 0) return channel ? { textContent: channel } : null
      const m = selector.match(/^meta\[(?:property|name)="([^"]+)"\]$/)
      if (!m || !(m[1] in meta)) return null
      return { getAttribute: () => meta[m[1]] }
    },
    querySelectorAll() {
      return ld.map((item) => ({ textContent: typeof item === "string" ? item : JSON.stringify(item) }))
    }
  }
}

test("a page's own description gives site, author and kind", () => {
  assert.deepEqual(
    readMeta(
      fakeDoc({ meta: { "og:site_name": "The Writer", "og:type": "article", author: "  A.  Writer " } }),
      { hostname: "writer.substack.com", pathname: "/p/small-models" }
    ),
    { site: "The Writer", author: "A. Writer", content: "article" }
  )
  assert.deepEqual(
    readMeta(
      fakeDoc({ ld: ["{ not json", { "@graph": [{ "@type": ["NewsArticle"], author: [{ name: "Reporter One" }] }] }] }),
      { hostname: "news.example", pathname: "/2026/story" }
    ),
    { site: "", author: "Reporter One", content: "article" }
  )
  assert.deepEqual(readMeta(fakeDoc({}), { hostname: "example.com", pathname: "/" }), { site: "", author: "", content: "page" })
  // An author given as a profile address is not a name.
  assert.equal(
    readMeta(fakeDoc({ meta: { "article:author": "https://social.example/someone" } }), { hostname: "x.example", pathname: "/" }).author,
    ""
  )
})

test("on YouTube the channel comes from the page, not from stale tags", () => {
  const watch = { hostname: "www.youtube.com", pathname: "/watch" }
  assert.deepEqual(readMeta(fakeDoc({ meta: { "og:site_name": "YouTube", author: "Stale" }, channel: " A Channel " }), watch), {
    site: "YouTube",
    author: "A Channel",
    content: "video"
  })
  assert.equal(readMeta(fakeDoc({ meta: { author: "Stale" } }), watch).author, "")
  assert.equal(readMeta(fakeDoc({}), { hostname: "www.youtube.com", pathname: "/feed/subscriptions" }).content, "page")
})
