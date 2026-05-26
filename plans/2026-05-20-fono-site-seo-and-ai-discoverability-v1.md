# Fono Site — SEO & AI/Agent Discoverability

## Objective

Make `fono.page` rank well in Google / Bing / DuckDuckGo for its target queries (Linux voice dictation, local Whisper dictation, open-source Linux speech-to-text, voice assistant for Linux, etc.) and make it cleanly ingestible by AI agents, LLM crawlers (GPTBot, ClaudeBot, PerplexityBot, Google-Extended), and code-indexers — without bloating the single-file static site or changing the editorial voice.

Scope is **discoverability only**: meta, structured data, crawl hints, social cards, content semantics, performance signals. No design changes, no new pages unless strictly required.

## Initial Assessment

### Project structure summary
- Single-page static site on GitHub Pages, custom domain `fono.page` (`CNAME:1`).
- Entry point `index.html:1-917` carries all content, inline CSS, and inline JS.
- Shared assets: `shared/fono.css`, `shared/fono-oscilloscope.js`, `shared/typed-terminal.js`.
- Sibling demo pages exist under `assets/` (`fono-oscilloscope-demo.html`, `fono-fft-demo.html`, `fono-assistant-demo.html`, `fono-demo.html`) and are reachable but unlinked from `index.html`.
- No `robots.txt`, no `sitemap.xml`, no `llms.txt`, no `humans.txt`, no `favicon`, no Open Graph image, no JSON-LD, no `.well-known/`.
- README confirms GitHub Pages deploy from `site` branch (`README.md:36-45`).

### What's already good
- Valid `<!DOCTYPE html>`, `lang="en"`, viewport, charset (`index.html:1-5`).
- Sensible `<title>` and `meta description` (`index.html:6-7`).
- `link rel="canonical"` to root (`index.html:9`).
- Open Graph `og:title`, `og:description`, `og:url`, `og:type` (`index.html:10-13`).
- `twitter:card = summary_large_image` declared (`index.html:14`).
- `theme-color` set (`index.html:8`).
- Single, semantically correct `<h1>` (`index.html:621`), well-structured `<nav> / <main> / <section> / <footer>`.
- Reasonable heading hierarchy (h1 → h2 per section → h3 features → h4 stages).
- Static, fast, no framework, fonts preconnected (`index.html:15-17`).
- Reduced-motion respected (`index.html:197-199`).

### Gaps (ranked by SEO/AI-ingestion impact)

