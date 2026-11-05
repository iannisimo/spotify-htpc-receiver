//! Cover art: fetch over HTTPS, decode, downscale, and keep the last few in memory.

use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// An RGBA8 image, rows top to bottom.
#[derive(Debug, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
    /// Whole request, connect to last byte.
    timeout: Duration,
    max_body_bytes: usize,
    /// Larger images are rejected before decoding (Spotify covers are at most 640 px).
    max_dimension: u32,
    capacity: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            timeout: Duration::from_secs(5),
            max_body_bytes: 8 * 1024 * 1024,
            max_dimension: 4096,
            capacity: 8,
        }
    }
}

type Key = (String, u32);

pub struct CoverCache {
    http: reqwest::Client,
    limits: Limits,
    /// Most recently used first.
    entries: Mutex<VecDeque<(Key, Arc<DecodedImage>)>>,
}

impl CoverCache {
    pub fn new() -> Result<Self, reqwest::Error> {
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(limits.timeout)
            .user_agent(concat!("htpc-nowplaying/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(CoverCache {
            http,
            limits,
            entries: Mutex::new(VecDeque::with_capacity(limits.capacity)),
        })
    }

    /// The image at `url`, scaled down to at most `max_px` on its long side.
    /// `None` on any failure (logged); failures are not cached, so a later call retries.
    pub async fn load(&self, url: &str, max_px: u32) -> Option<Arc<DecodedImage>> {
        let max_px = max_px.max(1);
        let key = (url.to_owned(), max_px);
        if let Some(hit) = self.lookup(&key) {
            return Some(hit);
        }
        let result = match self.fetch(url).await {
            Ok(body) => {
                let limits = self.limits;
                tokio::task::spawn_blocking(move || decode(&body, max_px, limits))
                    .await
                    .unwrap_or_else(|e| Err(format!("decoder task failed: {e}")))
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(image) => {
                let image = Arc::new(image);
                self.insert(key, image.clone());
                Some(image)
            }
            Err(e) => {
                log::warn!("cover {url}: {e}");
                None
            }
        }
    }

    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let max = self.limits.max_body_bytes;
        let too_big = |len: u64| format!("body of {len} bytes is over the {max} byte limit");
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?;
        if let Some(len) = response.content_length()
            && len > max as u64
        {
            return Err(too_big(len));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            if body.len() + chunk.len() > max {
                return Err(too_big((body.len() + chunk.len()) as u64));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn lookup(&self, key: &Key) -> Option<Arc<DecodedImage>> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let index = entries.iter().position(|(k, _)| k == key)?;
        let entry = entries.remove(index)?;
        let image = entry.1.clone();
        entries.push_front(entry);
        Some(image)
    }

    fn insert(&self, key: Key, image: Arc<DecodedImage>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|(k, _)| *k != key);
        entries.push_front((key, image));
        entries.truncate(self.limits.capacity);
    }
}

/// Decodes JPEG or PNG and scales it to fit `max_px` (never up). Dimensions and allocation
/// are checked before decoding, so a small file declaring a huge image is rejected cheaply.
fn decode(bytes: &[u8], max_px: u32, limits: Limits) -> Result<DecodedImage, String> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decode_limits = image::Limits::default();
    decode_limits.max_image_width = Some(limits.max_dimension);
    decode_limits.max_image_height = Some(limits.max_dimension);
    decode_limits.max_alloc = Some(u64::from(limits.max_dimension).pow(2) * 4);
    reader.limits(decode_limits);
    let mut image = reader.decode().map_err(|e| e.to_string())?;
    if image.width() > max_px || image.height() > max_px {
        image = image.resize(max_px, max_px, image::imageops::FilterType::Triangle);
    }
    let rgba = image.into_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const TEST_LIMITS: Limits = Limits {
        timeout: Duration::from_millis(300),
        max_body_bytes: 1024 * 1024,
        max_dimension: 4096,
        capacity: 8,
    };

    #[derive(Clone)]
    enum Reply {
        Ok(&'static str, Vec<u8>),
        Status(u16),
        /// Never answers within the test timeout.
        Hang,
        /// Declares the length up front.
        DeclaredSize(usize),
        /// No Content-Length; just keeps sending.
        Stream(usize),
    }

