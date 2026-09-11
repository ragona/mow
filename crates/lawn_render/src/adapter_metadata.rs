//! Metadata for the device actually rendering the scene. Browser information is
//! read from the configured canvas's device, never from a second adapter query.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterMetadataSource {
    /// Information returned by the selected wgpu device. The browser backend
    /// currently exposes only the description through this API.
    #[default]
    WgpuDevice,
    /// Standard GPUDevice.adapterInfo obtained from the configured canvas.
    BrowserConfiguredDevice,
}

/// Optional identifying information the selected graphics device exposes.
/// `None` means unavailable; it must not be interpreted as a zero identifier,
/// a hardware adapter, or a software adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterMetadata {
    pub description: Option<String>,
    /// Browser vendor string, or a hexadecimal backend vendor ID on native.
    pub vendor: Option<String>,
    pub architecture: Option<String>,
    /// Browser device string, or a hexadecimal backend device ID on native.
    pub device: Option<String>,
    /// WebGPU's explicit fallback flag, when exposed. Native wgpu does not
    /// expose this flag, so native device type is reported separately.
    pub fallback: Option<bool>,
    pub device_type: Option<String>,
    pub driver: Option<String>,
    pub driver_info: Option<String>,
    pub source: AdapterMetadataSource,
}

impl AdapterMetadata {
    /// A human-readable label made only from identifying fields the selected
    /// device exposes. An architecture is not an inferred GPU model.
    #[must_use]
    pub fn display_label(&self) -> String {
        if let Some(description) = self
            .description
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return description.to_owned();
        }
        let details: Vec<_> = [
            self.vendor.as_deref(),
            self.architecture.as_deref(),
            self.device.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();
        if details.is_empty() {
            "GPU details unavailable".to_owned()
        } else {
            details.join(" / ")
        }
    }

    /// Reads metadata after the window's surface has been configured with
    /// `device`. Missing browser APIs or privacy-redacted fields remain absent.
    #[must_use]
    pub fn for_device(device: &wgpu::Device, window: &winit::window::Window) -> Self {
        let info = device.adapter_info();
        let metadata = Self::from_info(&info);
        #[cfg(target_arch = "wasm32")]
        if info.backend == wgpu::Backend::BrowserWebGpu
            && let Some(details) = browser::from_configured_window(window)
        {
            return metadata.with_browser_details(details);
        }
        let _ = window;
        metadata
    }

