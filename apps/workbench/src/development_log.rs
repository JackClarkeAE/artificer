//! Bounded, local-only development session tracing from ADR 0022.
//!
//! The UI path only builds a compact event and calls `try_send`. JSON
//! serialization, batching, rotation, retention, and the crash-tail ring all
//! live on the dedicated writer thread.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Event, Key, Modifiers, PointerButton, Rect, TouchPhase};
use serde::Serialize;
use serde_json::{Value, json};

const EVENT_SCHEMA: u32 = 1;
const QUEUE_CAPACITY: usize = 2_048;
const CRASH_TAIL_CAPACITY: usize = 256;
const WRITER_BATCH_EVENTS: usize = 64;
const WRITER_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const WHEEL_GESTURE_IDLE: Duration = Duration::from_millis(120);
const MAX_SESSION_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SESSION_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_RETAINED_FILES: usize = 10;
const MAX_RETAINED_BYTES: u64 = 64 * 1024 * 1024;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);
static PANIC_HOOK: Once = Once::new();
static PANIC_CONTEXT: OnceLock<Mutex<Option<PanicContext>>> = OnceLock::new();

#[derive(Clone, Debug, Serialize)]
struct EventRecord {
    schema: u32,
    session: String,
    sequence: u64,
    monotonic_ms: u64,
    wall_utc: String,
    thread: &'static str,
    kind: &'static str,
    payload: Value,
}

struct PendingEvent {
    monotonic_ms: u64,
    wall_unix_ms: u64,
    thread: &'static str,
    kind: &'static str,
    payload: Value,
    flush: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UiTraceState {
    pub(crate) workbench: &'static str,
    pub(crate) pending_operation: Option<&'static str>,
    pub(crate) model_tool: String,
    pub(crate) sketch_tool: String,
    pub(crate) history_position: usize,
    pub(crate) snapshot: Option<String>,
    pub(crate) selected_targets: Vec<String>,
    pub(crate) drag_active: bool,
}

struct PanicContext {
    session: String,
    pending_path: PathBuf,
    final_path: PathBuf,
    file: Arc<Mutex<File>>,
    tail: Arc<Mutex<VecDeque<EventRecord>>>,
}

/// An always-on application-session recorder with a non-blocking UI producer.
pub(crate) struct DevelopmentRecorder {
    session: String,
    session_path: PathBuf,
    started: Instant,
    dropped: Arc<AtomicU64>,
    sender: Option<SyncSender<PendingEvent>>,
    writer: Option<JoinHandle<()>>,
    pointer_gestures: [Option<PointerGesture>; 5],
    wheel_gesture: Option<WheelGesture>,
    last_ui_state: Option<UiTraceState>,
    incident_pending_path: PathBuf,
}

#[derive(Clone, Copy)]
struct GesturePosition {
    surface: &'static str,
    x: f64,
    y: f64,
}

struct PointerGesture {
    started: Instant,
    start: GesturePosition,
}

struct WheelGesture {
    started: Instant,
    last_event: Instant,
    delta: egui::Vec2,
    samples: u32,
}

impl DevelopmentRecorder {
    pub(crate) fn start_default() -> io::Result<Self> {
        match Self::start_in(default_log_root()) {
            Ok(recorder) => Ok(recorder),
            Err(primary_error) => Self::start_in(std::env::temp_dir().join("Artificer-Logs"))
                .map_err(|_| primary_error),
        }
    }

