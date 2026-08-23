mod logitech_g923;

use std::fmt;
use std::io;
use std::path::PathBuf;

use super::devices::GamepadDevice;
use super::{DeviceId, RawGamepadEvent};

pub(crate) trait GamepadActivityAdapter: Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, device: &GamepadDevice) -> bool;
    fn reader_specs(&self, device: &GamepadDevice) -> Vec<Box<dyn ActivityReaderSpec>>;
}

pub(crate) trait ActivityReaderSpec: fmt::Debug {
    fn key(&self) -> ActivityReaderKey;
    fn open(&self) -> io::Result<Box<dyn ActivityReader>>;
}

pub(crate) trait ActivityReader: fmt::Debug {
    fn key(&self) -> &ActivityReaderKey;
    fn device_id(&self) -> &DeviceId;
    fn read_available(&mut self) -> io::Result<Vec<RawGamepadEvent>>;
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ActivityReaderKey {
    Adapter {
        adapter: &'static str,
        device_id: DeviceId,
        surface: ActivityReaderSurface,
    },
}

impl fmt::Display for ActivityReaderKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter {
                adapter,
                device_id,
                surface,
            } => write!(f, "{adapter} {surface} for {device_id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ActivityReaderSurface {
    Hidraw(PathBuf),
}

impl fmt::Display for ActivityReaderSurface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hidraw(path) => write!(f, "hidraw {}", path.display()),
        }
    }
}

static REGISTERED_ADAPTERS: &[&dyn GamepadActivityAdapter] = &[&logitech_g923::ADAPTER];

pub(crate) fn reader_specs_for_device(device: &GamepadDevice) -> Vec<Box<dyn ActivityReaderSpec>> {
    REGISTERED_ADAPTERS
        .iter()
        .filter(|adapter| adapter.supports(device))
        .flat_map(|adapter| adapter.reader_specs(device))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{reader_specs_for_device, ActivityReaderKey, ActivityReaderSurface};
    use crate::session::gamepad::devices::GamepadDevice;
    use crate::session::gamepad::DeviceId;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn device(
        id: &str,
        vendor_id: u16,
        product_id: u16,
        hidraw_paths: Vec<PathBuf>,
    ) -> GamepadDevice {
        GamepadDevice {
            id: DeviceId::new(id),
            path: PathBuf::from("/dev/input/event0"),
            vendor_id,
            product_id,
            hidraw_paths,
        }
    }

    #[test]
    fn reader_specs_for_device_ignores_unsupported_devices() {
        let specs = reader_specs_for_device(&device(
            "event-dualsense",
            0x054c,
            0x0df2,
            vec![PathBuf::from("/dev/hidraw2")],
        ));

        assert!(specs.is_empty());
    }

    #[test]
    fn reader_specs_for_device_collects_registered_adapter_specs() {
        let specs = reader_specs_for_device(&device(
            "event-wheel",
            0x046d,
            0xc267,
            vec![PathBuf::from("/dev/hidraw2"), PathBuf::from("/dev/hidraw8")],
        ));

        let keys = specs
            .iter()
            .map(|spec| spec.key().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            keys,
            vec![
                "logitech-g923 hidraw /dev/hidraw2 for event-wheel",
                "logitech-g923 hidraw /dev/hidraw8 for event-wheel",
            ]
        );
    }

    #[test]
    fn activity_reader_key_identity_includes_adapter_device_and_surface() {
        let base_key = ActivityReaderKey::Adapter {
            adapter: "adapter-a",
            device_id: DeviceId::new("event-wheel"),
            surface: ActivityReaderSurface::Hidraw(PathBuf::from("/dev/hidraw2")),
        };
        let different_adapter = ActivityReaderKey::Adapter {
            adapter: "adapter-b",
            device_id: DeviceId::new("event-wheel"),
            surface: ActivityReaderSurface::Hidraw(PathBuf::from("/dev/hidraw2")),
        };
        let different_device = ActivityReaderKey::Adapter {
            adapter: "adapter-a",
            device_id: DeviceId::new("event-flight-stick"),
            surface: ActivityReaderSurface::Hidraw(PathBuf::from("/dev/hidraw2")),
        };
        let different_surface = ActivityReaderKey::Adapter {
            adapter: "adapter-a",
            device_id: DeviceId::new("event-wheel"),
            surface: ActivityReaderSurface::Hidraw(PathBuf::from("/dev/hidraw8")),
        };

        let keys = HashSet::from([
            base_key.clone(),
            base_key.clone(),
            different_adapter,
            different_device,
            different_surface,
        ]);

        assert_eq!(keys.len(), 4);
        assert_eq!(
            base_key.to_string(),
            "adapter-a hidraw /dev/hidraw2 for event-wheel"
        );
    }
}
