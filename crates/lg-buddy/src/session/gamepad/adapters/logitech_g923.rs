use std::io;
use std::path::PathBuf;

use super::{
    ActivityReader, ActivityReaderKey, ActivityReaderSpec, ActivityReaderSurface,
    GamepadActivityAdapter,
};
use crate::session::gamepad::devices::GamepadDevice;
use crate::session::gamepad::hidraw::RawHidReportReader;
use crate::session::gamepad::{AxisRange, DeviceId, RawGamepadEvent, RawGamepadEventKind};

const LOGITECH_VENDOR_ID: u16 = 0x046d;
const LOGITECH_G923_PRODUCT_ID: u16 = 0xc267;
const INPUT_REPORT_ID: u8 = 0x01;
const INPUT_REPORT_MINIMUM_SIZE: usize = 10;
const BUTTON_COUNT: u16 = 14;
const ANALOG_BYTE_OFFSETS: [usize; 6] = [1, 2, 3, 4, 8, 9];
// Adapter axes must not share registry keys with the evdev reader: the two
// surfaces can expose the same physical control with different scales.
const ADAPTER_AXIS_BASE: u16 = 0x100;
const HAT_X_AXIS: u16 = ADAPTER_AXIS_BASE + ANALOG_BYTE_OFFSETS.len() as u16;
const HAT_Y_AXIS: u16 = HAT_X_AXIS + 1;
const AXIS_RANGE: AxisRange = AxisRange {
    minimum: 0,
    maximum: 255,
    flat: 15,
    fuzz: 0,
};
const HAT_RANGE: AxisRange = AxisRange {
    minimum: -1,
    maximum: 1,
    flat: 0,
    fuzz: 0,
};

#[derive(Debug)]
pub(super) struct LogitechG923Adapter;

pub(super) static ADAPTER: LogitechG923Adapter = LogitechG923Adapter;

