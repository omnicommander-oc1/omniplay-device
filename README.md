# Signage Client (`signaged`)

The client daemon for OMNIPLAY digital signage. It runs continuously on a Raspberry Pi (or a dev machine), checks in with the OC1 API, downloads playlist assets, and plays them through **mpv**.

This repo builds the **`signaged`** binary. Production Pis also run **`signaged-util`** (health, vitals, remote actions) and **`signaged-updater`** (OTA binary updates), which are installed together via `omniplay.sh`.

---

## Related Repositories

| Repo | GitHub | Role |
| :--- | :--- | :--- |
| **omniplay-device** (this repo) | [omnicommander-oc1/omniplay-device](https://github.com/omnicommander-oc1/omniplay-device) | `signaged` playback daemon |
| **signage-client-service** | [omnicommander-oc1/signage-client-service](https://github.com/omnicommander-oc1/signage-client-service) | `signaged-util` companion service (vitals, screenshots, remote restart) |
| **omniplay** (frontend) | [omnicommander-oc1/omniplay](https://github.com/omnicommander-oc1/omniplay) | OMNIPLAY web UI — playlists, players, assets, device admin |
| **oc1-api** | [omnicommander-oc1/oc1-api](https://github.com/omnicommander-oc1/oc1-api) | .NET backend — device check-in, auth, asset CDN, SignalR hub |
| **oc1** | [omnicommander-oc1/oc1](https://github.com/omnicommander-oc1/oc1) | Platform shell — Cognito login, org/product access; launches OMNIPLAY |
| **omniplay-api** (legacy) | [omnicommander-oc1/omniplay-api](https://github.com/omnicommander-oc1/omniplay-api) | Original Rust API — superseded by `oc1-api` for device check-in |

**Runtime flow:** `signaged` → `oc1-api` (`/api/omniplay/device-checkin/*`, `/get-new-key/:id`, `/health`) ← OMNIPLAY UI (via OC1 auth).

Install artifacts (binaries, services, `omniplay.sh`) are published to the `signage-client-install` S3 bucket.

---

## Raspberry Pi Deployment

Use these steps for each deployed Pi player.

### 1. Prepare the device

Set up the Pi with:

- **Username:** `pi`
- **Password:** `OmniP@55word!`

### 2. Download the install script

On the Pi:

```bash
curl -O https://signage-client-install.s3.us-east-1.amazonaws.com/omniplay.sh
sudo chmod +x omniplay.sh
```

### 3. Get the device ID from OMNIPLAY

1. Log in to **OMNIPLAY** (via OC1).
2. Go to the organization's account.
3. Find the device if it already exists, or add a new one.
4. Go to **Players** → find the player.
5. Click the **three-dot menu** in the top-right of the player node.
6. Select **Device ID** and copy the value.

### 4. Run the installer

```bash
sudo ./omniplay.sh
```

When prompted, enter the **Device ID** from OMNIPLAY.

Restart the Pi when prompted.

### 5. Configure Remote.it (initial install only)

On first install, open **Remote.it** and find the new device. Name it to match the OMNIPLAY player (same naming convention as in the cloud app).

---

## Development

### Prerequisites

- [Rust](https://www.rust-lang.org/learn/get-started) (2021 edition)
- **mpv** — must be on `PATH`; the daemon spawns it for playback
- A running **oc1-api** instance (local Docker or remote dev/stage)
- (Optional) **Nix** — `shell.nix` in this repo provides Rust, mpv, clippy, and rustfmt

```bash
# Optional: enter the Nix dev shell
nix-shell
```

### Local stack (recommended for integration testing)

Run the services the device talks to:

| Service | Repo | Default port |
| :--- | :--- | :--- |
| oc1-api | [oc1-api](https://github.com/omnicommander-oc1/oc1-api) | `5001` |
| OMNIPLAY UI | [omniplay](https://github.com/omnicommander-oc1/omniplay) | `4000` |
| oc1 (login shell) | [oc1](https://github.com/omnicommander-oc1/oc1) | `2000` |

```bash
# oc1-api (from oc1-api repo)
docker network create omniframe-network   # one-time
docker-compose up --build

# OMNIPLAY UI (from omniplay/front)
cp .env.example .env   # set VITE_DATABASE_URL=http://localhost:5001/api, VITE_OC1_BASE_URL=http://localhost:2000
npm install && npm run dev

# oc1 (from oc1 repo) — needed for Cognito login flow into OMNIPLAY
npm install && npm run dev
```

In OMNIPLAY, create or select a player and copy its **Device ID** (see deployment steps above).

### Configuration

The daemon reads `~/.config/signage/signage.json`. Cached videos and the mpv playlist live under `~/.local/share/signage/`.

Create the config directory and file:

```bash
mkdir -p ~/.config/signage
```

**Local dev example** (matches `BASIC_AUTH_*` in `oc1-api` `env.example`):

```json
{
  "url": "http://localhost:5001",
  "id": "<device-uuid-from-omniplay>",
  "username": "omni",
  "password": "make-fine-yes-torch"
}
```

| Field | Purpose |
| :--- | :--- |
| `url` | API base URL (no `/api` suffix). Production: `https://oc1-api.omnicommando.com` |
| `id` | Device UUID from OMNIPLAY |
| `username` / `password` | Basic Auth for initial `/get-new-key/:id`; the daemon stores a returned `key` back into this file |
| `key` | Populated automatically after first successful key exchange — do not set manually |

On first boot with no cached playlist, `signaged` blocks until `GET {url}/health` returns 200. If cached content exists, it waits up to 30 seconds then falls back to offline playback.

### Run the daemon

From this repo:

```bash
cargo run
```

Useful flags:

```bash
cargo run -- --version   # prints package version, git hash, and build time
```

On a Pi or Linux desktop with X11, the daemon sets `DISPLAY=:0` automatically. On macOS you may need a display server or XQuartz if testing playback locally.

To run the companion util service (separate repo):

```bash
# In signage-client-service — requires `key` already present in signage.json
cargo run
```

### Build release binaries

```bash
cargo build --release
# Binary: target/release/signaged
```

Release builds for Pi (`aarch64`, `armv7`) are uploaded through **Device Admin** in OMNIPLAY and distributed to devices via `signaged-updater`.

### Testing

There are no in-repo unit tests today. Use these approaches:

**1. Manual integration (full loop)**

1. Start `oc1-api` and OMNIPLAY UI.
2. Create a player with a playlist that has at least one video asset.
3. Configure `~/.config/signage/signage.json` with the player’s Device ID.
4. Run `cargo run` and confirm:
   - Health check succeeds (`✅ API is available`)
   - Videos download to `~/.local/share/signage/`
   - `playlist.txt` is written and mpv starts

**2. API contract (Bruno)**

The `oc1-api` repo includes a Bruno collection at `Collection/OMNIPLAY/Device Checkin/` covering sync, videos, update flags, acknowledge-updates, and `/get-new-key/:id`. Use it to verify API behavior independently of the device binary.

**3. Lint / format**

```bash
cargo clippy
cargo fmt --check
```

**4. Installed Pi smoke test**

After deployment, on the device:

```bash
sudo systemctl status signaged signaged-util signaged-updater
signaged --version
journalctl -u signaged -f
```

---

## TODO

- only download videos from whitelist
- move from tokio to blocking (we actually don't need tokio at all)
- release binaries
