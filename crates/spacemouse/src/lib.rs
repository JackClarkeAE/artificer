//! 3Dconnexion SpaceMouse input for the desktop applications.
//!
//! A SpaceMouse is a six-axis puck: the cap can be pushed along three axes
//! and twisted about three, and each deflection is reported continuously as
//! a signed value while the cap is held off centre. The device speaks plain
//! USB HID, so no vendor driver is involved: this crate enumerates the HID
//! devices, opens the puck, and parses its input reports on a reader thread.
//!
//! Everything that touches bytes is a pure function so it can be tested
//! without a device on the bench: [`parse_report`] turns one input report
//! into a [`Report`], and [`Accumulator`] folds reports into the [`Motion`]
//! the applications consume once a frame.
//!
//! # Coordinate frame
//!
//! [`Motion`] is expressed in the frame 3Dconnexion's own SDK uses: a
//! right-handed screen frame with **x to the right, y up, and z toward the
//! viewer**, rotations following the right-hand rule about those axes. A
//! raw HID report is in the device's own frame (x right, y toward the user,
//! z down, as the puck lies on the desk), and [`parse_report`] performs the
//! conversion so nothing downstream has to know how the puck is wired.
//!
//! # Absence
//!
//! No device, a device the process may not open (Linux hidraw nodes are
//! root-only until a udev rule grants access), or a device unplugged
//! mid-session are all ordinary states rather than errors. The reader thread
//! keeps looking every couple of seconds, and [`SpaceMouse::status`] says
//! what it found so the application can tell the user.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use hidapi::{DeviceInfo, HidApi, HidDevice};

/// Logitech's vendor id, used by the older 3Dconnexion units.
pub const VENDOR_LOGITECH: u16 = 0x046d;
/// 3Dconnexion's own vendor id.
pub const VENDOR_3DCONNEXION: u16 = 0x256f;

/// Product ids known to be six-axis pucks, under either vendor id.
pub const KNOWN_PRODUCT_IDS: [u16; 15] = [
    0xc625, // SpacePilot
    0xc626, // SpaceNavigator
    0xc627, // SpaceExplorer
    0xc628, // SpaceNavigator for Notebooks
    0xc629, // SpacePilot Pro
    0xc62b, // SpaceMouse Pro
    0xc62e, // SpaceMouse Wireless (cabled)
    0xc62f, // SpaceMouse Wireless receiver
    0xc631, // SpaceMouse Pro Wireless (cabled)
    0xc632, // SpaceMouse Pro Wireless receiver
    0xc633, // SpaceMouse Enterprise
    0xc635, // SpaceMouse Compact
    0xc650, // SpaceMouse Wireless BT (cabled)
    0xc651, // SpaceMouse Wireless BT receiver
    0xc652, // Universal receiver
];

/// HID usage page "Generic Desktop".
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
/// HID usage "Multi-axis Controller" on the Generic Desktop page.
const USAGE_MULTI_AXIS_CONTROLLER: u16 = 0x08;

/// The raw axis value the pucks report at full deflection.
pub const FULL_DEFLECTION: f64 = 350.0;
/// Deflections below this fraction of full scale are read as still, so a
/// cap that has not quite settled back to centre does not creep the view.
pub const DEAD_ZONE: f64 = 0.03;

/// How long the reader blocks in one read before checking whether it has
/// been asked to stop.
const READ_TIMEOUT_MS: i32 = 20;
/// How long the reader waits between looks for a device.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);
/// The granularity at which a waiting reader notices a stop request.
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// A puck reports continuously while deflected, so a deflection older than
/// this with no report since is a device that went quiet, not a cap still
/// being held: the held value is dropped rather than applied for ever.
const HOLD_TIMEOUT: Duration = Duration::from_millis(150);

