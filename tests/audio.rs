//! Audio capture against a real PipeWire. Ignored by default; run with
//! `cargo test --test audio -- --ignored --test-threads 1`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use htpc_nowplaying::audio::{self, AudioStatus, Capture, StreamMatch};

const WAIT: Duration = Duration::from_secs(5);
const RATE: u32 = 48_000;
/// Name our capture node has in PipeWire.
const CAPTURE_NODE: &str = "htpc-nowplaying-visualizer";

/// A null sink, so the test streams are silent. Unloaded on drop.
struct NullSink {
    name: String,
    module: String,
}

impl NullSink {
    fn load() -> NullSink {
        let name = format!("htpc_audio_test_{}", std::process::id());
        let out = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={name}"),
            ])
            .output()
            .expect("pactl");
        assert!(out.status.success(), "pactl load-module failed");
        let module = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        NullSink { name, module }
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        let _ = Command::new("pactl")
            .args(["unload-module", &self.module])
            .status();
    }
}

/// A playing process, killed on drop.
struct Player(Child);

impl Player {
    fn spawn(cmd: &mut Command) -> Player {
        Player(
            cmd.stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("player"),
        )
    }

    fn stop(mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Writes a 16-bit stereo WAV: a sine of `amplitude` at `hz` (0 amplitude = silence).
fn wav(dir: &Path, name: &str, hz: f32, amplitude: f32, secs: u32) -> PathBuf {
    let frames = RATE * secs;
    let mut data = Vec::with_capacity(frames as usize * 4);
    for i in 0..frames {
        let v = amplitude * (2.0 * std::f32::consts::PI * hz * i as f32 / RATE as f32).sin();
        let s = (v * f32::from(i16::MAX)) as i16;
        data.extend(s.to_le_bytes());
        data.extend(s.to_le_bytes());
    }
    let mut out = Vec::new();
    out.extend(b"RIFF");
    out.extend((36 + data.len() as u32).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(2u16.to_le_bytes()); // channels
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(data);
    let path = dir.join(name);
    std::fs::write(&path, out).unwrap();
    path
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("htpc-audio-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn next_status(status: &Receiver<AudioStatus>) -> AudioStatus {
    status.recv_timeout(WAIT).expect("no status change")
}

/// RMS of what the capture delivers over `secs`, after discarding what is already queued.
fn rms_over(capture: &mut Capture, secs: f32) -> f32 {
    let mut buf = Vec::new();
    capture.read(&mut buf);
    let (mut sum, mut n) = (0.0f64, 0usize);
    let end = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
        capture.read(&mut buf);
        sum += buf
            .iter()
            .map(|&v| f64::from(v) * f64::from(v))
            .sum::<f64>();
        n += buf.len();
    }
    assert!(n > 0, "no audio captured");
    (sum / n as f64).sqrt() as f32
}

fn capture_nodes_left() -> bool {
    let out = Command::new("pw-dump").output().expect("pw-dump");
    String::from_utf8_lossy(&out.stdout).contains(CAPTURE_NODE)
}

#[test]
#[ignore = "needs PipeWire with pipewire-pulse, pactl, paplay and pw-play"]
fn captures_only_target_stream_and_follows_lifecycle() {
    let sink = NullSink::load();
    let dir = temp_dir();
    let silence = wav(&dir, "silence.wav", 0.0, 0.0, 30);
    let tone = wav(&dir, "tone.wav", 440.0, 0.5, 30);
    let play_target = |file: &Path| {
        Player::spawn(
            Command::new("pw-play")
                .arg("--target")
                .arg(&sink.name)
                .arg(file),
        )
    };

    // The "daemon" is pw-play (binary pw-cat), a native PipeWire client like Soloist. The other
    // app is paplay, playing a loud tone into the same sink. (A PulseAudio-protocol stream like
    // paplay's cannot be the target: PipeWire hands captures of it only silence.)
    let target = StreamMatch {
        binary: "pw-cat".to_owned(),
    };
    let (mut capture, status) = audio::start(target, 0).expect("start");
    assert_eq!(next_status(&status), AudioStatus::Waiting);

    let _other = Player::spawn(Command::new("paplay").arg("-d").arg(&sink.name).arg(&tone));
    let daemon = play_target(&silence);
    assert_eq!(next_status(&status), AudioStatus::Active);
    let rms = rms_over(&mut capture, 1.5);
    assert!(rms < 1e-4, "other app leaked into the capture: rms {rms}");

    daemon.stop();
    assert_eq!(next_status(&status), AudioStatus::Lost);
    let mut buf = Vec::new();
    std::thread::sleep(Duration::from_millis(500));
    capture.read(&mut buf);
    assert!(
        buf.is_empty(),
        "captured {} samples with no target",
        buf.len()
    );

    // Comes back as a new node with a new serial: re-linked, and this time it carries the tone.
    let _daemon = play_target(&tone);
    assert_eq!(next_status(&status), AudioStatus::Active);
    let rms = rms_over(&mut capture, 1.5);
    assert!(
        (0.3..0.4).contains(&rms),
        "expected the tone (rms ~0.354), got {rms}"
    );

    drop(capture);
    assert!(
        !capture_nodes_left(),
        "capture node still in PipeWire after drop"
    );
    drop(sink);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
#[ignore = "needs Soloist playing music from the phone, and pw-play"]
fn captures_only_daemon_stream() {
    use htpc_nowplaying::config::Config;
    use htpc_nowplaying::model::Command as PlayerCommand;

    let dir = temp_dir();
    // A soft tone on the default output, from another app, for the whole test.
    let tone = wav(&dir, "tone.wav", 440.0, 0.05, 30);
    let _other = Player::spawn(Command::new("pw-play").arg(&tone));

    let (mut capture, status) = audio::start(StreamMatch::soloist(), 0).expect("start");
    assert_eq!(
        next_status(&status),
        AudioStatus::Active,
        "is Soloist playing?"
    );
    let playing = rms_over(&mut capture, 2.0);
    assert!(playing > 0.01, "no music captured: rms {playing}");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let (events, _events_rx) = tokio::sync::mpsc::channel(64);
    let client = {
        let _guard = rt.enter();
        htpc_nowplaying::player::spawn(Config::default().daemon, events)
    };
    std::thread::sleep(Duration::from_secs(1)); // connect
    client.send(PlayerCommand::PlayPause);
    std::thread::sleep(Duration::from_millis(1500));
    let paused = rms_over(&mut capture, 1.5);
    client.send(PlayerCommand::PlayPause);
    std::thread::sleep(Duration::from_millis(500));

    assert!(
        paused < 1e-4,
        "captured sound while Spotify was paused: rms {paused}"
    );
    assert_eq!(
        status.try_recv().ok(),
        None,
        "pausing must not change the status"
    );
    let _ = std::fs::remove_dir_all(dir);
}