    fn start_in(root: PathBuf) -> io::Result<Self> {
        let sessions = root.join("sessions");
        let incidents = root.join("incidents");
        fs::create_dir_all(&sessions)?;
        fs::create_dir_all(&incidents)?;
        prune_sessions(&sessions)?;

        let wall_ms = unix_millis(SystemTime::now());
        let session = format!(
            "{:x}-{:x}-{:x}",
            wall_ms,
            std::process::id(),
            SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let timestamp = filename_timestamp(wall_ms);
        let stem = format!("{timestamp}-{session}");
        let session_path = sessions.join(format!("{stem}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&session_path)?;

        let incident_pending_path = incidents.join(format!("{stem}.pending"));
        let incident_final_path = incidents.join(format!("{stem}.json"));
        let incident_file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&incident_pending_path)?;
        let incident_file = Arc::new(Mutex::new(incident_file));
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(CRASH_TAIL_CAPACITY)));
        install_panic_hook();
        *PANIC_CONTEXT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(PanicContext {
            session: session.clone(),
            pending_path: incident_pending_path.clone(),
            final_path: incident_final_path,
            file: Arc::clone(&incident_file),
            tail: Arc::clone(&tail),
        });

        let dropped = Arc::new(AtomicU64::new(0));
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let writer_session = session.clone();
        let writer_path = session_path.clone();
        let writer_dropped = Arc::clone(&dropped);
        let writer = thread::Builder::new()
            .name("artificer-development-log".to_owned())
            .spawn(move || {
                writer_loop(
                    receiver,
                    file,
                    writer_path,
                    writer_session,
                    writer_dropped,
                    tail,
                );
            })?;

        let recorder = Self {
            session,
            session_path,
            started: Instant::now(),
            dropped,
            sender: Some(sender),
            writer: Some(writer),
            pointer_gestures: Default::default(),
            wheel_gesture: None,
            last_ui_state: None,
            incident_pending_path,
        };
        recorder.log(
            "session.start",
            json!({
                "application": "Artificer",
                "version": env!("CARGO_PKG_VERSION"),
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
                "parallelism": thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get),
                "policy": "local_bounded_privacy_filtered"
            }),
        );
        Ok(recorder)
    }

    pub(crate) fn session_path(&self) -> &Path {
        &self.session_path
    }

    pub(crate) fn log(&self, kind: &'static str, payload: Value) {
        self.enqueue(kind, payload, false);
    }

    pub(crate) fn log_critical(&self, kind: &'static str, payload: Value) {
        self.enqueue(kind, payload, true);
    }

    fn enqueue(&self, kind: &'static str, payload: Value, flush: bool) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let event = PendingEvent {
            monotonic_ms: saturating_millis(self.started.elapsed()),
            wall_unix_ms: unix_millis(SystemTime::now()),
            thread: "ui",
            kind,
            payload,
            flush,
        };
        match sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Captures only privacy-approved physical input. Text, IME, paste
    /// payloads, and unmodified character keys never enter the queue.
    pub(crate) fn capture_egui_input(&mut self, context: &egui::Context, viewport: Rect) {
        let now = Instant::now();
        if self
            .wheel_gesture
            .as_ref()
            .is_some_and(|gesture| now.duration_since(gesture.last_event) >= WHEEL_GESTURE_IDLE)
        {
            self.finish_wheel_gesture("idle");
        }
        let mut finish_wheel = None;
        context.input(|input| {
            for event in &input.raw.events {
                match event {
                    Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        modifiers,
                    } => {
                        let (surface, x, y) = quantized_position(*pos, viewport);
                        let position = GesturePosition { surface, x, y };
                        let slot = button_index(*button);
                        if *pressed {
                            self.pointer_gestures[slot] = Some(PointerGesture {
                                started: now,
                                start: position,
                            });
                            self.log(
                                "input.pointer_gesture_start",
                                json!({
                                    "button": pointer_button_name(*button),
                                    "surface": surface,
                                    "x": x,
                                    "y": y,
                                    "modifiers": modifier_payload(*modifiers)
                                }),
                            );
                        } else {
                            let start = self.pointer_gestures[slot].take();
                            self.log(
                                "input.pointer_gesture_finish",
                                json!({
                                    "button": pointer_button_name(*button),
                                    "start": start.as_ref().map(|gesture| json!({
                                        "surface": gesture.start.surface,
                                        "x": gesture.start.x,
                                        "y": gesture.start.y
                                    })),
                                    "end": {"surface": surface, "x": x, "y": y},
                                    "duration_ms": start.as_ref().map(|gesture| saturating_millis(now.duration_since(gesture.started))),
                                    "modifiers": modifier_payload(*modifiers)
                                }),
                            );
                        }
                    }
                    Event::Key {
                        key,
                        pressed,
                        repeat,
                        modifiers,
                        ..
                    } if key_is_traceable(*key, *modifiers) => {
                        self.log(
                            "input.key",
                            json!({
                                "key": format!("{key:?}"),
                                "pressed": pressed,
                                "repeat": repeat,
                                "modifiers": modifier_payload(*modifiers)
                            }),
                        );
                    }
                    Event::MouseWheel { delta, phase, .. } => {
                        let gesture = self.wheel_gesture.get_or_insert(WheelGesture {
                            started: now,
                            last_event: now,
                            delta: egui::Vec2::ZERO,
                            samples: 0,
                        });
                        gesture.last_event = now;
                        gesture.delta += *delta;
                        gesture.samples = gesture.samples.saturating_add(1);
                        if matches!(phase, TouchPhase::End | TouchPhase::Cancel) {
                            finish_wheel = Some(if *phase == TouchPhase::Cancel {
                                "cancelled"
                            } else {
                                "ended"
                            });
                        }
                    }
                    Event::WindowFocused(focused) => {
                        self.log("input.window_focus", json!({"focused": focused}));
                    }
                    // Deliberately excluded: Text, Ime, Paste, Copy, Cut,
                    // unmodified character keys, raw idle mouse motion, and
                    // touch pressure/device identity.
                    _ => {}
                }
            }
        });
        if let Some(reason) = finish_wheel {
            self.finish_wheel_gesture(reason);
        }
    }

    fn finish_wheel_gesture(&mut self, reason: &'static str) {
        let Some(gesture) = self.wheel_gesture.take() else {
            return;
        };
        self.log(
            "input.wheel_gesture",
            json!({
                "x": quantize_scalar(f64::from(gesture.delta.x), 0.5),
                "y": quantize_scalar(f64::from(gesture.delta.y), 0.5),
                "samples_coalesced": gesture.samples,
                "duration_ms": saturating_millis(gesture.last_event.duration_since(gesture.started)),
                "finish": reason
            }),
        );
    }

    pub(crate) fn observe_ui_state(&mut self, state: UiTraceState) {
        if self.last_ui_state.as_ref() == Some(&state) {
            return;
        }
        self.log(
            "ui.state",
            json!({
                "workbench": state.workbench,
                "pending_operation": state.pending_operation,
                "model_tool": state.model_tool,
                "sketch_tool": state.sketch_tool,
                "history_position": state.history_position,
                "snapshot": state.snapshot,
                "selected_targets": state.selected_targets,
                "drag_active": state.drag_active
            }),
        );
        self.last_ui_state = Some(state);
    }
}