/// Six-degree-of-freedom motion, normalised so full deflection is ±1.
///
/// The frame is right-handed with x to the right, y up, and z toward the
/// viewer; see the crate documentation.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Motion {
    /// Translation along the right, up, and toward-viewer axes.
    pub translate: [f64; 3],
    /// Right-hand rotation about the right, up, and toward-viewer axes.
    pub rotate: [f64; 3],
    /// Buttons that went down since the motion was last taken, as a bitmask
    /// in the device's own numbering: bit 0 is button 1.
    pub buttons_pressed: u32,
}

impl Motion {
    /// The cap is at rest: no axis is deflected.
    #[must_use]
    pub fn is_still(&self) -> bool {
        self.translate
            .iter()
            .chain(&self.rotate)
            .all(|axis| *axis == 0.0)
    }

    /// Nothing at all happened: no deflection and no button press.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.is_still() && self.buttons_pressed == 0
    }

    /// Whether the given button (1-based, as printed on the device) went
    /// down since the motion was taken.
    #[must_use]
    pub const fn button_pressed(&self, button: u32) -> bool {
        button >= 1 && button <= 32 && self.buttons_pressed & (1 << (button - 1)) != 0
    }
}

/// One decoded input report.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Report {
    /// Report id 1 carrying translation only (six bytes).
    Translation([f64; 3]),
    /// Report id 2 carrying rotation only (six bytes).
    Rotation([f64; 3]),
    /// Report id 1 carrying both (twelve bytes, newer devices).
    Motion {
        translate: [f64; 3],
        rotate: [f64; 3],
    },
    /// Report id 3: the buttons currently held down, as a bitmask.
    Buttons(u32),
}

/// Decodes one raw HID input report, report id in the first byte.
///
/// Returns `None` for reports this crate does not understand, including
/// truncated ones, so a device with extra collections cannot confuse the
/// reader.
#[must_use]
pub fn parse_report(bytes: &[u8]) -> Option<Report> {
    let (&report_id, data) = bytes.split_first()?;
    match report_id {
        1 if data.len() >= 12 => Some(Report::Motion {
            translate: screen_translation(raw_axes(&data[..6])?),
            rotate: screen_rotation(raw_axes(&data[6..12])?),
        }),
        1 => Some(Report::Translation(screen_translation(raw_axes(data)?))),
        2 => Some(Report::Rotation(screen_rotation(raw_axes(data)?))),
        3 => {
            if data.is_empty() {
                return None;
            }
            let mut mask = [0_u8; 4];
            let width = data.len().min(4);
            mask[..width].copy_from_slice(&data[..width]);
            Some(Report::Buttons(u32::from_le_bytes(mask)))
        }
        _ => None,
    }
}

/// Three little-endian `i16` axes from the first six bytes.
fn raw_axes(data: &[u8]) -> Option<[i16; 3]> {
    if data.len() < 6 {
        return None;
    }
    Some([
        i16::from_le_bytes([data[0], data[1]]),
        i16::from_le_bytes([data[2], data[3]]),
        i16::from_le_bytes([data[4], data[5]]),
    ])
}

/// Normalises one raw axis to ±1 at full deflection, with the dead zone
/// removed and the remaining travel rescaled so the response is continuous
/// at its edge.
#[must_use]
pub fn normalise_axis(raw: i16) -> f64 {
    let value = (f64::from(raw) / FULL_DEFLECTION).clamp(-1.0, 1.0);
    let magnitude = value.abs();
    if magnitude <= DEAD_ZONE {
        return 0.0;
    }
    let rescaled = (magnitude - DEAD_ZONE) / (1.0 - DEAD_ZONE);
    rescaled.copysign(value)
}

/// Device translation (x right, y toward the user, z down) into the screen
/// frame (x right, y up, z toward the viewer).
fn screen_translation([x, y, z]: [i16; 3]) -> [f64; 3] {
    [normalise_axis(x), -normalise_axis(z), normalise_axis(y)]
}

/// Device rotation about (right, toward-user, down) into right-hand rotation
/// about (right, up, toward-viewer). The right axis is shared; "up" is the
/// reverse of "down"; "toward the viewer" is the device's own y axis.
fn screen_rotation([rx, ry, rz]: [i16; 3]) -> [f64; 3] {
    [normalise_axis(rx), -normalise_axis(rz), normalise_axis(ry)]
}

