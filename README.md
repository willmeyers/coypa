<div align="center">
  <img src="assets/icon.png" width="120" alt="coypa">
</div>

<h1 align="center">coypa</h1>

<p align="center">
  <b>coypa is a fun way to manage your clipboard.</b><br>
  Copies stack and follow your cursor. Hold paste, they fan out
  into a wheel. Pick one, and it pastes!
</p>

<div align="center">
  <img src="docs/demo.gif" width="420" alt="coypa in use: chips trailing the cursor">
</div>

---

## Install

```sh
brew install sdl3
git clone https://github.com/willmeyers/coypa
cd coypa
./scripts/bundle.sh
open dist/                  # drag coypa.app to /Applications
```

### Grant Accessibility

coypa has to notice the paste key being held **anywhere**, which macOS only
permits through a keyboard event tap:

> **System Settings ▸ Privacy & Security ▸ Accessibility** → add `coypa.app`

Grant it to the **app bundle**, not your terminal — macOS ties this permission
to an app's code-signing identity, so running the bare binary attaches the
grant to whatever launched it.

Without permission coypa still collects copies; only the hold-to-wheel trigger
goes quiet, and ⌘V behaves exactly as normal.

coypa runs as a background agent: no Dock icon, no ⌘-Tab entry.

## Develop

```sh
cargo run --release            # run from source
cargo test                     # unit tests
COYPA_DEBUG=1 cargo run -r     # log every capture: kind, mime, size, timing
COYPA_SELFTEST=1 cargo run -r  # headless check of the select→paste path
```

`COYPA_SELFTEST` is the fastest way to validate clipboard behaviour without a
display. It builds a stack, pops a non-top item, and confirms the OS clipboard
really holds it — covering both the eager (plain-text) and lazy (rich/image)
write paths.

Custom icon:

```sh
scripts/make-icon.sh            # from assets/icon.png
scripts/make-icon.sh art.png    # from your own 1024×1024 PNG
```