impl Drop for DevelopmentRecorder {
    fn drop(&mut self) {
        self.finish_wheel_gesture("session_end");
        // Dropping the sender is the reliable shutdown signal even if the
        // bounded queue is full. The writer drains every accepted record,
        // appends session.finish, and flushes before the join returns.
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        if let Some(global) = PANIC_CONTEXT.get() {
            let mut guard = global
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard
                .as_ref()
                .is_some_and(|context| context.session == self.session)
            {
                *guard = None;
            }
        }
        let _ = fs::remove_file(&self.incident_pending_path);
    }
}

fn writer_loop(
    receiver: Receiver<PendingEvent>,
    file: File,
    session_path: PathBuf,
    session: String,
    dropped: Arc<AtomicU64>,
    tail: Arc<Mutex<VecDeque<EventRecord>>>,
) {
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let session_started = Instant::now();
    let mut bytes_written = 0_u64;
    let mut file_started = Instant::now();
    let mut part = 1_u32;
    let mut unflushed = 0_usize;
    let mut last_flush = Instant::now();
    let mut last_sequence = 0_u64;

    loop {
        match receiver.recv_timeout(WRITER_FLUSH_INTERVAL) {
            Ok(event) => {
                last_sequence = last_sequence.saturating_add(1);
                if bytes_written >= MAX_SESSION_BYTES || file_started.elapsed() >= MAX_SESSION_AGE {
                    if writer.flush().is_err() {
                        break;
                    }
                    part = part.saturating_add(1);
                    let rotated = rotated_session_path(&session_path, part);
                    let Ok(file) = OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(rotated)
                    else {
                        break;
                    };
                    writer = BufWriter::with_capacity(64 * 1024, file);
                    bytes_written = 0;
                    file_started = Instant::now();
                }
                let record = EventRecord {
                    schema: EVENT_SCHEMA,
                    session: session.clone(),
                    sequence: last_sequence,
                    monotonic_ms: event.monotonic_ms,
                    wall_utc: rfc3339_millis(event.wall_unix_ms),
                    thread: event.thread,
                    kind: event.kind,
                    payload: event.payload,
                };
                if append_record(&mut writer, &record, &tail, &mut bytes_written).is_err() {
                    break;
                }
                unflushed += 1;

                if event.flush
                    || unflushed >= WRITER_BATCH_EVENTS
                    || last_flush.elapsed() >= WRITER_FLUSH_INTERVAL
                {
                    let _ = writer.flush();
                    unflushed = 0;
                    last_flush = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                append_dropped_record(
                    &mut writer,
                    &session,
                    &dropped,
                    &tail,
                    &mut bytes_written,
                    &mut last_sequence,
                    session_started.elapsed(),
                );
                let _ = writer.flush();
                unflushed = 0;
                last_flush = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                append_dropped_record(
                    &mut writer,
                    &session,
                    &dropped,
                    &tail,
                    &mut bytes_written,
                    &mut last_sequence,
                    session_started.elapsed(),
                );
                break;
            }
        }
    }

    let finish = EventRecord {
        schema: EVENT_SCHEMA,
        session,
        sequence: last_sequence.saturating_add(1),
        monotonic_ms: saturating_millis(session_started.elapsed()),
        wall_utc: rfc3339_millis(unix_millis(SystemTime::now())),
        thread: "log-writer",
        kind: "session.finish",
        payload: json!({"orderly": true}),
    };
    let _ = append_record(&mut writer, &finish, &tail, &mut bytes_written);
    let _ = writer.flush();
}

fn append_dropped_record(
    writer: &mut BufWriter<File>,
    session: &str,
    dropped: &AtomicU64,
    tail: &Arc<Mutex<VecDeque<EventRecord>>>,
    bytes_written: &mut u64,
    last_sequence: &mut u64,
    elapsed: Duration,
) {
    let dropped_count = dropped.swap(0, Ordering::Relaxed);
    if dropped_count == 0 {
        return;
    }
    *last_sequence = last_sequence.saturating_add(1);
    let record = EventRecord {
        schema: EVENT_SCHEMA,
        session: session.to_owned(),
        sequence: *last_sequence,
        monotonic_ms: saturating_millis(elapsed),
        wall_utc: rfc3339_millis(unix_millis(SystemTime::now())),
        thread: "log-writer",
        kind: "trace.dropped",
        payload: json!({"count": dropped_count, "reason": "queue_full"}),
    };
    let _ = append_record(writer, &record, tail, bytes_written);
}

fn append_record(
    writer: &mut BufWriter<File>,
    record: &EventRecord,
    tail: &Arc<Mutex<VecDeque<EventRecord>>>,
    bytes_written: &mut u64,
) -> io::Result<()> {
    let encoded = serde_json::to_vec(record).map_err(io::Error::other)?;
    writer.write_all(&encoded)?;
    writer.write_all(b"\n")?;
    *bytes_written = bytes_written.saturating_add(encoded.len() as u64 + 1);
    let mut ring = tail
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if ring.len() == CRASH_TAIL_CAPACITY {
        ring.pop_front();
    }
    ring.push_back(record.clone());
    Ok(())
}

fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            write_panic_incident(info.location());
            previous(info);
        }));
    });
}

