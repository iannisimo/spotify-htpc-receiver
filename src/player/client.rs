//! Connection to Soloist: forwards its events, sends commands, reconnects forever.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::protocol::{WireCommand, encode_command, parse_message};
use crate::config::DaemonConfig;
use crate::model::{Command, Playback, PlayerEvent};

/// Commands waiting to be sent; a remote cannot press buttons faster than this drains.
const COMMAND_QUEUE: usize = 32;

/// Handle to the background connection task. Dropping it closes the connection.
pub struct PlayerClient {
    commands: mpsc::Sender<Command>,
}

/// Starts the connection task. Must be called from within a tokio runtime.
///
/// Emits `Connected` on each successful connect, then the events parsed from Soloist's
/// messages, and `Disconnected` when an established connection is lost. Failed connection
/// attempts emit nothing; the task retries with [`Backoff`] for as long as the client and the
/// event receiver exist.
pub fn spawn(cfg: DaemonConfig, events: mpsc::Sender<PlayerEvent>) -> PlayerClient {
    let (commands, rx) = mpsc::channel(COMMAND_QUEUE);
    tokio::spawn(run(format!("ws://{}", cfg.address), events, rx));
    PlayerClient { commands }
}

impl PlayerClient {
    /// Queues a command without waiting. Failures are logged, never returned.
    pub fn send(&self, cmd: Command) {
        if let Err(e) = self.commands.try_send(cmd) {
            log::warn!("dropping {cmd:?}: {e}");
        }
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

enum SessionEnd {
    /// The connection was lost; try again.
    Reconnect,
    /// The client or the event receiver is gone; stop for good.
    Stop,
}

async fn run(
    url: String,
    events: mpsc::Sender<PlayerEvent>,
    mut commands: mpsc::Receiver<Command>,
) {
    let mut backoff = Backoff::new();
    let mut failure_logged = false;
    loop {
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => {
                log::info!("connected to Soloist at {url}");
                backoff.reset();
                failure_logged = false;
                if events.send(PlayerEvent::Connected).await.is_err() {
                    return;
                }
                if let SessionEnd::Stop = session(ws, &events, &mut commands).await {
                    return;
                }
                if events.send(PlayerEvent::Disconnected).await.is_err() {
                    return;
                }
            }
            // Warn once per outage; the retries after that would only repeat it.
            Err(e) if !failure_logged => {
                log::warn!("cannot connect to Soloist at {url}: {e}; retrying");
                failure_logged = true;
            }
            Err(e) => log::debug!("cannot connect to Soloist at {url}: {e}"),
        }

        let sleep = tokio::time::sleep(backoff.next_delay());
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = &mut sleep => break,
                cmd = commands.recv() => match cmd {
                    // Not queued: a stale skip arriving after a reconnect would surprise.
                    Some(cmd) => log::warn!("not connected to Soloist; dropping {cmd:?}"),
                    None => return,
                },
            }
        }
    }
}

async fn session(
    ws: Ws,
    events: &mpsc::Sender<PlayerEvent>,
    commands: &mut mpsc::Receiver<Command>,
) -> SessionEnd {
    let (mut sink, mut stream) = ws.split();
    let mut known = Known::default();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                // tungstenite reassembles fragmented messages before handing them over.
                Some(Ok(Message::Text(text))) => match parse_message(&text) {
                    Ok(parsed) => {
                        for ev in parsed {
                            known.observe(&ev);
                            if events.send(ev).await.is_err() {
                                return SessionEnd::Stop;
                            }
                        }
                    }
                    Err(e) => log::warn!("ignoring malformed Soloist message: {e}"),
                },
                Some(Ok(Message::Close(_))) | None => {
                    log::warn!("Soloist closed the connection");
                    return SessionEnd::Reconnect;
                }
                Some(Ok(_)) => {} // binary, ping, pong
                Some(Err(e)) => {
                    log::warn!("lost the connection to Soloist: {e}");
                    return SessionEnd::Reconnect;
                }
            },
            cmd = commands.recv() => {
                let Some(cmd) = cmd else {
                    let _ = sink.close().await;
                    return SessionEnd::Stop;
                };
                if let Some(wire) = known.translate(cmd)
                    && let Err(e) = sink.send(Message::text(encode_command(&wire))).await
                {
                    log::warn!("cannot send {wire:?} to Soloist: {e}");
                    return SessionEnd::Reconnect;
                }
            },
        }
    }
}

/// What Soloist last reported, needed because it has no play/pause toggle and no relative
/// volume. Reset on every connection, since a fresh state arrives right after connecting.
#[derive(Default)]
struct Known {
    playing: bool,
    volume: Option<u8>,
}

impl Known {
    fn observe(&mut self, ev: &PlayerEvent) {
        match ev {
            PlayerEvent::PlaybackChanged(p) => {
                self.playing = matches!(p, Playback::Playing | Playback::Buffering);
            }
            PlayerEvent::DeviceActive(false) | PlayerEvent::TrackChanged(None) => {
                self.playing = false;
            }
            PlayerEvent::VolumeChanged(v) => self.volume = Some(*v),
            _ => {}
        }
    }

    /// The wire command for a user command; `None` if it cannot be computed yet.
    fn translate(&mut self, cmd: Command) -> Option<WireCommand> {
        match cmd {
            Command::Previous => Some(WireCommand::SkipPrev),
            Command::Next => Some(WireCommand::SkipNext),
            Command::PlayPause if self.playing => Some(WireCommand::Pause),
            Command::PlayPause => Some(WireCommand::Play),
            Command::VolumeDelta(delta) => {
                let Some(volume) = self.volume else {
                    log::warn!("Soloist volume not known yet; dropping {cmd:?}");
                    return None;
                };
                let target = (i16::from(volume) + i16::from(delta)).clamp(0, 100) as u8;
                // Optimistic, so quick repeated presses each take a step.
                self.volume = Some(target);
                Some(WireCommand::SetVolume(target))
            }
        }
    }
}

/// Reconnect delays: 1 s, doubling, capped at 10 s; back to 1 s after a successful connect.
struct Backoff {
    next: Duration,
}

impl Backoff {
    const FIRST: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(10);

    fn new() -> Self {
        Backoff { next: Self::FIRST }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.next;
        self.next = (delay * 2).min(Self::MAX);
        delay
    }

    fn reset(&mut self) {
        self.next = Self::FIRST;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(delays: &[Duration]) -> Vec<u64> {
        delays.iter().map(Duration::as_secs).collect()
    }

    #[test]
    fn backoff_doubles_and_caps_at_10s() {
        let mut b = Backoff::new();
        let delays: Vec<_> = (0..7).map(|_| b.next_delay()).collect();
        assert_eq!(secs(&delays), [1, 2, 4, 8, 10, 10, 10]);
    }

    #[test]
    fn backoff_resets_after_success() {
        let mut b = Backoff::new();
        for _ in 0..5 {
            b.next_delay();
        }
        b.reset();
        assert_eq!(b.next_delay(), Backoff::FIRST);
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        assert_eq!(Backoff::MAX, Duration::from_secs(10));
    }
}
