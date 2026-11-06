//! Capture of the player daemon's own PipeWire stream, for the visualizer.
//!
//! [`start`] spawns a thread that runs a PipeWire main loop. It watches the registry for the
//! daemon's playback stream, links a capture stream to it and queues the samples in a
//! [`DelayLine`] that [`Capture::read`] drains. The stream is matched by the binary of the process
//! that owns it, taken from the node or else from its client (Soloist sets it only on the
//! client). It is linked by `object.serial`, which changes each time the stream is re-created.
//! The capture never falls back to another source: without `node.dont-reconnect` PipeWire would
//! relink it to the microphone when the daemon's stream goes away.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::client::{Client, ClientListener};
use pw::node::{Node, NodeListener};
use pw::spa;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::pod::Pod;
use pw::spa::utils::dict::DictRef;
use pw::stream::{StreamFlags, StreamListener, StreamRc, StreamState};
use pw::types::ObjectType;

/// Captured audio is interleaved stereo.
const CHANNELS: u32 = 2;
/// Rate assumed until PipeWire reports the negotiated one (the stream's native rate).
const DEFAULT_RATE: u32 = 48_000;
/// How long captured audio is kept past its delay when nobody reads it.
const HEADROOM: Duration = Duration::from_millis(500);
/// Name of our capture node in PipeWire.
const NODE_NAME: &str = "htpc-nowplaying-visualizer";
const OUTPUT_STREAM_CLASS: &str = "Stream/Output/Audio";
/// Linux errno PipeWire reports (negated) when its connection breaks.
const EPIPE: i32 = 32;

/// Which playback stream to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMatch {
    /// `application.process.binary` of the process that owns the stream.
    pub binary: String,
}