1. **No `og:image` / `twitter:image`** — `twitter:card` is declared `summary_large_image` but no image is referenced, so social previews fall back to a blank card. High visibility cost on X, LinkedIn, Slack, Discord, Mastodon, Bluesky.
2. **No `robots.txt`** — crawlers guess; no sitemap pointer; no explicit allow for AI bots (GPTBot, ClaudeBot, PerplexityBot, Google-Extended, CCBot, Applebot-Extended, Bytespider, Amazonbot, Meta-ExternalAgent).
3. **No `sitemap.xml`** — tiny site, but its absence costs nothing to fix and helps Bing/Yandex significantly.
4. **No `llms.txt` / `llms-full.txt`** — emerging standard (llmstxt.org) read by Anthropic, Perplexity, and several agent frameworks for a curated, plaintext site digest. Fono is the kind of project that benefits disproportionately.
5. **No JSON-LD structured data** — `SoftwareApplication` + `Organization` + `BreadcrumbList` + `FAQPage` would let Google render rich results (downloads, OS, license, price = free).
6. **`<h1>` content mutates client-side** ("It types." ↔ "It answers.") via the swap mechanism (`index.html:621`, `index.html:849-857`) — both variants are in the DOM so crawlers see both, but JS-blind indexers see "Speak. It types. It answers." back-to-back. Worth tightening the SR/crawler text.
7. **Title is brand-led, not query-led** — `Fono — Speak. It types.` is great editorially but contains zero of the queries users type. A subtitle slug ("Linux voice dictation") would lift CTR without hurting brand.
8. **No `meta name="keywords"` is fine, but missing `meta name="author"`, `meta name="application-name"`, and `apple-mobile-web-app-*` hints** for richer indexing surfaces.
9. **No favicon / no `apple-touch-icon` / no `manifest.webmanifest`** — favicon influences brand recognition in SERP results that now show site icons.
10. **No `<link rel="alternate" type="application/rss+xml">`** — releases on GitHub already have an Atom feed (`https://github.com/bogdanr/fono/releases.atom`) that could be advertised.
11. **Orphan demo pages in `assets/`** — either canonicalize them to `/`, link to them, or noindex them; currently they may be discovered, indexed, and dilute ranking.
12. **`/install` route serves a shell script** — should be `Content-Type: text/plain` and `X-Robots-Tag: noindex` so search engines do not index a shell script as a page. GitHub Pages headers are limited; the practical lever is a `robots.txt` `Disallow: /install`.
13. **No `prefers-color-scheme` paired `theme-color`** — minor, but Safari/Chrome will pick a better address-bar tint in light mode if both are declared.
14. **Font CSS is render-blocking from `fonts.googleapis.com`** — already `preconnect`-ed, but no `font-display: swap` enforced from our side (Google's CSS does include it). Acceptable; document as a non-issue.
15. **No image `alt` / decorative `aria-hidden` audit** — most SVGs are inline icons with `aria-label`s on parent links, but the brand wordmark and decorative star SVGs are unlabeled (acceptable since the link has accessible text, but worth confirming for Lighthouse a11y score, which feeds SEO).
16. **No content for long-tail queries** — single page covers "what". A small `/faq` or expanded FAQ section on the same page captures "how to install fono on arch", "fono vs whisper", "wayland dictation", etc., and is consumed verbatim by AI answer engines.
17. **GitHub repo description / topics** — off-site SEO. The `bogdanr/fono` repo's topics, About blurb, and pinned README affect Google's understanding of the brand entity. Out of scope for this site but worth flagging.
18. **No `Referrer-Policy`, `Permissions-Policy`** meta — small Lighthouse "best practices" lift, indirect ranking signal.

## Implementation Plan

### Phase A — Crawl & ingestion plumbing (highest leverage, smallest diffs)

- [ ] Task A1. Add `robots.txt` at repo root with: `User-agent: *` `Allow: /`, an explicit `Disallow: /install` (the shell script route), and a `Sitemap: https://fono.page/sitemap.xml` line. Explicitly allow the major AI crawlers by name (GPTBot, ClaudeBot, ClaudeBot-User, PerplexityBot, Perplexity-User, Google-Extended, CCBot, Applebot, Applebot-Extended, Amazonbot, Meta-ExternalAgent, Bytespider, DuckAssistBot, cohere-ai, anthropic-ai) so the policy is unambiguous rather than implied. Rationale: GitHub Pages serves any file at the path verbatim; this is the single most-asked-for file by every crawler.

- [ ] Task A2. Add `sitemap.xml` listing `https://fono.page/` (and any new pages introduced by tasks below) with `<lastmod>` matching the last content edit. Tiny file, large effect on Bing/Yandex. Rationale: declared sitemap is the canonical discovery surface and is required for Google Search Console submission.

- [ ] Task A3. Add `llms.txt` at repo root following llmstxt.org: an H1 (Fono), a one-sentence description, then sectioned links to the canonical page, install script, GitHub repo, releases, license, and the FAQ section anchor. Optionally add `llms-full.txt` containing a plaintext distillation of the index page (manifesto, install, pipeline, compatibility). Rationale: lets Claude, Perplexity, and agent frameworks ingest Fono in a single fetch without HTML noise.

- [ ] Task A4. Add a favicon set: `favicon.ico` (multi-resolution), `favicon.svg` (preferred — supports light/dark via `prefers-color-scheme`), `apple-touch-icon.png` (180×180), and a `site.webmanifest` with `name`, `short_name`, `theme_color`, `background_color`, `display: minimal-ui`. Reference all four from `<head>`. Rationale: favicons now appear in Google SERPs and feed brand recognition; a webmanifest unlocks "Add to Home Screen" surfaces and richer indexing.

### Phase B — Meta, social, and structured data

- [ ] Task B1. Add `og:image` and `twitter:image` pointing to a static 1200×630 PNG (e.g. `/assets/og.png`) generated from the hero (wordmark + tagline + oscilloscope still). Also add `og:image:width`, `og:image:height`, `og:image:alt`, `og:site_name = Fono`, `og:locale = en_US`, `twitter:title`, `twitter:description`, `twitter:image:alt`. Rationale: a card with a real image is the difference between a click and a scroll-past on every social surface and many AI answer cards.

- [ ] Task B2. Refine `<title>` toward query-led form while preserving brand: e.g. `Fono — Local-first voice dictation for Linux (Whisper, X11 & Wayland)`. Keep the editorial tagline as `og:title`. Update `meta description` to ~155 characters with primary keywords (Linux, dictation, Whisper, local, Wayland, open source, Rust) woven into a sentence that still reads human. Rationale: titles and descriptions are the strongest on-page ranking signals after H1; the current title contains none of the queries Fono actually answers.

- [ ] Task B3. Add `meta name="author"`, `meta name="application-name" = Fono`, `meta name="generator"` (omit or set to a project marker), `meta name="robots" = index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1` (lets Google use full snippets and large image previews in SERP). Rationale: `max-image-preview:large` is required for rich Discover-style cards and costs nothing.

- [ ] Task B4. Add JSON-LD `<script type="application/ld+json">` blocks for: (a) `SoftwareApplication` (name, description, operatingSystem `Linux`, applicationCategory `DeveloperApplication` / `UtilitiesApplication`, offers free, license URL, downloadUrl, softwareVersion sourced from latest release, author Person/Organization, aggregateRating if/when available), (b) `Organization` or `Person` for the author with `sameAs` linking to GitHub, (c) `WebSite` with `SearchAction` only if site search exists (skip if not), (d) `BreadcrumbList` for the single page (trivial but valid). Rationale: JSON-LD is how Google's Knowledge Graph and most AI answer engines extract structured facts.

- [ ] Task B5. Add a small FAQ section on `index.html` (4–8 Q&As covering: what is Fono, does it work offline, X11 vs Wayland support, supported distros, how to switch providers, license, telemetry, comparison to competitors) and back it with `FAQPage` JSON-LD. Rationale: FAQ blocks are over-represented in Google rich results and Perplexity/ChatGPT answers; they directly capture long-tail queries.

### Phase C — Content semantics and crawl hygiene

- [ ] Task C1. Add `rel="me"` and/or `rel="author"` links from `<head>` or footer to the author's GitHub profile (and any Mastodon/Bluesky handle) to anchor the author entity. Rationale: helps Google associate the page with a verified identity, which is a small but real E-E-A-T signal for technical content.

- [ ] Task C2. Add `<link rel="alternate" type="application/atom+xml" title="Fono releases" href="https://github.com/bogdanr/fono/releases.atom">` so feed readers and indexers can discover release news. Rationale: free signal of liveness.

- [ ] Task C3. Audit the JS-driven swap (`index.html:621`, `index.html:849-857`). Confirm both variants render in the static DOM (they do) and add a single, crawler-friendly sentence near the H1 (e.g. an `aria-hidden`-flagged `<p class="visually-hidden">` summary) describing the product in one sentence. Rationale: ensures JS-disabled crawlers and excerpt extractors get a clean, non-fragmented headline.

- [ ] Task C4. Decide policy for `assets/*-demo.html`: either (a) add `<meta name="robots" content="noindex,follow">` inside each demo page and add a `Disallow:` for `/assets/*-demo.html` in `robots.txt`, or (b) add `<link rel="canonical" href="https://fono.page/">` inside each so they consolidate to the homepage. Recommendation: (a) noindex, since they are dev artefacts, not user-facing pages. Rationale: prevents thin/duplicate content dilution.

- [ ] Task C5. Add `aria-hidden="true"` to purely decorative SVGs (waveform, star icons on links that already have accessible labels) and verify every actionable element has an accessible name. Rationale: Lighthouse a11y score feeds Page Experience signals and downstream ranking.

- [ ] Task C6. Add a paired `theme-color` for light scheme: `<meta name="theme-color" media="(prefers-color-scheme: dark)" content="#0e0d0c">` plus a `(prefers-color-scheme: light)` variant matching the cream background. Rationale: better mobile chrome integration, no SEO downside.

### Phase D — Performance & Core Web Vitals (Google ranking signal)

- [ ] Task D1. Add `loading="lazy"` and `decoding="async"` to any below-the-fold images introduced (favicons, og image not counted — those are referenced by meta). Add `fetchpriority="high"` to the LCP element if it ever becomes an image. Rationale: pre-empts CWV regressions.

- [ ] Task D2. Confirm the oscilloscope canvas demo (`shared/fono-oscilloscope.js`) does not regress LCP / TBT. Consider adding `defer` to the script tag at `index.html:629` and lazy-initialising the canvas with an `IntersectionObserver` so it only runs when visible. Rationale: hero JS that mutates a canvas can spike INP on slower devices; deferring protects the CWV score that Google factors into ranking.

- [ ] Task D3. Inline the critical CSS for the hero (already inlined in `<style>`) and move `shared/fono.css` to `media="print" onload="this.media='all'"` or a `preload` pattern if measurement shows it blocks render. Rationale: only worth doing if CWV measurement shows render-blocking; tag as conditional.

- [ ] Task D4. Add `Cache-Control` is owned by GitHub Pages and not configurable per-file; instead, fingerprint long-lived assets if/when a build step is added. Document as out-of-scope for now. Rationale: closes the loop on a question reviewers will ask.

### Phase E — Off-site reinforcement (one-time, not in this repo)

- [ ] Task E1. Submit `https://fono.page/sitemap.xml` to Google Search Console and Bing Webmaster Tools after Phase A ships. Verify ownership via DNS TXT (preferred — domain-level) or an HTML meta tag added to `index.html` (`google-site-verification`, `msvalidate.01`). Rationale: gets indexing telemetry and surfaces crawl errors.

- [ ] Task E2. Ensure the GitHub repository `bogdanr/fono` has: a one-sentence description that matches the site meta description, a homepage URL of `https://fono.page/`, and topics including `voice-dictation`, `speech-to-text`, `whisper`, `linux`, `wayland`, `x11`, `rust`, `voice-assistant`, `local-first`, `open-source`. Rationale: GitHub is a top-3 organic referrer for technical projects and the repo card itself appears in Google.

- [ ] Task E3. Cross-link from any related properties (author's other sites, blog posts, Mastodon/Bluesky bios) using the canonical `https://fono.page/` URL. Rationale: backlink quality is still the single largest ranking factor; even a few authoritative links move the needle for a new domain.

## Verification Criteria

- `curl https://fono.page/robots.txt` returns 200 with the documented content and a `Sitemap:` line.
- `curl https://fono.page/sitemap.xml` returns 200 with a valid `<urlset>` containing the homepage.
- `curl https://fono.page/llms.txt` returns 200 with a valid llmstxt.org-shaped document.
- Google's Rich Results Test (`https://search.google.com/test/rich-results`) reports zero errors and detects `SoftwareApplication` and `FAQPage` items.
- Facebook Sharing Debugger and Twitter/X Card Validator render a card with title, description, and the 1200×630 image.
- `https://fono.page/` Lighthouse scores ≥ 95 on Performance, Accessibility, Best Practices, and SEO categories on mobile.
- Core Web Vitals (PageSpeed Insights, mobile): LCP < 2.5 s, INP < 200 ms, CLS < 0.1.
- View-source of `index.html` shows: canonical, og:image with width/height/alt, twitter:image, JSON-LD `SoftwareApplication`, JSON-LD `FAQPage`, `meta robots` with `max-image-preview:large`, favicon and apple-touch-icon links, manifest link, atom alternate link.
- Each file under `assets/*-demo.html` either carries `<meta name="robots" content="noindex">` or a canonical link to `/`.
- `curl -I https://fono.page/` returns `200` and `Content-Type: text/html; charset=utf-8` (GitHub Pages default — confirm, do not break).
- Google Search Console and Bing Webmaster Tools both show the property verified and the sitemap submitted with `Success` status within one indexing cycle.

## Potential Risks and Mitigations

1. **Title rewrite hurts brand recognition or CTR.**
   Mitigation: keep "Fono" as the leading token; A/B by measuring CTR in Search Console over a 4–6 week window before/after; preserve the editorial tagline in `og:title` so social cards stay on-voice.

2. **JSON-LD claims (e.g. `aggregateRating`, `softwareVersion`) drift from reality.**
   Mitigation: omit fields that cannot be reliably auto-populated; populate `softwareVersion` from the existing GitHub Releases API call already wired in `index.html:892-901` so it stays current; leave ratings out until the project has real review data.

3. **Allowing AI crawlers conflicts with future monetisation or licensing intent.**
   Mitigation: make the allow-list explicit and per-bot in `robots.txt` so it can be tightened later without breaking general SEO; this is a policy decision the maintainer should ratify before shipping. Default recommendation: allow, because Fono is GPL-3.0 and benefits from being in answer engines' corpora.

4. **`llms.txt` is a draft standard; format may evolve.**
   Mitigation: follow the current llmstxt.org spec literally, keep the file under ~200 lines, and treat it as cheaply re-generatable. No downside if the standard shifts.

5. **Adding the FAQ section bloats the editorial hero page.**
   Mitigation: place the FAQ as the second-to-last section (before the final CTA), keep answers to 2–3 sentences, reuse the existing typography tokens, and gate the FAQ behind a `<details>` accordion if visual weight becomes an issue (JSON-LD still works inside collapsed `<details>`).

6. **Open Graph image goes stale (version numbers, branding).**
   Mitigation: design the OG image without version numbers or dated marketing; commit the source (Figma/SVG) alongside the export so regeneration is cheap.

7. **`Disallow: /install` blocks discovery of the install one-liner.**
   Mitigation: the install instructions live on the homepage in plaintext, which is what crawlers and AI agents will index. Disallowing the script-only endpoint prevents Google from indexing raw shell as a "page" while keeping the human-readable install copy fully discoverable.

8. **GitHub Pages cannot set custom HTTP headers.**
   Mitigation: rely on `<meta>` equivalents where possible (`robots`, `referrer`); accept the trade-off and document any remaining gap. If headers become critical later, front Pages with Cloudflare and add header rules there.

## Alternative Approaches

1. **Move to a generated static site (Astro/Eleventy/Zola) with built-in SEO plugins.** Trade-off: industrial-strength tooling (auto sitemap, auto OG-image, image optimisation) at the cost of a build step and a CI pipeline — directly contradicts the README's stated "one HTML file, one CSS, one JS — no build step" ethos. Not recommended unless the site grows past ~5 pages.

2. **Front the site with Cloudflare (or Netlify) for header control.** Trade-off: enables real `Cache-Control`, `X-Robots-Tag`, `Link: rel=preload`, and HTTP/3 niceties at the cost of one more dependency in the deploy path. Recommended only if Phase D measurements show GitHub Pages' defaults are hurting CWV.

3. **Expand to a small content cluster (`/faq`, `/changelog`, `/compare/vs-whisper-typer`, `/docs/wayland`).** Trade-off: dramatically more long-tail surface area and the strongest possible AI-agent ingestion, but real editorial work and ongoing maintenance. Recommended as a Phase F once Phases A–D are live and Search Console shows which queries are actually surfacing.

4. **Programmatic OG-image generation (e.g. Vercel OG, Satori) per section anchor.** Trade-off: prettier per-link previews at the cost of a serverless runtime — overkill for a single-page site. Skip.

5. **Ship `llms.txt` only, defer `llms-full.txt`.** Trade-off: faster to land, slightly less useful for agents that prefer the full digest. Acceptable interim state.