/// How well a HID interface matches a SpaceMouse; higher is better, `None`
/// is not a candidate at all.
///
/// A known product id is the strongest evidence. Newer pucks expose several
/// HID collections (a keyboard for the buttons, an LED consumer control),
/// so among a known device's interfaces the multi-axis one is preferred and
/// any other named usage is skipped. An unknown product that declares
/// itself a multi-axis controller is accepted as a last resort, which is
/// what picks up models released after this list was written.
#[must_use]
pub fn candidate_rank(vendor_id: u16, product_id: u16, usage_page: u16, usage: u16) -> Option<u8> {
    let known_vendor = vendor_id == VENDOR_LOGITECH || vendor_id == VENDOR_3DCONNEXION;
    let known_product = known_vendor && KNOWN_PRODUCT_IDS.contains(&product_id);
    let multi_axis =
        usage_page == USAGE_PAGE_GENERIC_DESKTOP && usage == USAGE_MULTI_AXIS_CONTROLLER;
    let usage_unknown = usage_page == 0 && usage == 0;
    match (known_product, multi_axis, usage_unknown) {
        (true, true, _) => Some(3),
        (true, false, true) => Some(2),
        (true, false, false) => None,
        (false, true, _) if known_vendor => Some(1),
        (false, true, _) => Some(0),
        (false, false, _) => None,
    }
}

/// What the reader knows about the device right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceStatus {
    /// Nothing that looks like a SpaceMouse is attached; the reader keeps
    /// looking.
    Searching,
    /// A puck is attached and reporting.
    Connected { name: String },
    /// A puck is attached but the process may not open it. On Linux this
    /// is the missing udev rule.
    AccessDenied { name: String, error: String },
    /// The puck stopped answering — usually unplugged — and the reader is
    /// looking for it again.
    Lost { name: String, error: String },
    /// The HID subsystem itself could not be used.
    Unavailable { error: String },
}

impl DeviceStatus {
    /// One line for a status card.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Searching => "No 3D mouse found; looking every few seconds".to_owned(),
            Self::Connected { name } => format!("{name} connected"),
            Self::AccessDenied { name, error } => {
                format!("{name} found but could not be opened: {error}")
            }
            Self::Lost { name, error } => format!("{name} stopped responding: {error}"),
            Self::Unavailable { error } => format!("HID devices are unavailable: {error}"),
        }
    }

    /// The device's product name, when one is attached.
    #[must_use]
    pub fn device_name(&self) -> Option<&str> {
        match self {
            Self::Connected { name }
            | Self::AccessDenied { name, .. }
            | Self::Lost { name, .. } => Some(name),
            Self::Searching | Self::Unavailable { .. } => None,
        }
    }
}

