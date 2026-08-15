# Deploy

Run waypointd as an OS service so it starts on boot and restarts after a
crash. The server is configured **only** via `WAYPOINTD_*` environment
variables, so each file below is just a place to set those and point at the
binary. Every file uses placeholders — replace them before deploying and
never commit real tokens.

```
deploy/
├── macos/waypointd.plist         → ~/Library/LaunchAgents/waypointd.plist
├── linux/waypointd.service       → /etc/systemd/system/waypointd.service
├── windows/install-waypointd.bat → run once (elevated) to install via NSSM
└── README.md                     → this file
```

## Build the binary first

```
cargo install --path . --locked        # installs ~/.cargo/bin/waypointd
```

The release binary embeds the built frontend, so `frontend/dist/` must exist
(`bun run build` in `frontend/`). Then copy/symlink it where the service
expects:

| OS      | expected path                                            | move it yourself |
| ------- | -------------------------------------------------------- | ---------------- |
| macOS   | `/usr/local/bin/waypointd` (or `~/.cargo/bin/waypointd`) | `cp` or `ln -s`  |
| Linux   | `/usr/local/bin/waypointd`                               | `sudo cp`        |
| Windows | `C:\waypoint\waypointd.exe`                              | `copy`           |

## macOS (launchd)

A LaunchAgent runs at login under your user. A LaunchDaemon
(`/Library/LaunchDaemons/`) runs system-wide before login.

```
cp deploy/macos/waypointd.plist ~/Library/LaunchAgents/
# edit it: replace /Users/YOUR_USERNAME and YOUR_LONG_RANDOM_TOKEN
mkdir -p ~/.waypoint/{backups,cache}
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/waypointd.plist
# or legacy: launchctl load ~/Library/LaunchAgents/waypointd.plist

launchctl stop|start com.waypointd.server
tail -f ~/.waypoint/waypointd.log
# uninstall: launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/waypointd.plist
```

## Linux (systemd)

```
sudo cp deploy/linux/waypointd.service /etc/systemd/system/
sudo useradd --system --no-create-home --shell /usr/sbin/nologin waypointd
sudo install -d -o waypointd -g waypointd \
  /var/lib/waypoint /var/lib/waypoint/backups /var/lib/waypoint/cache

# put the token in a root-only file so it never appears in the unit or
# `systemctl show`:
echo 'WAYPOINTD_SERVE_TOKEN=YOUR_LONG_RANDOM_TOKEN' \
  | sudo tee /etc/waypointd.env >/dev/null
sudo chmod 600 /etc/waypointd.env

sudo systemctl daemon-reload
sudo systemctl enable --now waypointd
systemctl status waypointd
journalctl -u waypointd -f
```

## Windows (NSSM)

Windows has no built-in "install a binary as a service" tool; the standard
approach is NSSM. The `.bat` wraps every `nssm` command, so you only run it
once:

```
copy target\release\waypointd.exe C:\waypoint\
cd deploy\windows
install-waypointd.bat        # from an elevated Command Prompt
```

The token is hard-coded at the top of the `.bat` — edit it before running.
Manage the service with `nssm start|stop|restart|remove waypointd`.

## Environment variables set by these files

All three set the same base set (see `docs/operations.md` for the full
list and defaults):

| variable                         | value                                                                       |
| -------------------------------- | --------------------------------------------------------------------------- |
| `WAYPOINTD_DB_FILE`              | `~/ .waypoint / /var/lib/waypoint / C:\waypoint\data` + `.sqlite`           |
| `WAYPOINTD_SERVE_TOKEN`          | `YOUR_LONG_RANDOM_TOKEN` (edit per file)                                    |
| `WAYPOINTD_SERVE_HOST`           | `localhost` — change to `0.0.0.0` only if you really want it on the network |
| `WAYPOINTD_SERVE_PORT`           | `8080`                                                                      |
| `WAYPOINTD_LOG_LEVEL`            | `info`                                                                      |
| `WAYPOINTD_LOG_FORMAT`           | `human-readable` (swap to `json` for a collector)                           |
| `WAYPOINTD_BACKUP_DIR`           | `<data>/backups` — enables automated `VACUUM INTO` backups                  |
| `WAYPOINTD_BACKUP_INTERVAL_SECS` | `86400` (daily)                                                             |
| `WAYPOINTD_BACKUP_KEEP`          | `7`                                                                         |
| `WAYPOINTD_CACHE_DIR`            | `<data>/cache` — fetched-media cache                                        |

## Security notes

- Keep `WAYPOINTD_SERVE_TOKEN` in a root/user-owned file (`/etc/waypointd.env`
  on Linux, the plist's `EnvironmentVariables` on macOS, the `.bat` header on
  Windows). It's a shared-secret handshake — anyone with it is an admin.
- `localhost` binding is the safe default. Only set `WAYPOINTD_SERVE_HOST` to
  `0.0.0.0` behind a reverse proxy that terminates TLS; if you do, also set
  `WAYPOINTD_COOKIE_SECURE=true`.
- Don't commit the deployed copies or the env file; keep only these templates
  in the repo.
