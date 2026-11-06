//! A fake Soloist for integration tests: accepts the client's WebSocket and replays recorded
//! messages.

#![allow(dead_code)] // each test binary uses a different part

use std::path::Path;
use std::time::Duration;

use futures_util::SinkExt;
use htpc_nowplaying::config::DaemonConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

/// Upper bound for anything a test waits for; reconnects take about 1 s.
pub const WAIT: Duration = Duration::from_secs(5);

pub type Ws = WebSocketStream<TcpStream>;

pub fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/soloist")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub struct FakeSoloist {
    pub listener: TcpListener,
}

impl FakeSoloist {
    pub async fn start() -> (FakeSoloist, DaemonConfig) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cfg = DaemonConfig {
            address: listener.local_addr().unwrap().to_string(),
        };
        (FakeSoloist { listener }, cfg)
    }

    /// Listens on an address chosen earlier (a daemon that comes up late).
    pub async fn start_at(address: &str) -> FakeSoloist {
        FakeSoloist {
            listener: TcpListener::bind(address).await.unwrap(),
        }
    }

    /// An address nothing listens on yet.
    pub fn free_address() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().to_string()
    }

    pub async fn accept(&self) -> Ws {
        let (stream, _) = timeout(WAIT, self.listener.accept())
            .await
            .expect("client connects")
            .unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    }
}

pub async fn send_text(ws: &mut Ws, text: &str) {
    ws.send(Message::text(text)).await.unwrap();
}