/// Folds reports into the motion an application takes once a frame.
///
/// A puck reports its deflection continuously, several times per frame,
/// so the value handed to the application is the mean deflection since it
/// last asked — or, when no report arrived in between, the last reported
/// deflection for as long as it is fresh. Buttons are edge events: a press
/// is reported once, however many frames the button is held.
#[derive(Clone, Debug)]
pub struct Accumulator {
    translate_sum: [f64; 3],
    translate_samples: u32,
    rotate_sum: [f64; 3],
    rotate_samples: u32,
    held_translate: [f64; 3],
    held_rotate: [f64; 3],
    held_at: Option<Instant>,
    buttons_down: u32,
    buttons_pressed: u32,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Accumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            translate_sum: [0.0; 3],
            translate_samples: 0,
            rotate_sum: [0.0; 3],
            rotate_samples: 0,
            held_translate: [0.0; 3],
            held_rotate: [0.0; 3],
            held_at: None,
            buttons_down: 0,
            buttons_pressed: 0,
        }
    }

    /// Records one report. Returns whether the application has anything
    /// new to see, which is when the reader wakes it.
    pub fn apply(&mut self, report: Report, now: Instant) -> bool {
        // A deflection is news; so is the cap coming to rest, once, so the
        // application draws the frame that settles. A resting cap repeating
        // that it is at rest is not.
        match report {
            Report::Translation(translate) => {
                let changed = translate != [0.0; 3] || translate != self.held_translate;
                self.record_translation(translate, now);
                changed || self.held_rotate != [0.0; 3]
            }
            Report::Rotation(rotate) => {
                let changed = rotate != [0.0; 3] || rotate != self.held_rotate;
                self.record_rotation(rotate, now);
                changed || self.held_translate != [0.0; 3]
            }
            Report::Motion { translate, rotate } => {
                let changed = translate != [0.0; 3]
                    || rotate != [0.0; 3]
                    || translate != self.held_translate
                    || rotate != self.held_rotate;
                self.record_translation(translate, now);
                self.record_rotation(rotate, now);
                changed
            }
            Report::Buttons(down) => {
                let pressed = down & !self.buttons_down;
                self.buttons_down = down;
                self.buttons_pressed |= pressed;
                pressed != 0
            }
        }
    }

    fn record_translation(&mut self, translate: [f64; 3], now: Instant) {
        for (sum, value) in self.translate_sum.iter_mut().zip(translate) {
            *sum += value;
        }
        self.translate_samples += 1;
        self.held_translate = translate;
        self.held_at = Some(now);
    }

    fn record_rotation(&mut self, rotate: [f64; 3], now: Instant) {
        for (sum, value) in self.rotate_sum.iter_mut().zip(rotate) {
            *sum += value;
        }
        self.rotate_samples += 1;
        self.held_rotate = rotate;
        self.held_at = Some(now);
    }

    /// The device went away: whatever it last reported no longer applies.
    pub fn release(&mut self) {
        self.held_translate = [0.0; 3];
        self.held_rotate = [0.0; 3];
        self.held_at = None;
        self.translate_sum = [0.0; 3];
        self.translate_samples = 0;
        self.rotate_sum = [0.0; 3];
        self.rotate_samples = 0;
        self.buttons_down = 0;
    }

    /// Returns the motion since the last take and clears it.
    pub fn take(&mut self, now: Instant) -> Motion {
        let fresh = self
            .held_at
            .is_some_and(|at| now.saturating_duration_since(at) <= HOLD_TIMEOUT);
        let translate = if self.translate_samples > 0 {
            self.translate_sum
                .map(|sum| sum / f64::from(self.translate_samples))
        } else if fresh {
            self.held_translate
        } else {
            [0.0; 3]
        };
        let rotate = if self.rotate_samples > 0 {
            self.rotate_sum
                .map(|sum| sum / f64::from(self.rotate_samples))
        } else if fresh {
            self.held_rotate
        } else {
            [0.0; 3]
        };
        let buttons_pressed = self.buttons_pressed;
        self.translate_sum = [0.0; 3];
        self.translate_samples = 0;
        self.rotate_sum = [0.0; 3];
        self.rotate_samples = 0;
        self.buttons_pressed = 0;
        Motion {
            translate,
            rotate,
            buttons_pressed,
        }
    }
}

/// The reader's wake-up: called from the reader thread whenever new motion
/// or a button press has arrived, so a UI can request a repaint rather than
/// poll.
pub type WakeCallback = Box<dyn Fn() + Send + 'static>;

struct Shared {
    accumulator: Mutex<Accumulator>,
    status: Mutex<DeviceStatus>,
    stop: AtomicBool,
}

impl Shared {
    fn accumulator(&self) -> MutexGuard<'_, Accumulator> {
        self.accumulator
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn status(&self) -> MutexGuard<'_, DeviceStatus> {
        self.status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set_status(&self, status: DeviceStatus) {
        *self.status() = status;
    }

    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Sleeps for `duration`, returning early (and `true`) once asked to stop.
    fn sleep_unless_stopped(&self, duration: Duration) -> bool {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if self.should_stop() {
                return true;
            }
            thread::sleep(
                STOP_POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        self.should_stop()
    }
}

/// A SpaceMouse being read on a background thread.
///
/// Dropping it stops the thread and closes the device.
pub struct SpaceMouse {
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for SpaceMouse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpaceMouse")
            .field("status", &*self.shared.status())
            .finish_non_exhaustive()
    }
}