impl StreamMatch {
    pub fn soloist() -> Self {
        StreamMatch {
            binary: "soloist".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioStatus {
    /// The daemon has no playback stream yet.
    Waiting,
    /// Capturing. Paused playback keeps the stream, so this stays `Active` and delivers silence.
    Active,
    /// The stream went away (playback moved to another device, daemon stopped, PipeWire gone).
    /// `Active` follows if it comes back.
    Lost,
}

#[derive(Debug)]
pub enum AudioError {
    /// PipeWire could not be reached or set up.
    PipeWire(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::PipeWire(msg) => write!(f, "PipeWire audio capture: {msg}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// A running capture. Dropping it stops the stream and its thread.
pub struct Capture {
    buffer: Arc<Mutex<DelayLine>>,
    quit: Option<pw::channel::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Replaces `out` with the interleaved stereo f32 samples that are due, oldest first.
    /// Never waits for audio; `out` is empty when nothing is due.
    pub fn read(&mut self, out: &mut Vec<f32>) {
        lock(&self.buffer).read(Instant::now(), out);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(quit) = self.quit.take() {
            // Fails only if the loop already stopped on its own.
            let _ = quit.send(());
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::warn!("audio capture thread panicked");
        }
    }
}

/// Starts capturing the stream that `target` matches, delaying delivery by `delay_ms`.
///
/// Status changes arrive on the returned channel: `Waiting` if the stream does not exist yet,
/// then `Active`/`Lost` as it appears and disappears. Fails only if PipeWire is unreachable.
pub fn start(
    target: StreamMatch,
    delay_ms: u32,
) -> Result<(Capture, mpsc::Receiver<AudioStatus>), AudioError> {
    let delay = Duration::from_millis(delay_ms.into());
    let buffer = Arc::new(Mutex::new(DelayLine::new(
        delay,
        capacity(DEFAULT_RATE, delay),
    )));
    let (status_tx, status_rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread_buffer = buffer.clone();
    let thread = std::thread::Builder::new()
        .name("audio-capture".to_owned())
        .spawn(move || thread_main(target, thread_buffer, status_tx, ready_tx))
        .map_err(|e| AudioError::PipeWire(format!("cannot start thread: {e}")))?;
    let quit = match ready_rx.recv() {
        Ok(Ok(quit)) => quit,
        Ok(Err(e)) => {
            let _ = thread.join();
            return Err(e);
        }
        Err(_) => {
            let _ = thread.join();
            return Err(AudioError::PipeWire(
                "thread exited during setup".to_owned(),
            ));
        }
    };
    let capture = Capture {
        buffer,
        quit: Some(quit),
        thread: Some(thread),
    };
    Ok((capture, status_rx))
}

/// Samples needed to hold `delay` plus [`HEADROOM`] at `rate`, in whole stereo frames.
fn capacity(rate: u32, delay: Duration) -> usize {
    let frames = (delay + HEADROOM).as_secs_f64() * f64::from(rate);
    frames.ceil() as usize * CHANNELS as usize
}

fn lock(buffer: &Mutex<DelayLine>) -> MutexGuard<'_, DelayLine> {
    buffer.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------------------- delay line

/// Holds captured samples until `delay` has passed since they arrived, so the visuals line up
/// with what is heard. Memory is bounded: past `capacity` samples the oldest are dropped.
#[derive(Debug)]
struct DelayLine {
    delay: Duration,
    capacity: usize,
    samples: VecDeque<f32>,
    /// One entry per pushed chunk still (partly) held: when it is due, and how many samples of
    /// it remain. The counts add up to `samples.len()`.
    chunks: VecDeque<(Instant, usize)>,
}

impl DelayLine {
    fn new(delay: Duration, capacity: usize) -> Self {
        DelayLine {
            delay,
            capacity,
            samples: VecDeque::with_capacity(capacity),
            chunks: VecDeque::new(),
        }
    }

    /// Empties the line and sets a new capacity (the sample rate changed).
    fn reset(&mut self, capacity: usize) {
        *self = DelayLine::new(self.delay, capacity);
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.chunks.clear();
    }

    /// Queues `chunk`, which arrived at `now`. Keeps whole stereo frames as long as chunks and
    /// the capacity are whole frames, which they are.
    fn push(&mut self, now: Instant, chunk: &[f32]) {
        let chunk = &chunk[chunk.len().saturating_sub(self.capacity)..];
        if chunk.is_empty() {
            return;
        }
        let mut excess = (self.samples.len() + chunk.len()).saturating_sub(self.capacity);
        self.samples.drain(..excess);
        while excess > 0 {
            let Some(front) = self.chunks.front_mut() else {
                break;
            };
            if front.1 <= excess {
                excess -= front.1;
                self.chunks.pop_front();
            } else {
                front.1 -= excess;
                excess = 0;
            }
        }
        self.samples.extend(chunk);
        self.chunks.push_back((now + self.delay, chunk.len()));
    }

    /// Replaces `out` with every sample due at `now`.
    fn read(&mut self, now: Instant, out: &mut Vec<f32>) {
        out.clear();
        let mut due = 0;
        while let Some(&(at, len)) = self.chunks.front() {
            if at > now {
                break;
            }
            due += len;
            self.chunks.pop_front();
        }
        out.extend(self.samples.drain(..due));
    }
}

// ---------------------------------------------------------------------------- stream matching

/// What is known about one stream node; fields fill in as registry and info events arrive.
#[derive(Debug, Default, Clone, PartialEq)]
struct NodeProps {
    class: Option<String>,
    serial: Option<String>,
    client: Option<u32>,
    binary: Option<String>,
}

impl NodeProps {
    fn from_dict(d: Option<&DictRef>) -> Self {
        let get = |k: &str| d.and_then(|d| d.get(k)).map(str::to_owned);
        NodeProps {
            class: get("media.class"),
            serial: get("object.serial"),
            client: get("client.id").and_then(|s| s.parse().ok()),
            binary: get("application.process.binary"),
        }
    }

    /// Takes every field `newer` knows.
    fn merge(&mut self, newer: NodeProps) {
        self.class = newer.class.or(self.class.take());
        self.serial = newer.serial.or(self.serial.take());
        self.client = newer.client.or(self.client);
        self.binary = newer.binary.or(self.binary.take());
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    /// A matching stream is now the target; link to this serial.
    Appeared { serial: String },
    /// The target stream was removed.
    Gone,
}

/// Picks the target stream from what the registry has reported. Pure, so it can be tested
/// without PipeWire.
#[derive(Debug)]
struct Matcher {
    binary: String,
    nodes: HashMap<u32, NodeProps>,
    clients: HashMap<u32, String>,
    matched: Option<u32>,
}

impl Matcher {
    fn new(target: &StreamMatch) -> Self {
        Matcher {
            binary: target.binary.clone(),
            nodes: HashMap::new(),
            clients: HashMap::new(),
            matched: None,
        }
    }

    fn node(&mut self, id: u32, props: NodeProps) -> Option<Change> {
        self.nodes.entry(id).or_default().merge(props);
        self.check()
    }

    fn client(&mut self, id: u32, binary: Option<String>) -> Option<Change> {
        if let Some(binary) = binary {
            self.clients.insert(id, binary);
        }
        self.check()
    }

    /// A global went away. Returns `Gone` if it was the target, then `Appeared` if another
    /// matching stream takes over.
    fn removed(&mut self, id: u32) -> Vec<Change> {
        self.nodes.remove(&id);
        self.clients.remove(&id);
        if self.matched != Some(id) {
            return Vec::new();
        }
        self.matched = None;
        let mut changes = vec![Change::Gone];
        changes.extend(self.check());
        changes
    }

    fn is_matched(&self) -> bool {
        self.matched.is_some()
    }

    fn check(&mut self) -> Option<Change> {
        if self.matched.is_some() {
            return None;
        }
        let (id, serial) = self
            .nodes
            .iter()
            .filter(|(_, n)| n.class.as_deref() == Some(OUTPUT_STREAM_CLASS))
            .filter(|(_, n)| {
                let binary = n
                    .binary
                    .as_deref()
                    .or_else(|| self.clients.get(&n.client?).map(String::as_str));
                binary == Some(self.binary.as_str())
            })
            .filter_map(|(&id, n)| Some((id, n.serial.clone()?)))
            .min_by_key(|&(id, _)| id)?;
        self.matched = Some(id);
        Some(Change::Appeared { serial })
    }
}

// ----------------------------------------------------------------------------- PipeWire thread

type ReadySender = mpsc::SyncSender<Result<pw::channel::Sender<()>, AudioError>>;

fn thread_main(
    target: StreamMatch,
    buffer: Arc<Mutex<DelayLine>>,
    status: mpsc::Sender<AudioStatus>,
    ready: ReadySender,
) {
    let mut ready = Some(ready);
    if let Err(e) = run_loop(&target, buffer, status, &mut ready) {
        match ready.take() {
            Some(ready) => {
                let _ = ready.send(Err(AudioError::PipeWire(e)));
            }
            None => log::warn!("audio capture stopped: {e}"),
        }
    }
}

/// Bound proxies must stay alive for their info events to arrive.
enum Bound {
    Node(#[allow(dead_code)] Node, #[allow(dead_code)] NodeListener),
    Client(
        #[allow(dead_code)] Client,
        #[allow(dead_code)] ClientListener,
    ),
}

/// State shared by the loop's callbacks. Callbacks hold it weakly, so dropping the one strong
/// reference in [`run_loop`] frees every proxy, listener and the capture stream.
struct Shared {
    matcher: Matcher,
    bound: HashMap<u32, Bound>,
    stream: Option<(StreamRc, StreamListener<()>)>,
    status: Option<AudioStatus>,
    status_tx: mpsc::Sender<AudioStatus>,
}

impl Shared {
    fn set_status(&mut self, status: AudioStatus) {
        if self.status != Some(status) {
            self.status = Some(status);
            // The receiver may be gone while the capture is being dropped.
            let _ = self.status_tx.send(status);
        }
    }
}

fn run_loop(
    target: &StreamMatch,
    buffer: Arc<Mutex<DelayLine>>,
    status_tx: mpsc::Sender<AudioStatus>,
    ready: &mut Option<ReadySender>,
) -> Result<(), String> {
    pw::init();
    fn setup(what: &str) -> impl FnOnce(pw::Error) -> String + '_ {
        move |e| format!("{what}: {e}")
    }
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(setup("cannot create loop"))?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(setup("cannot create context"))?;
    let core = context
        .connect_rc(None)
        .map_err(setup("cannot connect (is PipeWire running?)"))?;
    let registry = core
        .get_registry_rc()
        .map_err(setup("cannot get registry"))?;

    let shared = Rc::new(RefCell::new(Shared {
        matcher: Matcher::new(target),
        bound: HashMap::new(),
        stream: None,
        status: None,
        status_tx,
    }));

    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let quit_loop = mainloop.downgrade();
    let _quit = quit_rx.attach(mainloop.loop_(), move |()| {
        if let Some(mainloop) = quit_loop.upgrade() {
            mainloop.quit();
        }
    });

    // Report `Waiting` only once the registry's initial contents and the info of everything
    // bound from it have arrived: two round trips. Otherwise a stream that already exists would
    // briefly show as missing.
    let first_sync = core.sync(0).map_err(setup("cannot sync"))?.seq();
    let second_sync = Rc::new(RefCell::new(None));
    let core_weak = core.downgrade();
    let (w_done, w_err) = (Rc::downgrade(&shared), Rc::downgrade(&shared));
    let lost_loop = mainloop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id != pw::core::PW_ID_CORE {
                return;
            }
            let seq = seq.seq();
            if seq == first_sync {
                let next = core_weak.upgrade().and_then(|core| core.sync(0).ok());
                *second_sync.borrow_mut() = next.map(|s| s.seq());
            } else if Some(seq) == *second_sync.borrow()
                && let Some(shared) = w_done.upgrade()
            {
                let mut shared = shared.borrow_mut();
                if !shared.matcher.is_matched() {
                    shared.set_status(AudioStatus::Waiting);
                }
            }
        })
        .error(move |id, _seq, res, message| {
            if id == pw::core::PW_ID_CORE && res == -EPIPE {
                // The connection to PipeWire is gone; nothing will work until a new capture.
                log::warn!("PipeWire connection lost: {message}");
                if let Some(shared) = w_err.upgrade() {
                    shared.borrow_mut().set_status(AudioStatus::Lost);
                }
                if let Some(mainloop) = lost_loop.upgrade() {
                    mainloop.quit();
                }
            } else {
                log::warn!("PipeWire error on object {id} ({res}): {message}");
            }
        })
        .register();

    let registry_weak = registry.downgrade();
    let (w_global, w_remove) = (Rc::downgrade(&shared), Rc::downgrade(&shared));
    let core_global = core.clone();
    let core_remove = core.clone();
    let (buffer_global, buffer_remove) = (buffer.clone(), buffer.clone());
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            let (Some(registry), Some(shared)) = (registry_weak.upgrade(), w_global.upgrade())
            else {
                return;
            };
            let props = obj.props.as_ref().map(|p| p.as_ref());
            match obj.type_ {
                ObjectType::Node => {
                    let node_props = NodeProps::from_dict(props);
                    // Sinks, sources and devices never match; skip binding them.
                    if node_props
                        .class
                        .as_deref()
                        .is_some_and(|c| !c.starts_with("Stream/"))
                    {
                        return;
                    }
                    let change = shared.borrow_mut().matcher.node(obj.id, node_props);
                    apply(&shared, change, &core_global, &buffer_global);
                    let node: Node = match registry.bind(obj) {
                        Ok(node) => node,
                        Err(e) => return log::debug!("cannot bind node {}: {e}", obj.id),
                    };
                    let (id, w_info, core_info, buffer_info) = (
                        obj.id,
                        Rc::downgrade(&shared),
                        core_global.clone(),
                        buffer_global.clone(),
                    );
                    let listener = node
                        .add_listener_local()
                        .info(move |info| {
                            let Some(shared) = w_info.upgrade() else {
                                return;
                            };
                            let props = NodeProps::from_dict(info.props());
                            let change = shared.borrow_mut().matcher.node(id, props);
                            apply(&shared, change, &core_info, &buffer_info);
                        })
                        .register();
                    shared
                        .borrow_mut()
                        .bound
                        .insert(obj.id, Bound::Node(node, listener));
                }
                ObjectType::Client => {
                    let binary = props
                        .and_then(|p| p.get("application.process.binary"))
                        .map(str::to_owned);
                    let change = shared.borrow_mut().matcher.client(obj.id, binary);
                    apply(&shared, change, &core_global, &buffer_global);
                    let client: Client = match registry.bind(obj) {
                        Ok(client) => client,
                        Err(e) => return log::debug!("cannot bind client {}: {e}", obj.id),
                    };
                    let (id, w_info, core_info, buffer_info) = (
                        obj.id,
                        Rc::downgrade(&shared),
                        core_global.clone(),
                        buffer_global.clone(),
                    );
                    let listener = client
                        .add_listener_local()
                        .info(move |info| {
                            let Some(shared) = w_info.upgrade() else {
                                return;
                            };
                            let binary = info
                                .props()
                                .and_then(|p| p.get("application.process.binary"))
                                .map(str::to_owned);
                            let change = shared.borrow_mut().matcher.client(id, binary);
                            apply(&shared, change, &core_info, &buffer_info);
                        })
                        .register();
                    shared
                        .borrow_mut()
                        .bound
                        .insert(obj.id, Bound::Client(client, listener));
                }
                _ => {}
            }
        })
        .global_remove(move |id| {
            let Some(shared) = w_remove.upgrade() else {
                return;
            };
            let (changes, unbound) = {
                let mut s = shared.borrow_mut();
                (s.matcher.removed(id), s.bound.remove(&id))
            };
            drop(unbound);
            for change in changes {
                apply(&shared, Some(change), &core_remove, &buffer_remove);
            }
        })
        .register();

    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(quit_tx));
    }
    mainloop.run();

    // Disconnect the capture before the core goes away, then free everything else.
    let stream = shared.borrow_mut().stream.take();
    if let Some((stream, _listener)) = stream {
        let _ = stream.disconnect();
    }
    drop(shared);
    Ok(())
}

/// Acts on a matcher change: (re)links the capture stream or drops it.
fn apply(
    shared: &Rc<RefCell<Shared>>,
    change: Option<Change>,
    core: &pw::core::CoreRc,
    buffer: &Arc<Mutex<DelayLine>>,
) {
    let Some(change) = change else { return };
    // Taken out first so it is dropped outside the borrow.
    let old = shared.borrow_mut().stream.take();
    if let Some((stream, _listener)) = old {
        let _ = stream.disconnect();
    }
    lock(buffer).clear();
    match change {
        Change::Appeared { serial } => match open_stream(core, &serial, buffer.clone()) {
            Ok(stream) => {
                log::info!("capturing PipeWire stream (serial {serial})");
                let mut s = shared.borrow_mut();
                s.stream = Some(stream);
                s.set_status(AudioStatus::Active);
            }
            Err(e) => {
                // Nothing is captured; if the stream is re-created, the next match retries.
                log::warn!("cannot capture PipeWire stream {serial}: {e}");
                shared.borrow_mut().set_status(AudioStatus::Waiting);
            }
        },
        Change::Gone => {
            log::info!("PipeWire stream gone");
            shared.borrow_mut().set_status(AudioStatus::Lost);
        }
    }
}

fn open_stream(
    core: &pw::core::CoreRc,
    serial: &str,
    buffer: Arc<Mutex<DelayLine>>,
) -> Result<(StreamRc, StreamListener<()>), pw::Error> {
    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => NODE_NAME,
        *pw::keys::TARGET_OBJECT => serial,
        *pw::keys::NODE_DONT_RECONNECT => "true",
    };
    let stream = StreamRc::new(core.clone(), NODE_NAME, props)?;
    let buffer_format = buffer.clone();
    let mut scratch: Vec<f32> = Vec::new();
    let listener = stream
        .add_local_listener::<()>()
        .state_changed(|_, _, _, new| {
            if let StreamState::Error(e) = new {
                log::warn!("capture stream error: {e}");
            }
        })
        .param_changed(move |_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw()
                || !matches!(
                    format_utils::parse_format(param),
                    Ok((MediaType::Audio, MediaSubtype::Raw))
                )
            {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                log::debug!(
                    "capture format: {:?}, {} Hz, {} channels",
                    info.format(),
                    info.rate(),
                    info.channels()
                );
                let mut line = lock(&buffer_format);
                let delay = line.delay;
                line.reset(capacity(info.rate().max(1), delay));
            }
        })
        .process(move |stream, _| {
            let now = Instant::now();
            let Some(mut pw_buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = pw_buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = data.chunk().offset() as usize;
            let size = data.chunk().size() as usize;
            let Some(bytes) = data.data() else { return };
            let end = offset.saturating_add(size).min(bytes.len());
            let bytes = &bytes[offset.min(end)..end];
            scratch.clear();
            let (samples, _) = bytes.as_chunks::<4>();
            scratch.extend(samples.iter().map(|&b| f32::from_le_bytes(b)));
            // Keep whole frames so the channels never swap.
            scratch.truncate(scratch.len() - scratch.len() % CHANNELS as usize);
            lock(&buffer).push(now, &scratch);
        })
        .register()?;

    // F32LE stereo; no rate, so PipeWire keeps the stream's own and does not resample.
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_channels(CHANNELS);
    let format = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .map_err(|_| pw::Error::CreationFailed)
    .map(|(cursor, _)| cursor.into_inner())?;
    let pod = Pod::from_bytes(&format).ok_or(pw::Error::CreationFailed)?;
    stream.connect(
        spa::utils::Direction::Input,
        None,
        StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
        &mut [pod],
    )?;
    Ok((stream, listener))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn read_at(line: &mut DelayLine, now: Instant) -> Vec<f32> {
        let mut out = vec![99.0]; // read replaces, never appends
        line.read(now, &mut out);
        out
    }

    #[test]
    fn delay_zero_passes_through() {
        let t = Instant::now();
        let mut line = DelayLine::new(Duration::ZERO, 16);
        line.push(t, &[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(read_at(&mut line, t), [0.1, 0.2, 0.3, 0.4]);
        assert!(read_at(&mut line, t).is_empty());
    }

    #[test]
    fn delay_holds_samples_for_n_ms() {
        let t = Instant::now();
        let mut line = DelayLine::new(ms(100), 16);
        line.push(t, &[1.0, 1.0]);
        line.push(t + ms(40), &[2.0, 2.0]);
        assert!(read_at(&mut line, t + ms(99)).is_empty());
        assert_eq!(read_at(&mut line, t + ms(100)), [1.0, 1.0]);
        assert!(read_at(&mut line, t + ms(139)).is_empty());
        assert_eq!(read_at(&mut line, t + ms(500)), [2.0, 2.0]);
    }

    #[test]
    fn ring_buffer_drops_oldest_when_full() {
        let t = Instant::now();
        let mut line = DelayLine::new(ms(100), 4);
        line.push(t, &[1.0, 2.0]);
        line.push(t + ms(10), &[3.0, 4.0]);
        line.push(t + ms(20), &[5.0, 6.0]);
        // The first chunk was dropped; the rest keep their own release times.
        assert_eq!(read_at(&mut line, t + ms(110)), [3.0, 4.0]);
        assert_eq!(read_at(&mut line, t + ms(120)), [5.0, 6.0]);
    }

    #[test]
    fn partly_dropped_chunk_keeps_its_tail() {
        let t = Instant::now();
        let mut line = DelayLine::new(Duration::ZERO, 4);
        line.push(t, &[1.0, 2.0, 3.0, 4.0]);
        line.push(t, &[5.0, 6.0]);
        assert_eq!(read_at(&mut line, t), [3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn chunk_larger_than_capacity_keeps_newest_samples() {
        let t = Instant::now();
        let mut line = DelayLine::new(Duration::ZERO, 2);
        line.push(t, &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(read_at(&mut line, t), [3.0, 4.0]);
    }

    #[test]
    fn read_returns_empty_when_no_data() {
        let mut line = DelayLine::new(ms(50), 8);
        assert!(read_at(&mut line, Instant::now() + ms(1000)).is_empty());
    }

    #[test]
    fn capacity_covers_delay_plus_headroom_in_whole_frames() {
        assert_eq!(capacity(44_100, Duration::ZERO), 22_050 * 2);
        assert_eq!(capacity(48_000, ms(2000)), 120_000 * 2);
    }

    // ------------------------------------------------------------------ matcher

    fn node(class: &str, serial: &str, client: Option<u32>, binary: Option<&str>) -> NodeProps {
        NodeProps {
            class: Some(class.to_owned()),
            serial: Some(serial.to_owned()),
            client,
            binary: binary.map(str::to_owned),
        }
    }

    fn appeared(serial: &str) -> Option<Change> {
        Some(Change::Appeared {
            serial: serial.to_owned(),
        })
    }

    fn matcher() -> Matcher {
        Matcher::new(&StreamMatch::soloist())
    }

    #[test]
    fn matches_by_node_binary() {
        let mut m = matcher();
        let n = node(OUTPUT_STREAM_CLASS, "458", None, Some("soloist"));
        assert_eq!(m.node(76, n), appeared("458"));
    }

    #[test]
    fn matches_by_client_binary_when_node_has_none() {
        // Soloist's case: only the client carries the binary, and its info may come later.
        let mut m = matcher();
        assert_eq!(m.client(40, None), None);
        let n = node(OUTPUT_STREAM_CLASS, "458", Some(40), None);
        assert_eq!(m.node(76, n), None);
        assert_eq!(m.client(40, Some("soloist".to_owned())), appeared("458"));
    }

    #[test]
    fn ignores_other_binaries_and_classes() {
        let mut m = matcher();
        m.client(40, Some("soloist".to_owned()));
        m.client(41, Some("zen".to_owned()));
        assert_eq!(
            m.node(1, node(OUTPUT_STREAM_CLASS, "1", Some(41), None)),
            None
        );
        assert_eq!(
            m.node(2, node("Stream/Input/Audio", "2", Some(40), None)),
            None
        );
        assert_eq!(m.node(3, node("Audio/Sink", "3", Some(40), None)), None);
        assert!(!m.is_matched());
    }

    #[test]
    fn waits_for_class_and_serial() {
        let mut m = matcher();
        let partial = NodeProps {
            binary: Some("soloist".to_owned()),
            ..NodeProps::default()
        };
        assert_eq!(m.node(76, partial), None);
        let class = NodeProps {
            class: Some(OUTPUT_STREAM_CLASS.to_owned()),
            ..NodeProps::default()
        };
        assert_eq!(m.node(76, class), None);
        let serial = NodeProps {
            serial: Some("458".to_owned()),
            ..NodeProps::default()
        };
        assert_eq!(m.node(76, serial), appeared("458"));
    }

    #[test]
    fn reports_once_and_ignores_unrelated_removals() {
        let mut m = matcher();
        let n = node(OUTPUT_STREAM_CLASS, "458", None, Some("soloist"));
        assert_eq!(m.node(76, n.clone()), appeared("458"));
        assert_eq!(m.node(76, n), None);
        assert!(m.removed(99).is_empty());
        assert!(m.is_matched());
    }

    #[test]
    fn removal_reports_gone_and_reappearance_uses_new_serial() {
        // Observed live: switching device away and back reused node id 76 with a new serial.
        let mut m = matcher();
        m.client(40, Some("soloist".to_owned()));
        m.node(76, node(OUTPUT_STREAM_CLASS, "458", Some(40), None));
        assert_eq!(m.removed(76), [Change::Gone]);
        assert!(!m.is_matched());
        let back = node(OUTPUT_STREAM_CLASS, "487", Some(40), None);
        assert_eq!(m.node(76, back), appeared("487"));
    }

    #[test]
    fn removal_switches_to_another_matching_stream() {
        let mut m = matcher();
        m.node(5, node(OUTPUT_STREAM_CLASS, "10", None, Some("soloist")));
        m.node(6, node(OUTPUT_STREAM_CLASS, "11", None, Some("soloist")));
        assert_eq!(
            m.removed(5),
            [
                Change::Gone,
                Change::Appeared {
                    serial: "11".to_owned()
                }
            ]
        );
    }
}