    fn from_info(info: &wgpu::AdapterInfo) -> Self {
        let browser = info.backend == wgpu::Backend::BrowserWebGpu;
        Self {
            description: nonempty(&info.name),
            vendor: (!browser && info.vendor != 0).then(|| format!("0x{:04x}", info.vendor)),
            architecture: None,
            device: (!browser && info.device != 0).then(|| format!("0x{:04x}", info.device)),
            // DeviceType::Cpu is useful metadata but is not a WebGPU fallback
            // flag. Do not invent that flag for native or unreported browsers.
            fallback: None,
            device_type: (!browser && info.device_type != wgpu::DeviceType::Other)
                .then(|| format!("{:?}", info.device_type)),
            driver: (!browser).then(|| nonempty(&info.driver)).flatten(),
            driver_info: (!browser).then(|| nonempty(&info.driver_info)).flatten(),
            source: AdapterMetadataSource::WgpuDevice,
        }
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn with_browser_details(mut self, details: BrowserDetails) -> Self {
        self.description = details.description.and_then(|value| nonempty(&value));
        self.vendor = details.vendor.and_then(|value| nonempty(&value));
        self.architecture = details.architecture.and_then(|value| nonempty(&value));
        self.device = details.device.and_then(|value| nonempty(&value));
        self.fallback = details.fallback;
        self.source = AdapterMetadataSource::BrowserConfiguredDevice;
        self
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(Default)]
struct BrowserDetails {
    description: Option<String>,
    vendor: Option<String>,
    architecture: Option<String>,
    device: Option<String>,
    fallback: Option<bool>,
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use js_sys::{Function, Reflect};
    use wasm_bindgen::{JsCast, JsValue};
    use winit::{platform::web::WindowExtWebSys, window::Window};

    use super::BrowserDetails;

    fn property(object: &JsValue, name: &str) -> Option<JsValue> {
        Reflect::get(object, &JsValue::from_str(name))
            .ok()
            .filter(|value| !value.is_null() && !value.is_undefined())
    }

    pub(super) fn from_configured_window(window: &Window) -> Option<BrowserDetails> {
        let canvas = window.canvas()?;
        let context = canvas.get_context("webgpu").ok()??;
        // Wgpu 29 keeps its WebGPU handles private. getConfiguration() is the
        // standard browser API for reading the device already configured on
        // this exact canvas. Reflect keeps newer optional getters optional,
        // without depending on unstable web-sys WebGPU bindings.
        // https://gpuweb.github.io/gpuweb/#dom-gpucanvascontext-getconfiguration
        let get_configuration = property(context.as_ref(), "getConfiguration")?
            .dyn_into::<Function>()
            .ok()?;
        let configuration = get_configuration.call0(context.as_ref()).ok()?;
        let device = property(&configuration, "device")?;
        let info = property(&device, "adapterInfo")?;
        Some(BrowserDetails {
            description: property(&info, "description").and_then(|value| value.as_string()),
            vendor: property(&info, "vendor").and_then(|value| value.as_string()),
            architecture: property(&info, "architecture").and_then(|value| value.as_string()),
            device: property(&info, "device").and_then(|value| value.as_string()),
            fallback: property(&info, "isFallbackAdapter").and_then(|value| value.as_bool()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(backend: wgpu::Backend) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo {
            name: "Selected GPU".into(),
            vendor: 0x1234,
            device: 0x5678,
            device_type: wgpu::DeviceType::DiscreteGpu,
            device_pci_bus_id: String::new(),
            driver: "selected driver".into(),
            driver_info: "driver version".into(),
            backend,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        }
    }

    #[test]
    fn native_metadata_preserves_ids_and_device_type_without_guessing_fallback() {
        let metadata = AdapterMetadata::from_info(&info(wgpu::Backend::Vulkan));
        assert_eq!(metadata.description.as_deref(), Some("Selected GPU"));
        assert_eq!(metadata.vendor.as_deref(), Some("0x1234"));
        assert_eq!(metadata.device.as_deref(), Some("0x5678"));
        assert_eq!(metadata.device_type.as_deref(), Some("DiscreteGpu"));
        assert_eq!(metadata.driver.as_deref(), Some("selected driver"));
        assert!(metadata.architecture.is_none());
        assert!(metadata.fallback.is_none());
        let mut cpu = info(wgpu::Backend::Vulkan);
        cpu.device_type = wgpu::DeviceType::Cpu;
        let cpu = AdapterMetadata::from_info(&cpu);
        assert_eq!(cpu.device_type.as_deref(), Some("Cpu"));
        assert!(cpu.fallback.is_none());
    }

    #[test]
    fn missing_browser_details_do_not_turn_placeholders_into_hardware_claims() {
        let mut info = info(wgpu::Backend::BrowserWebGpu);
        info.name = " \t ".into();
        let metadata = AdapterMetadata::from_info(&info);
        assert_eq!(metadata, AdapterMetadata::default());
        let metadata = metadata.with_browser_details(BrowserDetails {
            description: Some(String::new()),
            vendor: Some("   ".into()),
            architecture: Some("\n".into()),
            device: None,
            fallback: None,
        });
        assert!(metadata.description.is_none());
        assert!(metadata.vendor.is_none());
        assert!(metadata.architecture.is_none());
        assert!(metadata.device.is_none());
        assert!(metadata.fallback.is_none());
        assert_eq!(
            metadata.source,
            AdapterMetadataSource::BrowserConfiguredDevice
        );
    }

    #[test]
    fn browser_explicit_false_and_true_are_distinct_from_unavailable() {
        for fallback in [Some(false), Some(true), None] {
            let metadata = AdapterMetadata::from_info(&info(wgpu::Backend::BrowserWebGpu))
                .with_browser_details(BrowserDetails {
                    description: Some("  Browser GPU  ".into()),
                    vendor: Some("vendor".into()),
                    architecture: Some("architecture".into()),
                    device: Some("device".into()),
                    fallback,
                });
            assert_eq!(metadata.description.as_deref(), Some("Browser GPU"));
            assert_eq!(metadata.vendor.as_deref(), Some("vendor"));
            assert_eq!(metadata.architecture.as_deref(), Some("architecture"));
            assert_eq!(metadata.device.as_deref(), Some("device"));
            assert_eq!(metadata.fallback, fallback);
        }
    }

    #[test]
    fn display_label_prefers_description_then_available_identifying_fields() {
        let mut metadata = AdapterMetadata {
            description: Some("  Apple M2  ".into()),
            vendor: Some("apple".into()),
            architecture: Some("metal-3".into()),
            device: Some("device-id".into()),
            ..AdapterMetadata::default()
        };
        assert_eq!(metadata.display_label(), "Apple M2");
        metadata.description = Some(" \t ".into());
        assert_eq!(metadata.display_label(), "apple / metal-3 / device-id");
        metadata.device = None;
        assert_eq!(metadata.display_label(), "apple / metal-3");
        metadata.vendor = None;
        assert_eq!(metadata.display_label(), "metal-3");
        metadata.architecture = None;
        metadata.device = Some("  device-id  ".into());
        assert_eq!(metadata.display_label(), "device-id");
    }

    #[test]
    fn display_label_does_not_invent_identity_from_type_or_fallback_status() {
        for fallback in [None, Some(false), Some(true)] {
            let metadata = AdapterMetadata {
                vendor: Some("\n".into()),
                fallback,
                device_type: Some("DiscreteGpu".into()),
                driver: Some("driver".into()),
                ..AdapterMetadata::default()
            };
            assert_eq!(metadata.display_label(), "GPU details unavailable");
        }
        assert_eq!(
            AdapterMetadata::default().display_label(),
            "GPU details unavailable"
        );
    }
}
