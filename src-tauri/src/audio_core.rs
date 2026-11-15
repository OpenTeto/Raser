use crate::storage;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use rodio::decoder::Decoder;
use rodio::source::Source;
use rodio::stream::{DeviceSinkBuilder, MixerDeviceSink};
use rodio::Player;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

const FFT_SAMPLE_COUNT: usize = 512;
const FFT_BIN_COUNT: usize = 64;
const PROGRESSIVE_PREBUFFER_BYTES: usize = 192 * 1024;

struct ProgressiveBuffer {
    data: Vec<u8>,
    finished: bool,
    error: Option<String>,
    total_len: Option<u64>,
}

struct SharedProgressive {
    state: Mutex<ProgressiveBuffer>,
    condvar: Condvar,
    load_gen: Arc<AtomicU64>,
}

struct ProgressiveReader {
    shared: Arc<SharedProgressive>,
    pos: u64,
    expected_gen: u64,
}

impl ProgressiveReader {
    fn new(shared: Arc<SharedProgressive>, expected_gen: u64) -> Self {
        Self {
            shared,
            pos: 0,
            expected_gen,
        }
    }
}

impl Read for ProgressiveReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.state.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "Progressive buffer lock poisoned")
        })?;

        loop {
            if self.shared.load_gen.load(Ordering::SeqCst) != self.expected_gen {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Superseded by newer load"));
            }

            let available = state.data.len() as u64;
            if self.pos < available {
                let start = self.pos as usize;
                let to_copy = (state.data.len() - start).min(buf.len());
                buf[..to_copy].copy_from_slice(&state.data[start..start + to_copy]);
                self.pos += to_copy as u64;
                return Ok(to_copy);
            }

            if state.finished {
                return Ok(0);
            }

            if let Some(ref err) = state.error {
                return Err(io::Error::new(io::ErrorKind::Other, err.clone()));
            }

            let (guard, _) = self
                .shared
                .condvar
                .wait_timeout(state, Duration::from_millis(100))
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::Other, "Progressive buffer condvar error")
                })?;
            state = guard;
        }
    }
}

impl Seek for ProgressiveReader {
    fn seek(&mut self, style: SeekFrom) -> io::Result<u64> {
        let mut state = self.shared.state.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "Progressive buffer lock poisoned")
        })?;

        let new_pos: i64 = match style {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => (self.pos as i64).saturating_add(offset),
            SeekFrom::End(offset) => {
                loop {
                    if self.shared.load_gen.load(Ordering::SeqCst) != self.expected_gen {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "Superseded by newer load"));
                    }
                    if let Some(total) = state.total_len {
                        break (total as i64).saturating_add(offset);
                    }
                    if state.finished {
                        break (state.data.len() as i64).saturating_add(offset);
                    }
                    if let Some(ref err) = state.error {
                        return Err(io::Error::new(io::ErrorKind::Other, err.clone()));
                    }
                    let (guard, _) = self
                        .shared
                        .condvar
                        .wait_timeout(state, Duration::from_millis(100))
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::Other, "Progressive seek condvar error")
                        })?;
                    state = guard;
                }
            }
        };

        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid seek to negative position",
            ));
        }

        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