impl SpaceMouse {
    /// Starts looking for a puck, without a wake-up callback.
    ///
    /// Returns `None` only when the HID subsystem cannot be used at all —
    /// a missing device is not that: the reader keeps looking for one every
    /// couple of seconds, and [`Self::status`] says how the search is going.
    #[must_use]
    pub fn open() -> Option<Self> {
        Self::start(None)
    }

    /// Starts looking for a puck; `wake` is called from the reader thread
    /// whenever new motion or a button press has arrived.
    #[must_use]
    pub fn open_with_wake(wake: impl Fn() + Send + 'static) -> Option<Self> {
        Self::start(Some(Box::new(wake)))
    }

    fn start(wake: Option<WakeCallback>) -> Option<Self> {
        // Prove the HID layer works before spending a thread on it.
        if let Err(error) = HidApi::new() {
            eprintln!("SpaceMouse support is unavailable: {error}");
            return None;
        }
        let shared = Arc::new(Shared {
            accumulator: Mutex::new(Accumulator::new()),
            status: Mutex::new(DeviceStatus::Searching),
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let reader = thread::Builder::new()
            .name("spacemouse-reader".to_owned())
            .spawn(move || reader_loop(&worker, wake.as_deref()))
            .ok()?;
        Some(Self {
            shared,
            reader: Some(reader),
        })
    }

    /// Returns the motion since the last call and clears it.
    #[must_use]
    pub fn take_motion(&self) -> Motion {
        self.shared.accumulator().take(Instant::now())
    }

    /// What the reader currently knows about the device.
    #[must_use]
    pub fn status(&self) -> DeviceStatus {
        self.shared.status().clone()
    }

    /// Whether a puck is attached and reporting.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(*self.shared.status(), DeviceStatus::Connected { .. })
    }

    /// The attached puck's product name, if one is attached.
    #[must_use]
    pub fn device_name(&self) -> Option<String> {
        self.shared.status().device_name().map(str::to_owned)
    }
}

impl Drop for SpaceMouse {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            // A panicking reader has already said what it had to say.
            let _ = reader.join();
        }
    }
}

/// The reader thread: find a puck, read it until it fails, look again.
fn reader_loop(shared: &Shared, wake: Option<&(dyn Fn() + Send)>) {
    while !shared.should_stop() {
        let Some((device, name)) = find_and_open(shared) else {
            if shared.sleep_unless_stopped(RECONNECT_INTERVAL) {
                break;
            }
            continue;
        };
        shared.set_status(DeviceStatus::Connected { name: name.clone() });
        let error = read_until_failure(shared, &device, wake);
        shared.accumulator().release();
        if shared.should_stop() {
            break;
        }
        shared.set_status(DeviceStatus::Lost { name, error });
        if shared.sleep_unless_stopped(RECONNECT_INTERVAL) {
            break;
        }
    }
}

/// Enumerates HID devices and opens the best SpaceMouse candidate.
fn find_and_open(shared: &Shared) -> Option<(HidDevice, String)> {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(error) => {
            shared.set_status(DeviceStatus::Unavailable {
                error: error.to_string(),
            });
            return None;
        }
    };
    let candidate = api
        .device_list()
        .filter_map(|info| {
            candidate_rank(
                info.vendor_id(),
                info.product_id(),
                info.usage_page(),
                info.usage(),
            )
            .map(|rank| (rank, info))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, info)| info);
    let Some(info) = candidate else {
        shared.set_status(DeviceStatus::Searching);
        return None;
    };
    let name = device_display_name(info);
    match info.open_device(&api) {
        Ok(device) => Some((device, name)),
        Err(error) => {
            shared.set_status(DeviceStatus::AccessDenied {
                name,
                error: error.to_string(),
            });
            None
        }
    }
}