fn write_panic_incident(location: Option<&std::panic::Location<'_>>) {
    let Some(global) = PANIC_CONTEXT.get() else {
        return;
    };
    let Ok(guard) = global.try_lock() else {
        return;
    };
    let Some(context) = guard.as_ref() else {
        return;
    };
    let Ok(mut file) = context.file.try_lock() else {
        return;
    };
    let Ok(tail) = context.tail.try_lock() else {
        return;
    };
    let _ = file.set_len(0);
    let _ = serde_json::to_writer_pretty(
        &mut *file,
        &json!({
            "schema": EVENT_SCHEMA,
            "session": context.session,
            "kind": "panic",
            "wall_utc": rfc3339_millis(unix_millis(SystemTime::now())),
            "location": location.map(|location| json!({
                "file": source_file_basename(location.file()),
                "line": location.line(),
                "column": location.column()
            })),
            "event_tail": tail.iter().collect::<Vec<_>>()
        }),
    );
    let _ = file.flush();
    let _ = fs::rename(&context.pending_path, &context.final_path);
}

fn default_log_root() -> PathBuf {
    if let Some(override_root) = std::env::var_os("ARTIFICER_LOG_ROOT") {
        return PathBuf::from(override_root);
    }
    if cfg!(target_os = "macos")
        && let Some(user_root) = std::env::var_os("HOME")
    {
        return PathBuf::from(user_root)
            .join("Library")
            .join("Logs")
            .join("Artificer");
    }
    std::env::temp_dir().join("Artificer-Logs")
}

