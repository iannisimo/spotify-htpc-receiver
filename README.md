# htpc-nowplaying

A Spotify Connect speaker for a Linux HTPC, with a now-playing screen and a Milkdrop-style
visualizer, all controlled from a TV remote (arrows, OK, Back).

You pick the HTPC in Spotify on your phone and the music plays on the HTPC. The TV shows the
cover, title, artist and progress. One button away is a fullscreen
[projectM](https://github.com/projectM-visualizer/projectm) visualizer that reacts only to
Spotify's audio.

The Spotify side is **Spotify Soloist**, Spotify's official headless Connect client. It owns the
Spotify session and plays the audio; this app only talks to it over its local WebSocket and
listens to its audio stream. There is no login, no Web API and no browsing here: you choose
what plays on your phone.

- One binary, no Qt, KDE or browser engine. Wayland or X11, with PipeWire.
- Near-zero cost when nothing changes: no redraws while paused, one per second while playing.
- The visualizer adapts its resolution, mesh and frame rate to the machine. Without OpenGL or
  projectM it explains why and the rest keeps working.

## What you need

- Linux with **PipeWire** (Soloist must play through PipeWire for the visualizer to hear it).
- **Spotify Premium**: Soloist needs an API key, and only Premium accounts can create one.
- For the visualizer (optional): **libprojectM 4** (not the old 3.x, often packaged as
  `projectm`) and a folder of `.milk` presets. Without them the app still runs; the
  visualizer says what is missing.

## Install

### 1. Build the app

Build requirements: Rust 1.92 or newer, `pkg-config`, `clang` (for the PipeWire bindings), and
the development files of PipeWire (`libpipewire-0.3`), fontconfig and freetype.

```sh
cargo build --release
install -Dm755 target/release/htpc-nowplaying ~/.local/bin/htpc-nowplaying
install -Dm644 packaging/htpc-nowplaying.desktop ~/.local/share/applications/htpc-nowplaying.desktop
```

At runtime it needs PipeWire, fontconfig and freetype, and loads Wayland or X11, EGL/OpenGL
and libprojectM-4 when they are present.

### 2. Install Soloist

Soloist cannot be redistributed, so the script downloads it from Spotify:

```sh
packaging/install-soloist.sh          # into ~/.local/opt/soloist
```

It also asks for your **API key**. Create one at
<https://developer.spotify.com/dashboard/soloist>; it is stored in
`~/.config/soloist/api-key`, readable only by you.

Then run Soloist as a user service:

```sh
mkdir -p ~/.config/systemd/user
cp packaging/systemd/soloist.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now soloist.service
```

The device appears in Spotify as **HTPC**. To change the name, edit `--device-name` in the
unit.

### 3. Pair it once

The first time, your phone has to reach Soloist directly on the local network. Soloist
listens for this on a **random TCP port that changes on every start**, so a firewall with
incoming connections denied (ufw's default, for example) blocks the pairing. Discovery itself
(mDNS) needs no rule.

1. Find the port: `ss -ltnp | grep soloist` lists two. The one on `0.0.0.0:NNNNN` is for
   pairing; `127.0.0.1:9090` is the WebSocket and needs no rule.
2. Allow it from your phone for now, for example
   `sudo ufw allow from <phone-ip> to any port NNNNN proto tcp`.
3. Open Spotify on the phone (same network) and pick HTPC.
4. Remove the rule again. After pairing, Soloist keeps the login in `~/.local/share/soloist`
   and your phone reaches it through Spotify's servers: no open port is needed, the new port
   after a restart does not matter, and it works from anywhere, even from mobile data.

If the phone and the HTPC are on the same Tailscale network, pairing also works with the phone's
Tailscale on and no firewall rule at all, because Tailscale lets tailnet traffic in by itself.

### 4. Open the UI when you cast (optional)

```sh
cp packaging/systemd/htpc-nowplaying-watch.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now htpc-nowplaying-watch.service
```

The watcher opens the now-playing screen when you pick HTPC in Spotify. It opens it once per
cast: close it and it stays closed until you cast again. It needs to run inside your
graphical session; `systemctl --user show-environment | grep DISPLAY` must list
`WAYLAND_DISPLAY` or `DISPLAY` (session managers such as uwsm set this up). You can also start
the UI yourself from the app menu or with `htpc-nowplaying run`. Only one UI runs at a time.

### 5. Presets for the visualizer

Any folder of Milkdrop `.milk` presets works. A good start is projectM's "cream of the crop"
collection (about 10,000 presets, 156 MB):

```sh
git clone --depth 1 https://github.com/projectM-visualizer/presets-cream-of-the-crop.git \
    ~/.local/share/projectM/presets
```

That folder is searched by default, then `/usr/local/share/projectM/presets` and
`/usr/share/projectM/presets`. Set `visualizer.preset_dir` for another one.

## Using it

| Screen | ◀ ▶ | ▲ ▼ | OK | Back |
|---|---|---|---|---|
| Now playing | move between previous, play/pause, next and visualizer | Spotify volume, if `audio_controls = true` | press the focused button | nothing |
| Visualizer | previous / next preset | beat sensitivity | lock / unlock the preset | back to now playing |

The remote's keys must arrive as ordinary key presses. The defaults are the arrow keys,
Return for OK and Escape or BackSpace for Back. Media keys and a dedicated Back key
(`XF86Back`) never reach the app, so a remote's Back button must send Escape or BackSpace.
Remap other keys in the config's `[keys]` section; `wev` shows the names your remote sends.

## Configuration

`~/.config/htpc-nowplaying/config.toml` (or under `$XDG_CONFIG_HOME`).
[`packaging/config.example.toml`](packaging/config.example.toml) lists every setting with
its default and range. Mistakes never stop the app: they are logged and the default is used.

**Visual sync (`visualizer.delay_ms`).** The app taps Spotify's audio before it reaches the
speakers, so the visuals run ahead by however long the output takes. Laptop speakers add about
45 ms; a TV over HDMI or Bluetooth speakers often 100–300 ms. Raise `delay_ms` until beats and
visuals line up, in steps of about 20.

## Soloist things to know

- **Builds expire after 90 days.** An expired Soloist exits with code 10; the service then
  stays stopped and the UI shows "Can't reach Soloist". Update with
  `packaging/install-soloist.sh`, then `systemctl --user restart soloist`.
- **The API key is visible to other local users** in the process list (`ps`), because Soloist
  only accepts it as a command-line argument. On a machine with other users, keep that in mind.
- **VPNs (Tailscale and others):** Soloist may announce itself on the LAN with the VPN address.
  In our test (Tailscale on the HTPC, phone on Wi-Fi only) the phone still reached it once the
  pairing port was open. If the device does not show up for pairing, try with the VPN off.
- Soloist's WebSocket has no authentication. Keep it on `127.0.0.1`, as the unit does.

## Troubleshooting

**HTPC is missing from Spotify's device list.** Check that Soloist runs
(`systemctl --user status soloist`). Before the first pairing, the phone must be on the same
network and the pairing port open (see "Pair it once"); with a VPN up, try without it. After
pairing, the phone finds it through Spotify, from any network.

**"Can't reach Soloist".** Soloist is not running, or the app looks at the wrong address. Look
at `journalctl --user -u soloist`: exit code 10 means the build expired (see above). The app
keeps retrying and recovers by itself once Soloist is back.

**No sound.** Soloist plays to PipeWire's default output. Check it with `wpctl status`, and
check Spotify's own volume on the phone (Soloist starts at 40 %).

**The visualizer shows a message instead of visuals.** It says what is wrong:
- *No visualizer presets found*: put `.milk` files in the folder it names.
- *The visualizer could not start*: libprojectM 4 is not installed.
- *The visualizer needs OpenGL*: the window has no OpenGL. That happens with Slint's software
  renderer, a missing GPU driver, or a projectM built for a different OpenGL flavour (desktop GL
  vs GLES) than the window uses.
- *Waiting for audio…*: nothing is playing on HTPC right now.

**The visuals lag or lead the music.** Adjust `visualizer.delay_ms` (see Configuration).

**The UI does not open when casting.** Check `journalctl --user -u htpc-nowplaying-watch`: it
logs each cast it sees and whether it launched the UI. If the UI cannot open a window, the
watcher is probably not in the graphical session (see step 4).

For more detail, run with `RUST_LOG=htpc_nowplaying=debug`; the visualizer then logs its
frame rate and quality every few seconds.
