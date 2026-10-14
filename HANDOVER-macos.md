# Handover: test worktree instances on macOS before release

Branch: `feat/worktree-instances` (pushed). Delete this file before merging to `main`.

## Context

The branch adds **worktree instances**: every git worktree of a compose project runs as its own
instance (own URLs, ports, compose project), while infrastructure (databases, Redis, …) runs once in
a shared stack. The user's compose files are never modified; Orca writes a generated override to
`<worktree>/.orca/`. UI: the **Projects** page. Also: `orca up/down/status/logs/env/run/projects` in
the CLI, `instance_*` MCP tools, a tray menu listing running instances.

Design doc (decisions, what was built, what changed from the plan):
https://claude.ai/code/artifact/0f1bcee1-69c7-4b77-b1b2-6acb0468e361

Everything was built and tested on **Linux only** (native Docker, real Postgres/MySQL/MariaDB/
Mongo/RabbitMQ/Redis containers, Caddy gateway, Claude Code over MCP). The web UI was checked in
headless Chromium with the Tauri IPC mock. **The desktop app itself has never been launched**, and
nothing has run on macOS. CI on macOS only runs `cargo check`.

Code: `crates/orca-core/src/dev/` (planning, no I/O), `crates/orca-daemon/src/dev/` (API under
`/api/v1/dev/*`, jobs, data, activity), `crates/orca-cli/src/dev.rs`, `gui/src/pages/ProjectsPage.tsx`,
`gui/src/components/dev/`, `src-tauri/src/{commands,tray}.rs`.

## Ground rules

- Don't touch the user's own containers, stacks or real projects. Use a throwaway repo (below).
- The packaged daemon logs to `~/Library/Application Support/orca/daemon.log`; config and API token
  are in `~/Library/Application Support/orca/config.json`.
- Commit fixes to `feat/worktree-instances`. Don't release; report back what passed and what you fixed.

## Setup

```bash
cd ~/projects/orca && git fetch && git checkout feat/worktree-instances && git pull
cd gui && npm install && cd ..
pkill orca-daemon            # the app starts its own daemon
cargo tauri dev              # desktop app with dev tools
```

Throwaway project (somewhere under your home folder):

```bash
mkdir -p ~/orca-wt-test/shop && cd ~/orca-wt-test/shop
cat > compose.yaml <<'EOF'
services:
  web:
    image: nginx:alpine
    ports: ["8081:80"]
    depends_on: [api]
  api:
    image: nginx:alpine
    ports: ["8082:80"]
    environment:
      DATABASE_URL: postgres://app:app@db:5432/app
  db:
    image: postgres:16-alpine
    environment: { POSTGRES_USER: app, POSTGRES_PASSWORD: app, POSTGRES_DB: app }
EOF
git init -q -b main && git add . && git commit -qm init
git worktree add -b feat-a ../shop-a
git worktree add -b claude/a-rather-long-branch-name-for-testing ../shop-long
```

## Checklist

### 1. Desktop shell (never run before)
- [ ] App starts; **Projects** is in the sidebar; the Dashboard has a "Worktree instances" card.
- [ ] **Add project** → **Choose a repository…** opens the native folder picker (new plugin:
      `tauri-plugin-dialog`, called from Rust in `pick_folder`). Pick `~/orca-wt-test/shop`.
- [ ] The folder icon next to a path (Reveal) opens Finder at that folder (`reveal_path`).
- [ ] Tray / menu bar menu: after starting an instance it lists it with its URLs within ~15 s
      (`tray.rs`, refresh loop); clicking a URL opens the browser.

### 2. Docker CLI from the packaged daemon (highest risk)
The feature shells out to `docker compose` and `docker exec` (`ops.rs`, `data.rs`), unlike most of
Orca, which talks to the Docker socket directly. The app bundle's PATH is minimal.
- [ ] Starting an instance works when the app is launched from Finder/Dock, not only from a
      terminal. If it fails with "could not run `docker compose`", the daemon needs the extended PATH
      (`/opt/homebrew/bin:/usr/local/bin`, see `extended_path()` in `api.rs`) and, if no docker
      context points at the Orca VM, `DOCKER_HOST=unix://$HOME/.lima/orca/sock/docker.sock` for those
      child processes. Fix it once in `dev/ops.rs` (`compose`) and `dev/data.rs` (`exec_sh`).