    /// A minimal HTTP/1.1 server: one request per connection, replies looked up by path.
    struct TestServer {
        base: String,
        hits: Arc<Mutex<HashMap<String, usize>>>,
    }

    impl TestServer {
        async fn start<P: Into<String>>(
            routes: impl IntoIterator<Item = (P, Reply)>,
        ) -> TestServer {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let hits = Arc::new(Mutex::new(HashMap::new()));
            let routes: Arc<HashMap<String, Reply>> =
                Arc::new(routes.into_iter().map(|(p, r)| (p.into(), r)).collect());
            let counter = hits.clone();
            tokio::spawn(async move {
                loop {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    let routes = routes.clone();
                    let counter = counter.clone();
                    tokio::spawn(async move {
                        let mut request = Vec::new();
                        let mut buf = [0u8; 1024];
                        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                            match sock.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => request.extend_from_slice(&buf[..n]),
                            }
                        }
                        let line = String::from_utf8_lossy(&request);
                        let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        *counter.lock().unwrap().entry(path.clone()).or_default() += 1;
                        let reply = routes
                            .get(path.as_str())
                            .cloned()
                            .unwrap_or(Reply::Status(404));
                        let _ = respond(&mut sock, reply).await;
                    });
                }
            });
            TestServer { base, hits }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{path}", self.base)
        }