fn extract_track_id_from_url(url: &str) -> Option<String> {
    if let Some(pos) = url.find('?') {
        let query = &url[pos + 1..];
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                if k == "id" || k == "trackId" || k == "v" {
                    let decoded = urlencoding::decode(v).unwrap_or(std::borrow::Cow::Borrowed(v));
                    let trimmed = decoded.trim();
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }
    None
}

// Lock-free wrapper around a rodio Source that downmixes channels to mono
// and pushes real-time PCM samples into an rtrb lock-free ring buffer for FFT analysis
pub struct VisualizerSource<S> {
    inner: S,
    channels: usize,
    sample_accum: f32,
    channel_idx: usize,
    producer: rtrb::Producer<f32>,
}

impl<S> Iterator for VisualizerSource<S>
where
    S: Source<Item = rodio::Sample>,
{
    type Item = rodio::Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        let sample_f32 = sample as f32;
        self.sample_accum += sample_f32;
        self.channel_idx += 1;
        if self.channel_idx >= self.channels {
            let mono_sample = self.sample_accum / (self.channels as f32);
            let _ = self.producer.push(mono_sample);
            self.sample_accum = 0.0;
            self.channel_idx = 0;
        }
        Some(sample)
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S> Source for VisualizerSource<S>
where
    S: Source<Item = rodio::Sample>,
{
    #[inline]
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    #[inline]
    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    #[inline]
    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    #[inline]
    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    #[inline]
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioState {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f32,
    pub speed: f32,
    pub loading: bool,
    pub ended: bool,
    pub error: Option<String>,
    pub url: Option<String>,
}

pub struct AudioCore {
    sink: Mutex<Option<MixerDeviceSink>>,
    player: Mutex<Option<Player>>,
    consumer: Mutex<Option<rtrb::Consumer<f32>>>,
    recent_samples: Mutex<Vec<f32>>,
    fft_plan: Arc<dyn Fft<f32>>,
    hann_window: Vec<f32>,
    cached_bytes: Arc<Mutex<Option<Arc<Vec<u8>>>>>,
    current_url: Arc<Mutex<Option<String>>>,
    current_duration: Arc<Mutex<f64>>,
    current_error: Arc<Mutex<Option<String>>>,
    is_playing: Arc<AtomicBool>,
    is_loading: Arc<AtomicBool>,
    has_ended: Arc<AtomicBool>,
    track_has_played: Arc<AtomicBool>,
    volume: Arc<Mutex<f32>>,
    speed: Arc<Mutex<f32>>,
    load_generation: Arc<AtomicU64>,
}

impl AudioCore {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft_plan = planner.plan_fft_forward(FFT_SAMPLE_COUNT);

        let mut hann_window = Vec::with_capacity(FFT_SAMPLE_COUNT);
        for i in 0..FFT_SAMPLE_COUNT {
            let mult = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SAMPLE_COUNT - 1) as f32).cos());
            hann_window.push(mult);
        }

        Self {
            sink: Mutex::new(None),
            player: Mutex::new(None),
            consumer: Mutex::new(None),
            recent_samples: Mutex::new(vec![0.0f32; FFT_SAMPLE_COUNT]),
            fft_plan,
            hann_window,
            cached_bytes: Arc::new(Mutex::new(None)),
            current_url: Arc::new(Mutex::new(None)),
            current_duration: Arc::new(Mutex::new(0.0)),
            current_error: Arc::new(Mutex::new(None)),
            is_playing: Arc::new(AtomicBool::new(false)),
            is_loading: Arc::new(AtomicBool::new(false)),
            has_ended: Arc::new(AtomicBool::new(false)),
            track_has_played: Arc::new(AtomicBool::new(false)),
            volume: Arc::new(Mutex::new(0.85)),
            speed: Arc::new(Mutex::new(1.0)),
            load_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    fn ensure_player(&self) -> Result<(), String> {
        let mut player_lock = self.player.lock().map_err(|e| e.to_string())?;
        if player_lock.is_some() {
            return Ok(());
        }

        let mut sink_lock = self.sink.lock().map_err(|e| e.to_string())?;
        if sink_lock.is_none() {
            let sink = DeviceSinkBuilder::open_default_sink()
                .map_err(|e| format!("Failed to open default audio sink: {:?}", e))?;
            *sink_lock = Some(sink);
        }

        let sink = sink_lock.as_ref().unwrap();
        let player = Player::connect_new(sink.mixer());
        let vol = *self.volume.lock().map_err(|e| e.to_string())?;
        let spd = *self.speed.lock().map_err(|e| e.to_string())?;
        player.set_volume(vol);
        player.set_speed(spd);
        *player_lock = Some(player);

        Ok(())
    }

    pub fn play(&self) -> Result<(), String> {
        self.ensure_player()?;
        self.is_playing.store(true, Ordering::SeqCst);
        self.has_ended.store(false, Ordering::SeqCst);
        if let Some(player) = self.player.lock().map_err(|e| e.to_string())?.as_ref() {
            player.play();
        }
        Ok(())
    }

    pub fn pause(&self) -> Result<(), String> {
        self.is_playing.store(false, Ordering::SeqCst);
        if let Some(player) = self.player.lock().map_err(|e| e.to_string())?.as_ref() {
            player.pause();
        }
        Ok(())
    }

    pub fn toggle(&self) -> Result<bool, String> {
        if self.is_playing.load(Ordering::SeqCst) {
            self.pause()?;
            Ok(false)
        } else {
            self.play()?;
            Ok(true)
        }
    }

    pub fn set_volume(&self, vol: f32) -> Result<(), String> {
        let clamped = vol.clamp(0.0, 2.0);
        *self.volume.lock().map_err(|e| e.to_string())? = clamped;
        if let Some(player) = self.player.lock().map_err(|e| e.to_string())?.as_ref() {
            player.set_volume(clamped);
        }
        Ok(())
    }

    pub fn set_speed(&self, spd: f32) -> Result<(), String> {
        let clamped = spd.clamp(0.25, 4.0);
        *self.speed.lock().map_err(|e| e.to_string())? = clamped;
        if let Some(player) = self.player.lock().map_err(|e| e.to_string())?.as_ref() {
            player.set_speed(clamped);
        }
        Ok(())
    }

    pub fn seek(&self, position_secs: f64) -> Result<(), String> {
        self.ensure_player()?;
        let target_pos = Duration::from_secs_f64(position_secs.max(0.0));
        let player_guard = self.player.lock().map_err(|e| e.to_string())?;
        if let Some(player) = player_guard.as_ref() {
            // Attempt fast in-stream seek first
            if player.try_seek(target_pos).is_ok() {
                self.has_ended.store(false, Ordering::SeqCst);
                return Ok(());
            }

            // Fallback: If try_seek fails, reconstruct decoder from cached bytes
            drop(player_guard);
            if let Some(bytes_arc) = self.cached_bytes.lock().map_err(|e| e.to_string())?.clone() {
                let cursor = Cursor::new(bytes_arc.as_ref().clone());
                if let Ok(mut decoder) = Decoder::new(cursor) {
                    let _ = decoder.try_seek(target_pos);
                    let channels = decoder.channels().get() as usize;
                    let (producer, consumer) = rtrb::RingBuffer::new(4096);
                    *self.consumer.lock().unwrap() = Some(consumer);

                    let viz_source = VisualizerSource {
                        inner: decoder,
                        channels: channels.max(1),
                        sample_accum: 0.0,
                        channel_idx: 0,
                        producer,
                    };

                    if let Some(player) = self.player.lock().map_err(|e| e.to_string())?.as_ref() {
                        player.clear();
                        player.append(viz_source);
                        let _ = player.try_seek(target_pos);
                        if self.is_playing.load(Ordering::SeqCst) {
                            player.play();
                        } else {
                            player.pause();
                        }
                        self.has_ended.store(false, Ordering::SeqCst);
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn load_audio_async(
        self: &Arc<Self>,
        url: String,
        start_pos_secs: Option<f64>,
        duration_hint: Option<f64>,
        autoplay: bool,
        track_id: Option<String>,
    ) -> Result<AudioState, String> {
        let current_gen = self.load_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.is_loading.store(true, Ordering::SeqCst);
        self.has_ended.store(false, Ordering::SeqCst);
        *self.current_error.lock().unwrap() = None;
        *self.current_url.lock().unwrap() = Some(url.clone());
        if let Some(dur) = duration_hint {
            *self.current_duration.lock().unwrap() = dur;
        }

        // Immediately stop previous audio so there is no audio bleed while loading
        if let Ok(player_guard) = self.player.lock() {
            if let Some(player) = player_guard.as_ref() {
                player.clear();
                player.pause();
            }
        }
        // Silence visualizer samples immediately
        if let Ok(mut samples) = self.recent_samples.lock() {
            samples.fill(0.0);
        }

        let this = Arc::clone(self);
        let effective_tid = track_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| extract_track_id_from_url(&url));

        tauri::async_runtime::spawn_blocking(move || {
            let mut cached_bytes_opt = None;
            if let Some(ref tid) = effective_tid {
                if let Some(cached_path) = storage::get_cached_audio_path(tid) {
                    if let Ok(b) = std::fs::read(&cached_path) {
                        log::info!("AudioCore: Loaded track {} from disk cache {:?}", tid, cached_path);
                        cached_bytes_opt = Some(b);
                    }
                }
            }

            enum LoadedDecoder {
                Buffered(Decoder<Cursor<Vec<u8>>>),
                Progressive(Decoder<ProgressiveReader>),
            }

            let loaded_decoder = if cached_bytes_opt.is_none()
                && (url.starts_with("http://") || url.starts_with("https://"))
            {
                let client = match reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
                    .build()
                {
                    Ok(c) => c,
                    Err(e) => return Err(format!("Failed to create HTTP client: {:?}", e)),
                };

                let resp = match client.get(&url).send() {
                    Ok(r) => r,
                    Err(e) => {
                        let err_msg = format!("Failed to fetch stream URL: {:?}", e);
                        log::error!("Audio load failed for {}: {}", url, err_msg);
                        *this.current_error.lock().unwrap() = Some(err_msg.clone());
                        this.is_loading.store(false, Ordering::SeqCst);
                        this.is_playing.store(false, Ordering::SeqCst);
                        return Err(err_msg);
                    }
                };

                if !resp.status().is_success() {
                    let err_msg = format!(
                        "HTTP error {}: {}",
                        resp.status().as_u16(),
                        resp.status().canonical_reason().unwrap_or("Failed")
                    );
                    log::error!("Audio load failed for {}: {}", url, err_msg);
                    *this.current_error.lock().unwrap() = Some(err_msg.clone());
                    this.is_loading.store(false, Ordering::SeqCst);
                    this.is_playing.store(false, Ordering::SeqCst);
                    return Err(err_msg);
                }

                if this.load_generation.load(Ordering::SeqCst) != current_gen {
                    return Err("Superseded by newer load".to_string());
                }

                *this.cached_bytes.lock().unwrap() = None;

                let total_len = resp.content_length();
                let initial_cap = total_len.map(|l| l as usize).unwrap_or(3 * 1024 * 1024);
                let shared = Arc::new(SharedProgressive {
                    state: Mutex::new(ProgressiveBuffer {
                        data: Vec::with_capacity(initial_cap),
                        finished: false,
                        error: None,
                        total_len,
                    }),
                    condvar: Condvar::new(),
                    load_gen: Arc::clone(&this.load_generation),
                });

                let shared_dl = Arc::clone(&shared);
                let this_dl = Arc::clone(&this);
                let tid_dl = effective_tid.clone();
                std::thread::spawn(move || {
                    let mut stream_resp = resp;
                    let mut chunk = [0u8; 32768];
                    loop {
                        if this_dl.load_generation.load(Ordering::SeqCst) != current_gen {
                            shared_dl.condvar.notify_all();
                            return;
                        }
                        match stream_resp.read(&mut chunk) {
                            Ok(0) => {
                                let completed_bytes = {
                                    let mut st = shared_dl.state.lock().unwrap();
                                    st.finished = true;
                                    if st.data.is_empty() {
                                        st.error = Some("Empty stream response".to_string());
                                        None
                                    } else {
                                        Some(st.data.clone())
                                    }
                                };
                                shared_dl.condvar.notify_all();

                                if let Some(full_bytes) = completed_bytes {
                                    if this_dl.load_generation.load(Ordering::SeqCst) == current_gen {
                                        let bytes_arc = Arc::new(full_bytes);
                                        *this_dl.cached_bytes.lock().unwrap() = Some(Arc::clone(&bytes_arc));
                                        if let Some(ref tid) = tid_dl {
                                            storage::save_cached_audio(tid, &bytes_arc);
                                        }
                                    }
                                }
                                return;
                            }
                            Ok(n) => {
                                {
                                    let mut st = shared_dl.state.lock().unwrap();
                                    st.data.extend_from_slice(&chunk[..n]);
                                }
                                shared_dl.condvar.notify_all();
                            }
                            Err(e) => {
                                {
                                    let mut st = shared_dl.state.lock().unwrap();
                                    st.error = Some(format!("Failed to read stream response bytes: {:?}", e));
                                    st.finished = true;
                                }
                                shared_dl.condvar.notify_all();
                                return;
                            }
                        }
                    }
                });

                // Wait only until prebuffer (192 KB) or EOF arrives so playback starts in <1s
                {
                    let mut st = shared.state.lock().unwrap();
                    while st.data.len() < PROGRESSIVE_PREBUFFER_BYTES
                        && !st.finished
                        && st.error.is_none()
                    {
                        if this.load_generation.load(Ordering::SeqCst) != current_gen {
                            return Err("Superseded by newer load".to_string());
                        }
                        let (guard, _) = shared
                            .condvar
                            .wait_timeout(st, Duration::from_millis(100))
                            .unwrap();
                        st = guard;
                    }
                    if this.load_generation.load(Ordering::SeqCst) != current_gen {
                        return Err("Superseded by newer load".to_string());
                    }
                    if st.data.is_empty() {
                        let err_msg = st
                            .error
                            .clone()
                            .unwrap_or_else(|| "Empty stream response".to_string());
                        log::error!("Audio load failed for {}: {}", url, err_msg);
                        *this.current_error.lock().unwrap() = Some(err_msg.clone());
                        this.is_loading.store(false, Ordering::SeqCst);
                        this.is_playing.store(false, Ordering::SeqCst);
                        return Err(err_msg);
                    }
                }

                if let Err(e) = this.ensure_player() {
                    log::error!("Error ensuring audio player: {:?}", e);
                    *this.current_error.lock().unwrap() = Some(e.clone());
                    this.is_loading.store(false, Ordering::SeqCst);
                    return Err(e);
                }

                let reader = ProgressiveReader::new(Arc::clone(&shared), current_gen);
                let decoder = match Decoder::new(reader) {
                    Ok(d) => d,
                    Err(e) => {
                        let err_msg = format!("Audio decoder error for {}: {:?}", url, e);
                        log::error!("{}", err_msg);
                        *this.current_error.lock().unwrap() = Some(err_msg.clone());
                        this.is_loading.store(false, Ordering::SeqCst);
                        return Err(err_msg);
                    }
                };
                LoadedDecoder::Progressive(decoder)
            } else {
                let bytes_res = if let Some(b) = cached_bytes_opt {
                    Ok(b)
                } else {
                    let mut path_str = if let Some(stripped) = url.strip_prefix("file:///") {
                        stripped.replace('/', "\\")
                    } else if let Some(stripped) = url.strip_prefix("file://") {
                        stripped.replace('/', "\\")
                    } else {
                        url.clone()
                    };
                    if let Ok(decoded) = urlencoding::decode(&path_str) {
                        path_str = decoded.into_owned();
                    }
                    match std::fs::read(&path_str) {
                        Ok(b) => Ok(b),
                        Err(e) => Err(format!("Failed to read local file '{}': {:?}", path_str, e)),
                    }
                };

                if this.load_generation.load(Ordering::SeqCst) != current_gen {
                    return Err("Superseded by newer load".to_string());
                }

                let bytes = match bytes_res {
                    Ok(b) => b,
                    Err(err_msg) => {
                        log::error!("Audio load failed for {}: {}", url, err_msg);
                        *this.current_error.lock().unwrap() = Some(err_msg.clone());
                        this.is_loading.store(false, Ordering::SeqCst);
                        this.is_playing.store(false, Ordering::SeqCst);
                        return Err(err_msg);
                    }
                };

                let bytes_arc = Arc::new(bytes);
                *this.cached_bytes.lock().unwrap() = Some(Arc::clone(&bytes_arc));

                if let Err(e) = this.ensure_player() {
                    log::error!("Error ensuring audio player: {:?}", e);
                    *this.current_error.lock().unwrap() = Some(e.clone());
                    this.is_loading.store(false, Ordering::SeqCst);
                    return Err(e);
                }

                let cursor = Cursor::new(bytes_arc.as_ref().clone());
                let decoder = match Decoder::new(cursor) {
                    Ok(d) => d,
                    Err(e) => {
                        let err_msg = format!("Audio decoder error for {}: {:?}", url, e);
                        log::error!("{}", err_msg);
                        *this.current_error.lock().unwrap() = Some(err_msg.clone());
                        this.is_loading.store(false, Ordering::SeqCst);
                        return Err(err_msg);
                    }
                };
                LoadedDecoder::Buffered(decoder)
            };

            let (total_dur, channels) = match &loaded_decoder {
                LoadedDecoder::Buffered(d) => (d.total_duration(), d.channels().get() as usize),
                LoadedDecoder::Progressive(d) => (d.total_duration(), d.channels().get() as usize),
            };

            if let Some(total) = total_dur {
                *this.current_duration.lock().unwrap() = total.as_secs_f64();
            } else if let Some(dur) = duration_hint {
                *this.current_duration.lock().unwrap() = dur;
            }

            let (producer, consumer) = rtrb::RingBuffer::new(4096);
            *this.consumer.lock().unwrap() = Some(consumer);

            if let Ok(player_guard) = this.player.lock() {
                if let Some(player) = player_guard.as_ref() {
                    player.clear();
                    match loaded_decoder {
                        LoadedDecoder::Buffered(decoder) => {
                            player.append(VisualizerSource {
                                inner: decoder,
                                channels: channels.max(1),
                                sample_accum: 0.0,
                                channel_idx: 0,
                                producer,
                            });
                        }
                        LoadedDecoder::Progressive(decoder) => {
                            player.append(VisualizerSource {
                                inner: decoder,
                                channels: channels.max(1),
                                sample_accum: 0.0,
                                channel_idx: 0,
                                producer,
                            });
                        }
                    }

                    if let Some(start_sec) = start_pos_secs {
                        if start_sec > 0.0 {
                            let _ = player.try_seek(Duration::from_secs_f64(start_sec));
                        }
                    }

                    let vol = *this.volume.lock().unwrap();
                    let spd = *this.speed.lock().unwrap();
                    player.set_volume(vol);
                    player.set_speed(spd);

                    let should_play = autoplay || this.is_playing.load(Ordering::SeqCst);
                    if should_play {
                        player.play();
                        this.is_playing.store(true, Ordering::SeqCst);
                    } else {
                        player.pause();
                        this.is_playing.store(false, Ordering::SeqCst);
                    }
                }
            }

            this.track_has_played.store(false, Ordering::SeqCst);
            this.has_ended.store(false, Ordering::SeqCst);
            this.is_loading.store(false, Ordering::SeqCst);

            Ok(this.get_state())
        })
        .await
        .map_err(|e| format!("Task execution error: {:?}", e))?
    }

    pub fn get_state(&self) -> AudioState {
        let (pos, empty) = if let Ok(player_guard) = self.player.lock() {
            if let Some(player) = player_guard.as_ref() {
                (player.get_pos().as_secs_f64(), player.empty())
            } else {
                (0.0, true)
            }
        } else {
            (0.0, true)
        };

        if pos > 0.1 {
            self.track_has_played.store(true, Ordering::SeqCst);
        }

        let duration = *self.current_duration.lock().unwrap_or_else(|e| e.into_inner());
        let loading = self.is_loading.load(Ordering::SeqCst);
        let playing = self.is_playing.load(Ordering::SeqCst);
        let vol = *self.volume.lock().unwrap_or_else(|e| e.into_inner());
        let spd = *self.speed.lock().unwrap_or_else(|e| e.into_inner());
        let url = self.current_url.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let error = self.current_error.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let track_played = self.track_has_played.load(Ordering::SeqCst);
        let ended = if !loading && empty && track_played && (duration <= 0.0 || pos >= (duration - 1.5).max(0.0)) {
            self.has_ended.store(true, Ordering::SeqCst);
            self.is_playing.store(false, Ordering::SeqCst);
            true
        } else {
            self.has_ended.load(Ordering::SeqCst)
        };

        AudioState {
            playing: if ended { false } else { playing },
            position: pos,
            duration,
            volume: vol,
            speed: spd,
            loading,
            ended,
            error,
            url,
        }
    }

    pub fn get_frequency_data(&self) -> Vec<u8> {
        let is_playing = self.is_playing.load(Ordering::SeqCst);
        if !is_playing {
            return vec![0u8; FFT_BIN_COUNT];
        }

        let mut samples_guard = match self.recent_samples.lock() {
            Ok(g) => g,
            Err(_) => return vec![0u8; FFT_BIN_COUNT],
        };

        // Drain fresh mono samples from lock-free consumer into history buffer
        if let Ok(mut cons_guard) = self.consumer.lock() {
            if let Some(cons) = cons_guard.as_mut() {
                while let Ok(sample) = cons.pop() {
                    samples_guard.push(sample);
                }
                if samples_guard.len() > FFT_SAMPLE_COUNT {
                    let excess = samples_guard.len() - FFT_SAMPLE_COUNT;
                    samples_guard.drain(0..excess);
                }
            }
        }

        if samples_guard.len() < FFT_SAMPLE_COUNT {
            let needed = FFT_SAMPLE_COUNT - samples_guard.len();
            samples_guard.splice(0..0, std::iter::repeat(0.0f32).take(needed));
        }

        // Apply precomputed Hann window
        let mut complex_buf: Vec<Complex<f32>> = samples_guard
            .iter()
            .zip(self.hann_window.iter())
            .map(|(&s, &w)| Complex {
                re: s * w,
                im: 0.0,
            })
            .collect();

        // Process forward FFT with cached plan
        self.fft_plan.process(&mut complex_buf);

        // Aggregate into 64 frequency bins
        let mut bins = vec![0u8; FFT_BIN_COUNT];
        let useful_fft_bins = FFT_SAMPLE_COUNT / 2; // 256
        let samples_per_bin = (useful_fft_bins / FFT_BIN_COUNT).max(1);

        for i in 0..FFT_BIN_COUNT {
            let mut sum_mag = 0.0f32;
            let start = i * samples_per_bin;
            let end = (start + samples_per_bin).min(useful_fft_bins);
            for k in start..end {
                let c = complex_buf[k];
                sum_mag += (c.re * c.re + c.im * c.im).sqrt();
            }
            let avg_mag = sum_mag / (end - start).max(1) as f32;
            let scaled = ((avg_mag * 35.0).log10() * 120.0).clamp(0.0, 255.0);
            bins[i] = if scaled.is_finite() && scaled > 0.0 {
                scaled as u8
            } else {
                0
            };
        }

        bins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_core_initial_state() {
        let core = AudioCore::new();
        let state = core.get_state();
        assert_eq!(state.playing, false);
        assert_eq!(state.position, 0.0);
        assert_eq!(state.duration, 0.0);
        assert!((state.volume - 0.85).abs() < 0.001);
        assert!((state.speed - 1.0).abs() < 0.001);
        assert_eq!(state.loading, false);
        assert_eq!(state.ended, false);
        assert_eq!(state.error, None);
    }

    #[test]
    fn test_volume_and_speed_clamping() {
        let core = AudioCore::new();
        assert!(core.set_volume(5.0).is_ok());
        let st1 = core.get_state();
        assert_eq!(st1.volume, 2.0);

        assert!(core.set_volume(-1.0).is_ok());
        let st2 = core.get_state();
        assert_eq!(st2.volume, 0.0);

        assert!(core.set_speed(10.0).is_ok());
        let st3 = core.get_state();
        assert_eq!(st3.speed, 4.0);

        assert!(core.set_speed(0.01).is_ok());
        let st4 = core.get_state();
        assert_eq!(st4.speed, 0.25);
    }

    #[test]
    fn test_frequency_data_bins() {
        let core = AudioCore::new();
        let freq = core.get_frequency_data();
        assert_eq!(freq.len(), FFT_BIN_COUNT);
        // While paused, it should return zeroes
        assert!(freq.iter().all(|&v| v == 0));
    }
}