- [ ] `docker compose version` must be ≥ 2.24.4 for override tags; older shows "resolved mode" in the
      Generated files tab (that's expected and fine).

### 3. Instances end to end
- [ ] Setup wizard: Orca suggests `db` as shared; the Reach step shows the hostname picker (Flat
      default). Save writes `orca.yaml` in the main checkout.
- [ ] Start `feat-a` from its card: steps finish, card shows `https://web-feat-a-shop.localhost`
      (plus `:<port>` if the gateway isn't on 443). The URL opens in **Chrome**.
- [ ] Start the long-branch worktree too: its hostname is shortened with a hash, both run at once,
      both reach the shared `db`.
- [ ] Start dialog: switch `db` to **Own database** for one instance; `api`'s `DATABASE_URL` in the
      Env tab points at `app_<instance>`. Data tab → Reset works.
- [ ] Stop / Remove: containers, networks and the instance's routes go away; shared `db` stays.
- [ ] CLI from inside `~/orca-wt-test/shop-a`: `orca status`, `orca up`, `orca env`, `orca logs api`.

### 4. DNS and certificates (the reason for this session)
- [ ] With the default `localhost` domain, the Projects page should show a yellow banner that
      `web-dns-check-shop.localhost` doesn't resolve on this computer (macOS doesn't resolve
      `*.localhost` for non-browser programs). Confirm: `curl https://web-feat-a-shop.localhost` fails
      or `dscacheutil -q host -a name web-feat-a-shop.localhost` returns nothing, while Chrome works.
      If macOS *does* resolve it, the banner must not show; either way, report what you saw.
- [ ] Safari: does `https://web-feat-a-shop.localhost` open? (Expected: no.)
- [ ] Gateway → Configuration: set the domain to one with a wildcard pointing to 127.0.0.1 (the
      user's team uses `local.groupone.dev`). Restart the instance: URL becomes
      `web-feat-a-shop.local.groupone.dev`, the banner disappears, curl and Safari work.
- [ ] HTTPS trust: install the Orca CA (Settings → Certificates) and check Safari and curl accept it.
- [ ] Optional: with a custom wildcard certificate in the gateway, choosing **Nested** hostnames in
      the orca.yaml editor shows "certificate doesn't cover …"; Flat doesn't.

### 5. Gateway access log, idle stop and wake (changed late)
Access lines now go to `~/Library/Application Support/orca/ca/certs/access.log` (written by Caddy in
the VM through the home mount; rolled at 10 MB, mode 0644).
- [ ] The file exists after opening an instance URL and is readable by your user.
- [ ] Set `idle_stop: 1m` under `dev.instance` in `orca.yaml`, start an instance, wait ~2 min without
      opening it: it shows **Idle-stopped**. Open its URL: a "starting" page (503), then the app
      after a few seconds.
- [ ] `https://localhost/localhost-key.pem` (gateway port) returns the landing page, not a key.

### 6. Host dev servers (likely to differ on macOS)
- [ ] From `shop-a`: `orca run --serve web -- python3 -m http.server 3000` and open the web URL.
      The gateway proxies to `host.docker.internal:<port>`. On Linux the gateway gets
      `host.docker.internal:host-gateway`; on macOS it gets nothing (`gateway.rs`, `extra_hosts`),
      and inside the Lima VM `host-gateway` is the VM, not the Mac (`host.lima.internal` is the Mac).
      Expect this to fail; if it does, map `host.docker.internal` to the Mac for the gateway on macOS.

## Afterwards

- Clean up: Remove both instances in the UI, **Stop shared**, unregister the project,
  `rm -rf ~/orca-wt-test`.
- Write down per checklist item: pass / fixed (commit) / still broken.
- Release: version bump in **both** `Cargo.toml` (`workspace.package.version`) and
  `src-tauri/tauri.conf.json` (the auto-updater 404s otherwise). Suggested v0.47.0 (new feature).
  Commit `af88194` (gateway no longer serves the certs folder, which exposed private keys) is a
  security fix worth calling out in the release notes.