fn prune_sessions(directory: &Path) -> io::Result<()> {
    let mut files = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let is_jsonl = path
                .extension()
                .is_some_and(|extension| extension == "jsonl");
            let metadata = is_jsonl.then(|| entry.metadata().ok()).flatten()?;
            Some((
                path,
                metadata.modified().unwrap_or(UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, modified, _)| *modified);
    let mut total = files.iter().map(|(_, _, bytes)| *bytes).sum::<u64>();
    while files.len() > MAX_RETAINED_FILES || total > MAX_RETAINED_BYTES {
        let (path, _, bytes) = files.remove(0);
        // The directory scan accepts only direct `.jsonl` children of the
        // dedicated sessions directory, keeping retention narrowly scoped.
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(bytes);
        }
    }
    Ok(())
}

fn rotated_session_path(base: &Path, part: u32) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session");
    base.with_file_name(format!("{stem}.part-{part}.jsonl"))
}

fn quantized_position(position: egui::Pos2, viewport: Rect) -> (&'static str, f64, f64) {
    if viewport.contains(position) {
        (
            "viewport",
            quantize_scalar(f64::from(position.x - viewport.left()), 0.5),
            quantize_scalar(f64::from(position.y - viewport.top()), 0.5),
        )
    } else {
        (
            "ui",
            quantize_scalar(f64::from(position.x), 0.5),
            quantize_scalar(f64::from(position.y), 0.5),
        )
    }
}

fn quantize_scalar(value: f64, quantum: f64) -> f64 {
    (value / quantum).round() * quantum
}

fn pointer_button_name(button: PointerButton) -> &'static str {
    match button {
        PointerButton::Primary => "primary",
        PointerButton::Secondary => "secondary",
        PointerButton::Middle => "middle",
        PointerButton::Extra1 => "extra_1",
        PointerButton::Extra2 => "extra_2",
    }
}

fn button_index(button: PointerButton) -> usize {
    match button {
        PointerButton::Primary => 0,
        PointerButton::Secondary => 1,
        PointerButton::Middle => 2,
        PointerButton::Extra1 => 3,
        PointerButton::Extra2 => 4,
    }
}

fn modifier_payload(modifiers: Modifiers) -> Value {
    json!({
        "alt": modifiers.alt,
        "ctrl": modifiers.ctrl,
        "shift": modifiers.shift,
        "command": modifiers.command,
        "mac_cmd": modifiers.mac_cmd
    })
}

fn key_is_traceable(key: Key, modifiers: Modifiers) -> bool {
    modifiers.command
        || modifiers.ctrl
        || modifiers.alt
        || matches!(
            key,
            Key::Enter
                | Key::Escape
                | Key::Tab
                | Key::Backspace
                | Key::Delete
                | Key::Space
                | Key::ArrowDown
                | Key::ArrowLeft
                | Key::ArrowRight
                | Key::ArrowUp
                | Key::Home
                | Key::End
                | Key::PageDown
                | Key::PageUp
        )
}

fn source_file_basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
}

fn unix_millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn saturating_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn filename_timestamp(unix_ms: u64) -> String {
    rfc3339_millis(unix_ms)
        .replace(':', "-")
        .trim_end_matches('Z')
        .to_owned()
}

