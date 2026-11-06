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

Then run Soloist as a user service. The unit starts it through `htpc-nowplaying soloist`, which
adds the speaker name and address from your config and then becomes Soloist (so the unit
needs the app installed at `~/.local/bin/htpc-nowplaying`):

```sh
mkdir -p ~/.config/systemd/user
cp packaging/systemd/soloist.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now soloist.service
```

The device appears in Spotify as **soloist-&lt;hostname&gt;** (for example `soloist-htpc`). To
use another name, set `device_name` under `[daemon]` in `config.toml` (1–32 letters, digits,
space and `- _ . '`), then `systemctl --user restart soloist.service`. The settings panel has
no field for it, because a remote cannot type.

To run Soloist by hand instead, use `htpc-nowplaying soloist` (extra Soloist options go after
`--`, for example `htpc-nowplaying soloist -- -v`), or start Soloist directly as before.

### 3. Install the pairing helper (once, with sudo; optional)

Pairing (next step) needs a firewall port opened for a while, and opening a port takes root.
The helper is a small root-owned script that only does that, and a sudoers rule that lets
your user, and nobody else, run it without a password:

```sh
sudo packaging/install-pairing-helper.sh
```

It installs `/usr/local/libexec/htpc-nowplaying/pairing-port` and
`/etc/sudoers.d/htpc-nowplaying`. The helper only touches the rule it created itself, and
opens the port for your local subnet only, never the whole internet. It supports ufw and
firewalld. Without a firewall there is nothing to open. Remove it with
`sudo packaging/install-pairing-helper.sh --uninstall`.

You can skip this step and pair by hand (see the fallback below).

### 4. Pairing

Pairing links a Spotify account to Soloist. A phone that has not paired yet has to reach
Soloist directly on the local network. Soloist listens for this on a **random TCP port that
changes on every start**, so a firewall that denies incoming connections (ufw's default, for
example) blocks it. Discovery itself (mDNS) needs no rule.

Open the settings (hold OK, see "Using it") and turn **Pairing** on. While Soloist runs, the
port it listens on stays open to the local network, and it is closed again when Soloist stops
or when you turn Pairing off. Then pick the device in Spotify on the phone. The setting is
saved as `pairing = "on"` under `[daemon]`.

Leave it on if more than one person in the house casts here. **Soloist holds one account at a
time**: each new pairing replaces the previous one (tested with two phones), so whoever should
be able to cast has to pair again, and only the account paired last can reach Soloist from
outside the local network. With Pairing on, that works without touching the firewall each
time. With it off, pair once and turn it off: afterwards the phone reaches Soloist through
Spotify's servers from anywhere, even mobile data, and no port is needed.

The idle screen says "Not paired" while no account is paired.

**By hand instead** (no helper): `ss -ltnp | grep soloist` lists two ports. The one on
`0.0.0.0:NNNNN` is for pairing; `127.0.0.1:9090` is the WebSocket and needs no rule. Allow the
pairing port from your phone, for example
`sudo ufw allow from <phone-ip> to any port NNNNN proto tcp`, pair, and remove the rule.

If the phone and the HTPC are on the same Tailscale network, pairing also works with the phone's
Tailscale on and no firewall rule at all, because Tailscale lets tailnet traffic in by itself.

### 5. Open the UI when you cast (optional)

```sh
cp packaging/systemd/htpc-nowplaying-watch.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now htpc-nowplaying-watch.service
```

The watcher opens the now-playing screen when you pick the device in Spotify. It opens it once per
cast: close it and it stays closed until you cast again. It needs to run inside your
graphical session; `systemctl --user show-environment | grep DISPLAY` must list
`WAYLAND_DISPLAY` or `DISPLAY` (session managers such as uwsm set this up). You can also start
the UI yourself from the app menu or with `htpc-nowplaying run`. Only one UI runs at a time.

If you run Soloist by hand and Pairing is on, the watcher also reopens the pairing port each
time Soloist (re)starts, even with the UI closed. When the Soloist unit runs it, the unit does
that itself.

### 6. Presets for the visualizer

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

### Settings

**Hold OK** for about half a second (on any screen) to open the settings on the right of the
window. The idle screen reminds you: "Hold OK for settings". A remote that cannot hold a button
can use the `Menu` key instead (the logical key `settings` in `[keys]`). ▲ ▼ move between rows,
◀ ▶ change a value, OK toggles or opens, Back goes up one level and finally closes the panel.
Every change is saved to `config.toml` at once (your comments stay) and applies immediately,
with the visualizer running behind the panel.

| Row | What it does |
|---|---|
| Pairing | Keeps the pairing port open while Soloist runs (see "Pairing") |
| Beat sensitivity | 0.0–5.0 |
| Preset duration | 5–300 seconds |
| Shuffle | Random or alphabetical preset order |
| Visual delay | 0–2000 ms, in steps of 10 |
| Calibrate delay | Flashes the screen on each beat of the music: change the delay until the flash matches what you hear. Needs music playing. |
| Presets | The preset folder as a tree with check boxes. OK switches a preset or a whole folder on or off (kept in `disabled-presets.txt` next to `config.toml`); **Right** on a preset shows it in the visualizer, even a switched-off one, and pauses automatic switching until you leave the tree. |

A broken `config.toml` is never overwritten: the panel says "Not saved" and the change lasts
until the app closes.

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
45 ms; a TV over HDMI or Bluetooth speakers often 100–300 ms. Use Settings → Calibrate delay,
or raise `delay_ms` until beats and visuals line up, in steps of about 20.

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

**The device is missing from Spotify's device list.** Check that Soloist runs
(`systemctl --user status soloist`); the name is `soloist-<hostname>` unless you set
`device_name`. Before the first pairing, the phone must be on the same network and the pairing
port open (see "Pairing"); with a VPN up, try without it. After pairing, the phone finds it
through Spotify, from any network.

**"Pairing helper not installed" in the settings.** Install it (step 3), or pair by hand. If
you installed it, `sudo -n /usr/local/libexec/htpc-nowplaying/pairing-port status` must run
without asking for a password.

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
watcher is probably not in the graphical session (see step 5).

For more detail, run with `RUST_LOG=htpc_nowplaying=debug`; the visualizer then logs its
frame rate and quality every few seconds.