impl GamepadActivityAdapter for LogitechG923Adapter {
    fn name(&self) -> &'static str {
        "logitech-g923"
    }

    fn supports(&self, device: &GamepadDevice) -> bool {
        device.vendor_id == LOGITECH_VENDOR_ID && device.product_id == LOGITECH_G923_PRODUCT_ID
    }

    fn reader_specs(&self, device: &GamepadDevice) -> Vec<Box<dyn ActivityReaderSpec>> {
        device
            .hidraw_paths
            .iter()
            .map(|path| {
                Box::new(LogitechG923ActivityReaderSpec::new(
                    self.name(),
                    device.id.clone(),
                    path.clone(),
                )) as Box<dyn ActivityReaderSpec>
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct LogitechG923ActivityReaderSpec {
    key: ActivityReaderKey,
    device_id: DeviceId,
    path: PathBuf,
}

impl LogitechG923ActivityReaderSpec {
    fn new(adapter: &'static str, device_id: DeviceId, path: PathBuf) -> Self {
        let key = ActivityReaderKey::Adapter {
            adapter,
            device_id: device_id.clone(),
            surface: ActivityReaderSurface::Hidraw(path.clone()),
        };

        Self {
            key,
            device_id,
            path,
        }
    }
}

impl ActivityReaderSpec for LogitechG923ActivityReaderSpec {
    fn key(&self) -> ActivityReaderKey {
        self.key.clone()
    }

    fn open(&self) -> io::Result<Box<dyn ActivityReader>> {
        Ok(Box::new(LogitechG923ActivityReader {
            key: self.key.clone(),
            device_id: self.device_id.clone(),
            reports: RawHidReportReader::open(&self.path)?,
            previous_buttons: None,
        }))
    }
}

#[derive(Debug)]
struct LogitechG923ActivityReader {
    key: ActivityReaderKey,
    device_id: DeviceId,
    reports: RawHidReportReader,
    previous_buttons: Option<u16>,
}

impl ActivityReader for LogitechG923ActivityReader {
    fn key(&self) -> &ActivityReaderKey {
        &self.key
    }

    fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    fn read_available(&mut self) -> io::Result<Vec<RawGamepadEvent>> {
        let mut events = Vec::new();
        for report in self.reports.read_available()? {
            events.extend(events_from_report(
                &self.device_id,
                &mut self.previous_buttons,
                &report,
            ));
        }
        Ok(events)
    }
}

fn events_from_report(
    device_id: &DeviceId,
    previous_buttons: &mut Option<u16>,
    report: &[u8],
) -> Vec<RawGamepadEvent> {
    if report.len() < INPUT_REPORT_MINIMUM_SIZE || report[0] != INPUT_REPORT_ID {
        return Vec::new();
    }

    let mut observations = Vec::with_capacity(8 + usize::from(BUTTON_COUNT));
    for (index, byte_offset) in ANALOG_BYTE_OFFSETS.iter().copied().enumerate() {
        observations.push(axis_observation(
            device_id,
            ADAPTER_AXIS_BASE + index as u16,
            i32::from(report[byte_offset]),
            AXIS_RANGE,
        ));
    }

    let (hat_x, hat_y) = hat_coordinates(report[5] & 0x0f);
    observations.push(axis_observation(device_id, HAT_X_AXIS, hat_x, HAT_RANGE));
    observations.push(axis_observation(device_id, HAT_Y_AXIS, hat_y, HAT_RANGE));

    let buttons =
        (u32::from(report[5]) | (u32::from(report[6]) << 8) | (u32::from(report[7]) << 16)) >> 4;
    let buttons = (buttons & ((1 << BUTTON_COUNT) - 1)) as u16;
    if let Some(previous) = previous_buttons.replace(buttons) {
        let changed = previous ^ buttons;
        for bit in 0..BUTTON_COUNT {
            let mask = 1_u16 << bit;
            if changed & mask != 0 {
                observations.push(RawGamepadEvent {
                    device_id: device_id.clone(),
                    kind: RawGamepadEventKind::Button {
                        code: bit,
                        pressed: buttons & mask != 0,
                    },
                });
            }
        }
    }

    observations
}

fn axis_observation(
    device_id: &DeviceId,
    code: u16,
    value: i32,
    range: AxisRange,
) -> RawGamepadEvent {
    RawGamepadEvent {
        device_id: device_id.clone(),
        kind: RawGamepadEventKind::Axis { code, value, range },
    }
}

fn hat_coordinates(value: u8) -> (i32, i32) {
    match value {
        0 => (0, -1),
        1 => (1, -1),
        2 => (1, 0),
        3 => (1, 1),
        4 => (0, 1),
        5 => (-1, 1),
        6 => (-1, 0),
        7 => (-1, -1),
        _ => (0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        events_from_report, GamepadActivityAdapter, LogitechG923Adapter, ANALOG_BYTE_OFFSETS,
    };
    use crate::session::gamepad::activity::ActivityPolicy;
    use crate::session::gamepad::devices::GamepadDevice;
    use crate::session::gamepad::registry::ActivityRegistry;
    use crate::session::gamepad::{DeviceId, RawGamepadEvent};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn device(vendor_id: u16, product_id: u16, hidraw_paths: Vec<PathBuf>) -> GamepadDevice {
        GamepadDevice {
            id: DeviceId::new("event-controller"),
            path: PathBuf::from("/dev/input/event0"),
            vendor_id,
            product_id,
            hidraw_paths,
        }
    }

    #[test]
    fn supports_only_logitech_g923() {
        let adapter = LogitechG923Adapter;

        assert!(adapter.supports(&device(0x046d, 0xc267, Vec::new())));
        assert!(!adapter.supports(&device(0x054c, 0x0df2, Vec::new())));
        assert!(!adapter.supports(&device(0x046d, 0xc299, Vec::new())));
    }

    #[test]
    fn reader_specs_follow_hidraw_paths() {
        let adapter = LogitechG923Adapter;
        let specs = adapter.reader_specs(&device(
            0x046d,
            0xc267,
            vec![PathBuf::from("/dev/hidraw2"), PathBuf::from("/dev/hidraw8")],
        ));

        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[0].key().to_string(),
            "logitech-g923 hidraw /dev/hidraw2 for event-controller"
        );
        assert_eq!(
            specs[1].key().to_string(),
            "logitech-g923 hidraw /dev/hidraw8 for event-controller"
        );
    }

    fn report() -> [u8; 64] {
        let mut report = [0_u8; 64];
        report[..10].copy_from_slice(&[0x01, 128, 128, 128, 128, 0x08, 0, 0, 0, 0]);
        report
    }

    fn observe(
        registry: &mut ActivityRegistry,
        events: Vec<RawGamepadEvent>,
        now: Instant,
    ) -> bool {
        events.into_iter().any(|event| registry.observe(event, now))
    }

    #[test]
    fn first_report_seeds_controls_without_activity() {
        let device_id = DeviceId::new("event-controller");
        let mut previous_buttons = None;
        let events = events_from_report(&device_id, &mut previous_buttons, &report());
        let mut registry = ActivityRegistry::new(ActivityPolicy::default());

        assert!(!observe(&mut registry, events, Instant::now()));
        assert_eq!(previous_buttons, Some(0));
    }

    #[test]
    fn changing_only_vendor_status_bytes_is_not_activity() {
        let device_id = DeviceId::new("event-controller");
        let mut previous_buttons = None;
        let mut registry = ActivityRegistry::new(ActivityPolicy::default());
        let started = Instant::now();
        assert!(!observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &report()),
            started,
        ));

        let mut status_report = report();
        status_report[10..].fill(0xff);

        assert!(!observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &status_report),
            started + Duration::from_secs(20),
        ));
    }

    #[test]
    fn meaningful_movement_on_each_analog_field_is_activity() {
        for byte_offset in ANALOG_BYTE_OFFSETS {
            let device_id = DeviceId::new("event-controller");
            let mut previous_buttons = None;
            let mut registry = ActivityRegistry::new(ActivityPolicy::default());
            let started = Instant::now();
            assert!(!observe(
                &mut registry,
                events_from_report(&device_id, &mut previous_buttons, &report()),
                started,
            ));

            let mut moved_report = report();
            moved_report[byte_offset] = 144;

            assert!(
                observe(
                    &mut registry,
                    events_from_report(&device_id, &mut previous_buttons, &moved_report),
                    started + Duration::from_millis(100),
                ),
                "movement in report byte {byte_offset} should be activity"
            );
        }
    }

    #[test]
    fn small_axis_jitter_is_not_activity() {
        let device_id = DeviceId::new("event-controller");
        let mut previous_buttons = None;
        let mut registry = ActivityRegistry::new(ActivityPolicy::default());
        let started = Instant::now();
        assert!(!observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &report()),
            started,
        ));

        let mut jittered_report = report();
        jittered_report[1] = 129;

        assert!(!observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &jittered_report),
            started + Duration::from_millis(100),
        ));
    }

    #[test]
    fn button_transition_is_activity() {
        let device_id = DeviceId::new("event-controller");
        let mut previous_buttons = None;
        let mut registry = ActivityRegistry::new(ActivityPolicy::default());
        let started = Instant::now();
        assert!(!observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &report()),
            started,
        ));

        let mut pressed_report = report();
        pressed_report[5] |= 0x10;

        assert!(observe(
            &mut registry,
            events_from_report(&device_id, &mut previous_buttons, &pressed_report),
            started + Duration::from_millis(100),
        ));
    }
}
