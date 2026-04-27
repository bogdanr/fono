# Fono — site

The source for [fono.page](https://fono.page) — the landing page for [Fono](https://github.com/bogdanr/fono),
a lightweight, native voice-dictation tool for Linux.

This is a static site (one HTML file, one CSS, one JS — no build step).
Live this branch on its own; the project code lives on `main`.

## Layout

```
index.html            editorial hero, install matrix, pipeline, compatibility
install               POSIX sh installer served at https://fono.page/install
shared/fono.css       reset
shared/typed-terminal.js  hero typewriter
CNAME                 fono.page
.nojekyll             skip Jekyll on GitHub Pages
```

The `install` script is what `curl -L https://fono.page/install | sh` runs.
It resolves the latest release from `bogdanr/fono`, downloads the matching
binary, and installs to `/usr/local/bin` (override with `BIN_DIR=…`).

## Local preview

Any static server works. Two quick options:

```sh
python3 -m http.server 8000
# or
npx --yes serve .
```

Then open http://localhost:8000.

## Deploy

GitHub Pages is configured to serve this branch at the repo root.

1. Push to the `site` branch.
2. Repo **Settings → Pages → Build and deployment → Source: Deploy from a branch**.
3. **Branch: `site` / `(root)`**.
4. Confirm the `CNAME` is pointed at `fono.page` and DNS is in place.

No Actions workflow needed — Pages picks up the branch directly.