        fn hits(&self, path: &str) -> usize {
            self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
        }
    }

    async fn respond(sock: &mut tokio::net::TcpStream, reply: Reply) -> std::io::Result<()> {
        match reply {
            Reply::Ok(content_type, body) => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                sock.write_all(head.as_bytes()).await?;
                sock.write_all(&body).await
            }
            Reply::Status(code) => {
                let head = format!(
                    "HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                sock.write_all(head.as_bytes()).await
            }
            Reply::Hang => {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(())
            }
            Reply::DeclaredSize(len) => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                );
                sock.write_all(head.as_bytes()).await?;
                sock.write_all(&vec![0u8; len]).await
            }
            Reply::Stream(len) => {
                let head =
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nConnection: close\r\n\r\n";
                sock.write_all(head.as_bytes()).await?;
                for _ in 0..len.div_ceil(64 * 1024) {
                    sock.write_all(&[0u8; 64 * 1024]).await?;
                }
                Ok(())
            }
        }
    }

    fn encoded(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let img =
            image::RgbImage::from_fn(width, height, |x, y| image::Rgb([x as u8, y as u8, 128]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    fn png(width: u32, height: u32) -> Reply {
        Reply::Ok("image/png", encoded(width, height, image::ImageFormat::Png))
    }

    fn cache() -> CoverCache {
        CoverCache::with_limits(TEST_LIMITS).unwrap()
    }

    #[tokio::test]
    async fn decodes_and_downscales_to_max_px() {
        let server = TestServer::start(HashMap::from([("/wide.png", png(800, 400))])).await;
        let img = cache().load(&server.url("/wide.png"), 300).await.unwrap();
        assert_eq!((img.width, img.height), (300, 150));
        assert_eq!(img.rgba.len(), 300 * 150 * 4);
    }

    #[tokio::test]
    async fn small_image_is_not_upscaled() {
        let server = TestServer::start(HashMap::from([("/small.png", png(64, 32))])).await;
        let img = cache().load(&server.url("/small.png"), 300).await.unwrap();
        assert_eq!((img.width, img.height), (64, 32));
    }

    #[tokio::test]
    async fn decodes_jpeg() {
        let jpeg = Reply::Ok("image/jpeg", encoded(640, 640, image::ImageFormat::Jpeg));
        let server = TestServer::start(HashMap::from([("/cover.jpg", jpeg)])).await;
        let img = cache().load(&server.url("/cover.jpg"), 640).await.unwrap();
        assert_eq!((img.width, img.height), (640, 640));
    }

    #[tokio::test]
    async fn cache_hit_does_not_refetch() {
        let server = TestServer::start(HashMap::from([("/a.png", png(10, 10))])).await;
        let cache = cache();
        let first = cache.load(&server.url("/a.png"), 300).await.unwrap();
        let second = cache.load(&server.url("/a.png"), 300).await.unwrap();
        assert_eq!(server.hits("/a.png"), 1);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn other_size_is_a_separate_entry() {
        let server = TestServer::start(HashMap::from([("/a.png", png(400, 400))])).await;
        let cache = cache();
        let big = cache.load(&server.url("/a.png"), 400).await.unwrap();
        let small = cache.load(&server.url("/a.png"), 100).await.unwrap();
        assert_eq!((big.width, small.width), (400, 100));
    }

    #[tokio::test]
    async fn lru_evicts_oldest() {
        let paths: Vec<String> = (0..9).map(|i| format!("/{i}.png")).collect();
        let server = TestServer::start(paths.iter().map(|p| (p.clone(), png(4, 4)))).await;
        let cache = cache();
        for p in &paths[..8] {
            cache.load(&server.url(p), 64).await.unwrap();
        }
        // Touch 0 so that 1 becomes the oldest, then overflow with 8.
        cache.load(&server.url(&paths[0]), 64).await.unwrap();
        cache.load(&server.url(&paths[8]), 64).await.unwrap();

        cache.load(&server.url(&paths[0]), 64).await.unwrap();
        assert_eq!(server.hits(&paths[0]), 1, "recently used entry kept");
        cache.load(&server.url(&paths[1]), 64).await.unwrap();
        assert_eq!(server.hits(&paths[1]), 2, "oldest entry was evicted");
    }

    #[tokio::test]
    async fn timeout_returns_none() {
        let server = TestServer::start(HashMap::from([("/slow.png", Reply::Hang)])).await;
        let started = std::time::Instant::now();
        assert!(cache().load(&server.url("/slow.png"), 300).await.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn non_image_body_returns_none() {
        let html = Reply::Ok("text/html", b"<html>not a cover</html>".to_vec());
        let server = TestServer::start(HashMap::from([("/page", html)])).await;
        assert!(cache().load(&server.url("/page"), 300).await.is_none());
    }

    #[tokio::test]
    async fn http_error_returns_none() {
        let server = TestServer::start(HashMap::<String, Reply>::new()).await;
        assert!(
            cache()
                .load(&server.url("/missing.png"), 300)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_body_returns_none() {
        let limit = TEST_LIMITS.max_body_bytes;
        let server = TestServer::start(HashMap::from([
            ("/declared.png", Reply::DeclaredSize(limit + 1)),
            ("/streamed.png", Reply::Stream(limit + 64 * 1024)),
        ]))
        .await;
        let cache = cache();
        assert!(
            cache
                .load(&server.url("/declared.png"), 300)
                .await
                .is_none()
        );
        assert!(
            cache
                .load(&server.url("/streamed.png"), 300)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn huge_dimensions_return_none() {
        // A small file can declare a huge image; it must be rejected before decoding.
        let server = TestServer::start(HashMap::from([("/tall.png", png(1, 5000))])).await;
        assert!(cache().load(&server.url("/tall.png"), 300).await.is_none());
    }

    #[tokio::test]
    async fn failures_are_not_cached() {
        let server = TestServer::start(HashMap::<String, Reply>::new()).await;
        let cache = cache();
        assert!(cache.load(&server.url("/x.png"), 300).await.is_none());
        assert!(cache.load(&server.url("/x.png"), 300).await.is_none());
        assert_eq!(server.hits("/x.png"), 2);
    }

    #[tokio::test]
    async fn bad_url_or_unreachable_host_returns_none() {
        let cache = cache();
        assert!(cache.load("not a url", 300).await.is_none());
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(
            cache
                .load(&format!("http://127.0.0.1:{port}/a.png"), 300)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn zero_max_px_still_gives_an_image() {
        let server = TestServer::start(HashMap::from([("/a.png", png(10, 10))])).await;
        let img = cache().load(&server.url("/a.png"), 0).await.unwrap();
        assert_eq!((img.width, img.height), (1, 1));
    }

    /// Checks HTTPS against Spotify's image CDN (the local tests are plain HTTP).
    /// Run manually: `cargo test --lib -- --ignored real_spotify_cover`
    #[tokio::test]
    #[ignore = "needs internet access"]
    async fn real_spotify_cover() {
        // "Tangerine" cover from tests/fixtures/soloist/track_changed.json.
        let url = "https://i.scdn.co/image/ab67616d0000b273712701c5e263efc8726b1464";
        let img = CoverCache::new().unwrap().load(url, 300).await.unwrap();
        assert_eq!((img.width, img.height), (300, 300));
    }
}