fn device_display_name(info: &DeviceInfo) -> String {
    match info.product_string() {
        Some(product) if !product.trim().is_empty() => product.trim().to_owned(),
        _ => format!(
            "3Dconnexion device {:04x}:{:04x}",
            info.vendor_id(),
            info.product_id()
        ),
    }
}

/// Reads reports until the device fails or the reader is stopped; returns
/// the failure's description (empty when stopped on request).
fn read_until_failure(
    shared: &Shared,
    device: &HidDevice,
    wake: Option<&(dyn Fn() + Send)>,
) -> String {
    let mut buffer = [0_u8; 64];
    loop {
        if shared.should_stop() {
            return String::new();
        }
        match device.read_timeout(&mut buffer, READ_TIMEOUT_MS) {
            Ok(0) => {}
            Ok(length) => {
                if let Some(report) = parse_report(&buffer[..length]) {
                    let changed = shared.accumulator().apply(report, Instant::now());
                    if changed && let Some(wake) = wake {
                        wake();
                    }
                }
            }
            Err(error) => return error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report_with_axes(id: u8, axes: [i16; 3]) -> Vec<u8> {
        let mut bytes = vec![id];
        for axis in axes {
            bytes.extend_from_slice(&axis.to_le_bytes());
        }
        bytes
    }

    fn assert_axes_close(actual: [f64; 3], expected: [f64; 3]) {
        for (actual, expected) in actual.iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0e-12,
                "{actual:?} != {expected:?}"
            );
        }
    }

    #[test]
    fn full_deflection_normalises_to_one_and_clamps_beyond() {
        assert_eq!(normalise_axis(350), 1.0);
        assert_eq!(normalise_axis(-350), -1.0);
        assert_eq!(normalise_axis(700), 1.0);
        assert_eq!(normalise_axis(i16::MIN), -1.0);
        assert_eq!(normalise_axis(0), 0.0);
    }

    #[test]
    fn the_dead_zone_swallows_a_resting_cap_and_is_continuous_at_its_edge() {
        // 3 % of 350 is 10.5 raw counts.
        assert_eq!(normalise_axis(10), 0.0);
        assert_eq!(normalise_axis(-10), 0.0);
        let just_outside = normalise_axis(11);
        assert!(just_outside > 0.0 && just_outside < 0.01, "{just_outside}");
        assert!(normalise_axis(-11) < 0.0);
        // Half deflection is rescaled past the dead zone, not merely passed.
        let half = normalise_axis(175);
        let expected = (0.5 - DEAD_ZONE) / (1.0 - DEAD_ZONE);
        assert!((half - expected).abs() <= 1.0e-12);
    }

    #[test]
    fn a_six_byte_translation_report_lands_in_the_screen_frame() {
        // Device x right, y toward the user, z down.
        let report = parse_report(&report_with_axes(1, [350, 0, 0])).unwrap();
        assert_eq!(report, Report::Translation([1.0, 0.0, 0.0]));
        // Pulling the cap toward the user is motion toward the viewer.
        let report = parse_report(&report_with_axes(1, [0, 350, 0])).unwrap();
        assert_eq!(report, Report::Translation([0.0, 0.0, 1.0]));
        // Pressing the cap down is motion downward on screen.
        let report = parse_report(&report_with_axes(1, [0, 0, 350])).unwrap();
        assert_eq!(report, Report::Translation([0.0, -1.0, 0.0]));
    }

    #[test]
    fn a_six_byte_rotation_report_lands_in_the_screen_frame() {
        let report = parse_report(&report_with_axes(2, [350, 0, 0])).unwrap();
        assert_eq!(report, Report::Rotation([1.0, 0.0, 0.0]));
        // Rotation about the device's toward-user axis is roll about the
        // viewing axis.
        let report = parse_report(&report_with_axes(2, [0, 350, 0])).unwrap();
        assert_eq!(report, Report::Rotation([0.0, 0.0, 1.0]));
        // A twist about the device's down axis is a right-hand rotation
        // about "up" with the opposite sign.
        let report = parse_report(&report_with_axes(2, [0, 0, 350])).unwrap();
        assert_eq!(report, Report::Rotation([0.0, -1.0, 0.0]));
    }

    #[test]
    fn a_twelve_byte_report_carries_both_halves() {
        let mut bytes = report_with_axes(1, [175, -350, 0]);
        for axis in [0_i16, 350, -175] {
            bytes.extend_from_slice(&axis.to_le_bytes());
        }
        let Report::Motion { translate, rotate } = parse_report(&bytes).unwrap() else {
            panic!("twelve bytes under report id 1 are translation and rotation");
        };
        let half = normalise_axis(175);
        assert_axes_close(translate, [half, 0.0, -1.0]);
        assert_axes_close(rotate, [0.0, -(-half), 1.0]);
    }

    #[test]
    fn button_reports_read_two_or_four_byte_masks() {
        assert_eq!(parse_report(&[3, 0b0000_0011]), Some(Report::Buttons(3)));
        assert_eq!(
            parse_report(&[3, 0x01, 0x02]),
            Some(Report::Buttons(0x0201))
        );
        assert_eq!(
            parse_report(&[3, 0x01, 0x00, 0x00, 0x80]),
            Some(Report::Buttons(0x8000_0001))
        );
        // Windows pads every report to the collection's longest; the pad
        // bytes are zero and do not disturb the mask.
        assert_eq!(
            parse_report(&[3, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00]),
            Some(Report::Buttons(2))
        );
    }

    #[test]
    fn malformed_reports_are_ignored_rather_than_misread() {
        assert_eq!(parse_report(&[]), None);
        assert_eq!(parse_report(&[1]), None);
        assert_eq!(parse_report(&[1, 1, 2, 3]), None);
        assert_eq!(parse_report(&[2, 1, 2, 3, 4, 5]), None);
        assert_eq!(parse_report(&[3]), None);
        assert_eq!(parse_report(&[9, 1, 2, 3, 4, 5, 6]), None);
    }

    #[test]
    fn known_products_outrank_the_usage_heuristic_and_skip_their_other_collections() {
        // A SpaceNavigator's one collection.
        assert_eq!(candidate_rank(0x046d, 0xc626, 1, 8), Some(3));
        // A libusb-style enumeration with no usage information.
        assert_eq!(candidate_rank(0x256f, 0xc635, 0, 0), Some(2));
        // A SpaceMouse Pro's keyboard collection is not the puck.
        assert_eq!(candidate_rank(0x256f, 0xc62b, 1, 6), None);
        // A newer 3Dconnexion model this list has not heard of.
        assert_eq!(candidate_rank(0x256f, 0xc700, 1, 8), Some(1));
        // Some other vendor's multi-axis controller, as a last resort.
        assert_eq!(candidate_rank(0x1234, 0x0001, 1, 8), Some(0));
        // Anything else is not a candidate.
        assert_eq!(candidate_rank(0x1234, 0x0001, 1, 6), None);
        assert_eq!(candidate_rank(0x046d, 0xc52b, 1, 2), None);
    }

    #[test]
    fn the_accumulator_reports_the_mean_deflection_since_the_last_take() {
        let start = Instant::now();
        let mut accumulator = Accumulator::new();
        assert!(accumulator.apply(Report::Translation([1.0, 0.0, 0.0]), start));
        assert!(accumulator.apply(Report::Translation([0.0, 0.0, 0.0]), start));
        assert!(accumulator.apply(Report::Rotation([0.0, 0.5, 0.0]), start));
        let motion = accumulator.take(start);
        assert_axes_close(motion.translate, [0.5, 0.0, 0.0]);
        assert_axes_close(motion.rotate, [0.0, 0.5, 0.0]);
        assert_eq!(motion.buttons_pressed, 0);
    }

    #[test]
    fn a_held_deflection_persists_between_reports_and_expires_when_the_device_goes_quiet() {
        let start = Instant::now();
        let mut accumulator = Accumulator::new();
        accumulator.apply(
            Report::Motion {
                translate: [0.0, 0.0, 1.0],
                rotate: [0.0, 0.0, 0.0],
            },
            start,
        );
        let first = accumulator.take(start);
        assert_axes_close(first.translate, [0.0, 0.0, 1.0]);
        // No report arrived since, but the cap is still held.
        let held = accumulator.take(start + Duration::from_millis(8));
        assert_axes_close(held.translate, [0.0, 0.0, 1.0]);
        // Long after the last report, the value is no longer trusted.
        let stale = accumulator.take(start + Duration::from_secs(1));
        assert!(stale.is_still());
        // A released device holds nothing at all.
        accumulator.apply(Report::Translation([1.0, 0.0, 0.0]), start);
        accumulator.release();
        assert!(accumulator.take(start).is_empty());
    }

    #[test]
    fn buttons_are_reported_once_per_press_however_long_they_are_held() {
        let now = Instant::now();
        let mut accumulator = Accumulator::new();
        assert!(accumulator.apply(Report::Buttons(0b01), now));
        // The device repeats the held state; that is not a second press.
        assert!(!accumulator.apply(Report::Buttons(0b01), now));
        assert!(accumulator.apply(Report::Buttons(0b11), now));
        let motion = accumulator.take(now);
        assert_eq!(motion.buttons_pressed, 0b11);
        assert!(motion.button_pressed(1));
        assert!(motion.button_pressed(2));
        assert!(!motion.button_pressed(3));
        assert!(!motion.button_pressed(0));
        // Taking clears the edges; the buttons are still down, not pressed.
        assert!(accumulator.take(now).is_empty());
        assert!(!accumulator.apply(Report::Buttons(0b00), now));
        assert!(accumulator.apply(Report::Buttons(0b01), now));
        assert_eq!(accumulator.take(now).buttons_pressed, 0b01);
    }

    #[test]
    fn a_resting_cap_wakes_the_application_once_and_then_not_again() {
        let now = Instant::now();
        let mut accumulator = Accumulator::new();
        assert!(!accumulator.apply(Report::Translation([0.0; 3]), now));
        assert!(!accumulator.apply(Report::Rotation([0.0; 3]), now));
        // Coming to rest is one last wake, so the settling frame is drawn.
        accumulator.apply(Report::Translation([1.0, 0.0, 0.0]), now);
        assert!(accumulator.apply(Report::Translation([0.0; 3]), now));
        assert!(!accumulator.apply(Report::Translation([0.0; 3]), now));
        // A rotation report while translation is held still matters: the
        // application has to keep applying the held translation.
        accumulator.apply(Report::Translation([1.0, 0.0, 0.0]), now);
        assert!(accumulator.apply(Report::Rotation([0.0; 3]), now));
    }

    #[test]
    fn status_lines_name_the_device_when_there_is_one() {
        let connected = DeviceStatus::Connected {
            name: "SpaceMouse Compact".to_owned(),
        };
        assert_eq!(connected.device_name(), Some("SpaceMouse Compact"));
        assert_eq!(connected.describe(), "SpaceMouse Compact connected");
        assert_eq!(DeviceStatus::Searching.device_name(), None);
        let denied = DeviceStatus::AccessDenied {
            name: "SpaceNavigator".to_owned(),
            error: "Permission denied (os error 13)".to_owned(),
        };
        assert!(denied.describe().contains("could not be opened"));
    }

    /// The bench has no puck; opening must still be quiet and stopping must
    /// not hang on the reconnect wait.
    #[test]
    fn opening_without_a_device_is_harmless_and_stops_promptly() {
        let Some(mouse) = SpaceMouse::open_with_wake(|| {}) else {
            // No HID subsystem at all in this environment: also fine.
            return;
        };
        let _ = mouse.status().describe();
        let started = Instant::now();
        drop(mouse);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stopping took {:?}",
            started.elapsed()
        );
    }
}