fn rfc3339_millis(unix_ms: u64) -> String {
    let seconds = (unix_ms / 1_000) as i64;
    let millis = unix_ms % 1_000;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

// Howard Hinnant's proleptic-Gregorian civil-from-days conversion.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "artificer-development-log-{name}-{}-{}",
            std::process::id(),
            SESSION_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn every_recorder_writes_an_ordered_session() {
        let root = test_root("session");
        let path = {
            let recorder = DevelopmentRecorder::start_in(root.clone()).unwrap();
            recorder.log("command.activate", json!({"command": "Extrude"}));
            recorder.session_path().to_path_buf()
        };
        let contents = fs::read_to_string(path).unwrap();
        assert!(contents.contains("\"session.start\""));
        assert!(contents.contains("\"command.activate\""));
        assert!(contents.contains("\"session.finish\""));
        let sequences = contents
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["sequence"]
                    .as_u64()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(sequences.iter().copied().eq(1..=sequences.len() as u64));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn text_ime_paste_and_plain_character_keys_are_never_recorded() {
        let root = test_root("privacy");
        let context = egui::Context::default();
        let path = {
            let mut recorder = DevelopmentRecorder::start_in(root.clone()).unwrap();
            context.begin_pass(egui::RawInput {
                events: vec![
                    Event::Text("sensitive-model-name".to_owned()),
                    Event::Paste("secret clipboard".to_owned()),
                    Event::Key {
                        key: Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                    Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
                ..Default::default()
            });
            recorder.capture_egui_input(
                &context,
                Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0)),
            );
            let _ = context.end_pass();
            recorder.session_path().to_path_buf()
        };
        let contents = fs::read_to_string(path).unwrap();
        assert!(!contents.contains("sensitive-model-name"));
        assert!(!contents.contains("secret clipboard"));
        assert!(!contents.contains("\"key\":\"A\""));
        assert!(contents.contains("\"key\":\"Enter\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn drag_motion_records_endpoints_without_a_motion_trail() {
        let root = test_root("coalesce");
        let context = egui::Context::default();
        let path = {
            let mut recorder = DevelopmentRecorder::start_in(root.clone()).unwrap();
            context.begin_pass(egui::RawInput {
                events: vec![Event::PointerButton {
                    pos: egui::pos2(10.0, 10.0),
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                }],
                ..Default::default()
            });
            let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 100.0));
            recorder.capture_egui_input(&context, viewport);
            let _ = context.end_pass();
            context.begin_pass(egui::RawInput {
                events: (0..100)
                    .map(|index| Event::PointerMoved(egui::pos2(index as f32, 20.0)))
                    .chain(std::iter::once(Event::PointerButton {
                        pos: egui::pos2(90.0, 20.0),
                        button: PointerButton::Primary,
                        pressed: false,
                        modifiers: Modifiers::NONE,
                    }))
                    .collect(),
                ..Default::default()
            });
            recorder.capture_egui_input(&context, viewport);
            let _ = context.end_pass();
            recorder.session_path().to_path_buf()
        };
        let contents = fs::read_to_string(path).unwrap();
        assert_eq!(contents.matches("input.pointer_gesture_start").count(), 1);
        assert_eq!(contents.matches("input.pointer_gesture_finish").count(), 1);
        assert!(!contents.contains("PointerMoved"));
        assert!(!contents.contains("input.drag_sample"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn idle_frame_capture_stays_far_below_the_sixty_hertz_budget() {
        let root = test_root("idle-budget");
        let context = egui::Context::default();
        let mut recorder = DevelopmentRecorder::start_in(root.clone()).unwrap();
        context.begin_pass(egui::RawInput::default());
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1_280.0, 800.0));
        let started = Instant::now();
        for _ in 0..600 {
            recorder.capture_egui_input(&context, viewport);
        }
        let elapsed = started.elapsed();
        let _ = context.end_pass();
        drop(recorder);
        assert!(
            elapsed < Duration::from_millis(250),
            "600 idle frames of trace capture took {elapsed:?}"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retention_keeps_the_newest_ten_session_files() {
        let root = test_root("retention");
        let sessions = root.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        for index in 0..14 {
            fs::write(sessions.join(format!("{index:02}.jsonl")), b"event\n").unwrap();
            thread::sleep(Duration::from_millis(2));
        }
        prune_sessions(&sessions).unwrap();
        let remaining = fs::read_dir(&sessions).unwrap().count();
        assert_eq!(remaining, MAX_RETAINED_FILES);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rfc3339_formatter_matches_the_unix_epoch() {
        assert_eq!(rfc3339_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_millis(1_775_214_896_789),
            "2026-04-03T11:14:56.789Z"
        );
    }
}
