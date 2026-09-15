# Installing on bare metal (systemd)

The Docker image and Helm chart in the [README](../README.md#quickstart) bundle everything.
This page is the other path: the release binary on a Debian/Ubuntu host under systemd, with
jellyfin-ffmpeg and the jellyfin-web client installed from Jellyfin's own apt repository.
The result is the layout `contrib/systemd/ferrofin.service` expects.

| Path | What lives there |
|---|---|
| `/opt/ferrofin/<version>/ferrofin-server`, `/opt/ferrofin/current` → it | the binary; upgrades swap the symlink |
| `/etc/ferrofin/config.toml` | configuration ([`docs/CONFIG.md`](CONFIG.md)) |
| `/var/lib/ferrofin/data` | `jellyfin.db`, `cache/` (transcodes), `log/`, `plugins/`, `config/` |
| `/usr/lib/jellyfin-ffmpeg/` | jellyfin-ffmpeg (`ffmpeg`, `ffprobe`) |
| `/usr/share/jellyfin/web/` | jellyfin-web's built client, served at `/web` |

## 1. ffmpeg and the web client

Ferrofin does not ship its own ffmpeg. It uses **jellyfin-ffmpeg**, the same build the
release image bundles: SIMD tonemapping, current libx264/libx265, `libfdk_aac`, and
`--enable-chromaprint` for the intro skipper. Your distro's `ffmpeg` works for basic
transcodes but lacks several of those, and Ferrofin plans every transcode against the
binary it probes at startup, so pick one and point Ferrofin at it explicitly.

```sh
sudo apt-get install -y ca-certificates curl gnupg
curl -fsSL https://repo.jellyfin.org/jellyfin_team.gpg.key \
  | sudo gpg --dearmor -o /usr/share/keyrings/jellyfin.gpg
echo "deb [signed-by=/usr/share/keyrings/jellyfin.gpg] https://repo.jellyfin.org/debian $(. /etc/os-release; echo "$VERSION_CODENAME") main" \
  | sudo tee /etc/apt/sources.list.d/jellyfin.list
sudo apt-get update
sudo apt-get install -y jellyfin-ffmpeg8 jellyfin-web
```

`jellyfin-web` installs the built client at `/usr/share/jellyfin/web`. Replace `debian`
with `ubuntu` in the repository line on Ubuntu. Do **not** install the `jellyfin-server`
package on the same host unless you mean to run both; they would race for port 8096.

## 2. The binary

Download the tarball for your architecture from the
[releases page](https://github.com/mangoleaf/ferrofin/releases) and verify it:

```sh
V=v1.0.1; T=x86_64-unknown-linux-gnu          # or aarch64-unknown-linux-gnu
curl -fsSLO "https://github.com/mangoleaf/ferrofin/releases/download/$V/ferrofin-$V-$T.tar.gz"
curl -fsSLO "https://github.com/mangoleaf/ferrofin/releases/download/$V/ferrofin-$V-$T.tar.gz.sha256"
sha256sum -c "ferrofin-$V-$T.tar.gz.sha256"
sudo mkdir -p /opt/ferrofin
sudo tar xzf "ferrofin-$V-$T.tar.gz" -C /opt/ferrofin
sudo ln -sfn "/opt/ferrofin/ferrofin-$V-$T" /opt/ferrofin/current
```

## 3. User, directories, configuration

```sh
sudo useradd --system --home /var/lib/ferrofin --shell /usr/sbin/nologin ferrofin
sudo mkdir -p /var/lib/ferrofin/data /etc/ferrofin
sudo chown -R ferrofin:ferrofin /var/lib/ferrofin
sudo tee /etc/ferrofin/config.toml >/dev/null <<'TOML'
data_dir = "/var/lib/ferrofin/data"
bind_addr = "0.0.0.0"
port = 8096
# ffmpeg_path / ffprobe_path / web_dir are set by the unit file's Environment=
# lines; put them here instead if you prefer a single source of truth.
TOML
```

Every key is documented in [`docs/CONFIG.md`](CONFIG.md). The `ferrofin` user needs read
access to your media, typically by adding it to the group that owns the library.

## 4. The unit

```sh
sudo cp contrib/systemd/ferrofin.service /etc/systemd/system/ferrofin.service
sudo systemctl daemon-reload
sudo systemctl enable --now ferrofin
journalctl -u ferrofin -f
```

On a fresh database the log prints the generated `admin` password once; record it, or set
`FERROFIN_ADMIN_PASSWORD` in the unit before first start. Then open `http://host:8096/web`.

The unit runs with `ProtectSystem=strict`: the filesystem is read-only except
`/var/lib/ferrofin/data`. Scans and playback only read media, so that is enough for most
installs. If you enable deleting items or saving metadata/images into the library from the
UI, add that library to `ReadWritePaths=` in a drop-in:

```sh
sudo systemctl edit ferrofin      # opens an override; add:
# [Service]
# ReadWritePaths=/srv/media
```

For VAAPI/QSV hardware transcoding uncomment the `DeviceAllow=` and
`SupplementaryGroups=` lines.

## 5. Migrating a Jellyfin database

Stop Jellyfin, then copy three things from its data directory (`/var/lib/jellyfin` on a
Debian install, the `/config` volume in Docker) into `/var/lib/ferrofin/data`:

```sh
sudo cp -a /var/lib/jellyfin/data/jellyfin.db /var/lib/ferrofin/data/jellyfin.db
sudo cp -a /var/lib/jellyfin/root            /var/lib/ferrofin/data/   # library definitions
sudo cp -a /var/lib/jellyfin/metadata        /var/lib/ferrofin/data/   # images, NFO cache
sudo chown -R ferrofin:ferrofin /var/lib/ferrofin/data
```

The database holds the items, users and watch state. The library definitions are folders
under `root/default/`, one per library with its `.mblink` path shortcuts and `options.xml`
(imported into Ferrofin's `options.json` on first read); without them the admin Libraries
page is empty. The images live under `metadata/`; without them every poster is a blurhash
placeholder. Ferrofin adopts a **Jellyfin 10.11.8 through 10.11.11** database on first
boot, writing `jellyfin.db.pre-ferrofin` beside it first. The adoption is one-way; going back to Jellyfin
means restoring that copy. [`docs/UPGRADING.md`](UPGRADING.md) has the full notes.

A database from any other Jellyfin version is refused with a message naming the unexpected
migration ids. Bring it to 10.11.x under Jellyfin first.

## Upgrading

Unpack the new tarball next to the old one, repoint `/opt/ferrofin/current`, and restart:

```sh
sudo ln -sfn "/opt/ferrofin/ferrofin-$NEW-$T" /opt/ferrofin/current && sudo systemctl restart ferrofin
```

Ferrofin's own migrations run on start; the previous directory stays in place for a
rollback of the binary, though a database that a newer version has migrated may not open
under an older one. Back up `jellyfin.db` before a major upgrade.
