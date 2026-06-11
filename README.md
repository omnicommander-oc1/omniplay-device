# Signage Client

The client daemon for Digital Signage. It is designed to run continuously in the background of a client (such as a Raspberry Pi), where it pulls videos from the Digital Signage API and displays them through an MPV playlist.

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

## Getting Started (Development)

Before you can run the client locally, you need Rust installed: [Rust Getting Started Guide](https://www.rust-lang.org/learn/get-started).

Additionally, a config file at `~/.config/signage/signage.json` needs to be set up properly (see the "Configuration" section for more information).

To run the client, use `cargo run`. This will download and compile all of the dependencies, as well as compile and run the client.

## Configuration

There are two directories to be aware of:

1. `~/.config/signage` for the config file (`signage.json`)
2. `~/.local/share/signage` for data, video, and playlist files

### Example signage.json

```json
{
  "url": "https://ds-api.omnicommando.com",
  "id": "<client_id>",
  "username": "<username>",
  "password": "<password>"
}
```

## TODO

- only download videos from whitelist
- move from tokio to blocking (we actually don't need tokio at all)
- release binaries
