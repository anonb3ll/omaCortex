// What a page says about itself: its site name, its author and whether it is
// an article or a video. Reads only <meta> tags, JSON-LD and, on YouTube, the
// channel name. It never reads the text of the page, and it does nothing
// until the omaCortex extension asks.
(() => {
  const LIMIT = 120

  function clip(value) {
    return String(value || "").replace(/\s+/g, " ").trim().slice(0, LIMIT)
  }

  function content(doc, selector) {
    const el = doc.querySelector(selector)
    return el ? clip(el.getAttribute("content")) : ""
  }

  function jsonLd(doc) {
    const out = []
    const scripts = doc.querySelectorAll('script[type="application/ld+json"]')
    for (let i = 0; i < scripts.length && i < 8; i++) {
      try {
        const parsed = JSON.parse(scripts[i].textContent || "")
        const list = Array.isArray(parsed) ? parsed : parsed && Array.isArray(parsed["@graph"]) ? parsed["@graph"] : [parsed]
        for (const item of list) if (item && typeof item === "object") out.push(item)
      } catch (e) {
        // Malformed JSON-LD is common; ignore it.
      }
    }
    return out
  }

  function types(item) {
    const t = item["@type"]
    return (Array.isArray(t) ? t : [t]).map(String)
  }

  function authorName(author) {
    const first = Array.isArray(author) ? author[0] : author
    if (!first) return ""
    return clip(typeof first === "string" ? first : first.name)
  }

  function readMeta(doc, loc) {
    const host = String(loc.hostname || "").replace(/^www\./, "")
    const path = String(loc.pathname || "")
    const ld = jsonLd(doc)
    const ogType = content(doc, 'meta[property="og:type"]').toLowerCase()

    let kind = "page"
    const isVideoPage =
      (/(^|\.)youtube\.com$/.test(host) && /^\/(watch|shorts\/|live\/)/.test(path)) ||
      (host === "vimeo.com" && /^\/\d+/.test(path)) ||
      /(^|\.)twitch\.tv$/.test(host)
    if (isVideoPage || ogType.indexOf("video") === 0 || ld.some((i) => types(i).indexOf("VideoObject") !== -1)) {
      kind = "video"
    } else if (
      ogType === "article" ||
      ld.some((i) => types(i).some((t) => /^(News|Scholarly|Tech)?Article$|^BlogPosting$|^Report$/.test(t))) ||
      doc.querySelector('meta[property="article:published_time"]')
    ) {
      kind = "article"
    }

    let author = content(doc, 'meta[name="author"]')
    if (!author) {
      const tagged = content(doc, 'meta[property="article:author"]')
      if (tagged && !/^https?:\/\//.test(tagged)) author = tagged
    }
    if (!author) {
      for (const item of ld) {
        author = authorName(item.author)
        if (author) break
      }
    }
    if (/(^|\.)youtube\.com$/.test(host)) {
      // The <meta> tags on YouTube describe the first video opened in the tab;
      // the channel name in the page follows the video that is playing.
      const channel = doc.querySelector("ytd-video-owner-renderer ytd-channel-name a, #owner #channel-name a")
      const name = channel ? clip(channel.textContent) : ""
      author = name || (isVideoPage ? "" : author)
    }

    return {
      site: content(doc, 'meta[property="og:site_name"]'),
      author: author,
      content: kind
    }
  }

  if (typeof chrome !== "undefined" && chrome.runtime && chrome.runtime.onMessage) {
    chrome.runtime.onMessage.addListener((message, sender, reply) => {
      if (message && message.type === "omacortex-meta") reply(readMeta(document, location))
    })
  }
  if (typeof globalThis.__omacortexTest === "object") globalThis.__omacortexTest.readMeta = readMeta
})()
