//! CPU preparation shared by the native test harness and the browser worker.
//! The wire payload contains owned Rust data only: no GPU handles, native
//! threads, or borrowed references cross the browser worker boundary.

use bincode::Options;
use lawn_core::{
    GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION,
    run::PreparedWorld,
};
use lawn_render::PreparedSky;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

const WIRE_VERSION: u32 = 1;
const MAX_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GenerationRequest {
    World {
        config: GeneratorConfig,
        seed: WorldSeed,
    },
    Sky,
    Startup {
        config: GeneratorConfig,
        seed: WorldSeed,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum GenerationResult {
    World(PreparedWorld),
    Sky(PreparedSky),
    Startup {
        world: PreparedWorld,
        sky: PreparedSky,
    },
}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    version: u32,
    value: T,
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    bincode::DefaultOptions::new()
        .with_limit(MAX_PAYLOAD_BYTES)
        .serialize(&Envelope {
            version: WIRE_VERSION,
            value,
        })
        .map_err(|error| format!("could not encode preparation payload: {error}"))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() as u64 > MAX_PAYLOAD_BYTES {
        return Err("preparation payload exceeds the browser size limit".into());
    }
    let envelope: Envelope<T> = bincode::DefaultOptions::new()
        .with_limit(MAX_PAYLOAD_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|error| format!("could not decode preparation payload: {error}"))?;
    if envelope.version != WIRE_VERSION {
        return Err("preparation worker and app use different payload versions".into());
    }
    Ok(envelope.value)
}

fn prepare(request: GenerationRequest) -> Result<GenerationResult, String> {
    let world = |config, seed| {
        PreparedWorld::generate(
            &PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config),
            seed,
        )
        .map_err(|error| error.to_string())
    };
    match request {
        GenerationRequest::World { config, seed } => {
            world(config, seed).map(GenerationResult::World)
        }
        GenerationRequest::Sky => Ok(GenerationResult::Sky(PreparedSky::bake())),
        GenerationRequest::Startup { config, seed } => Ok(GenerationResult::Startup {
            world: world(config, seed)?,
            sky: PreparedSky::bake(),
        }),
    }
}

fn prepare_payload(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let result = prepare(decode(bytes)?)?;
    encode(&result)
}

fn decode_result(bytes: &[u8]) -> Result<GenerationResult, String> {
    let result = decode(bytes)?;
    match &result {
        GenerationResult::Sky(sky) | GenerationResult::Startup { sky, .. } => {
            sky.validate().map_err(str::to_owned)?;
        }
        GenerationResult::World(_) => {}
    }
    Ok(result)
}

/// Called explicitly by `worker.js`, after initializing the WASM module. It
/// must never call the application's browser entry point or create a canvas.
///
/// # Errors
///
/// Returns a JavaScript error if the request is invalid or preparation fails.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn prepare_worker(bytes: &[u8]) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    prepare_payload(bytes).map_err(|error| wasm_bindgen::JsValue::from_str(&error))
}

#[cfg(target_arch = "wasm32")]
pub use browser::GenerationJob;

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::{cell::RefCell, rc::Rc};

    use js_sys::{Array, ArrayBuffer, Function, Promise, Reflect, Uint8Array};
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{ErrorEvent, MessageEvent, Worker, WorkerOptions, WorkerType};

    use super::{GenerationRequest, GenerationResult, decode_result, encode};

    type Completion = Rc<RefCell<Option<Result<GenerationResult, String>>>>;
    const PREPARATION_TIMEOUT_MS: i32 = 120_000;

    pub struct GenerationJob {
        worker: Worker,
        result: Completion,
        completion: Promise,
        // Retain the callbacks until completion/cancellation, then unregister
        // them before dropping their Rust closures.
        _on_message: Closure<dyn FnMut(MessageEvent)>,
        _on_error: Closure<dyn FnMut(ErrorEvent)>,
        _on_message_error: Closure<dyn FnMut(MessageEvent)>,
        on_timeout: Closure<dyn FnMut()>,
        timeout_handle: Option<i32>,
    }

    impl std::fmt::Debug for GenerationJob {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("GenerationJob")
                .field("finished", &self.result.borrow().is_some())
                .finish_non_exhaustive()
        }
    }

    fn complete(result: &Completion, resolve: &Function, value: Result<GenerationResult, String>) {
        if result.borrow().is_some() {
            return;
        }
        *result.borrow_mut() = Some(value);
        let _ = resolve.call0(&JsValue::UNDEFINED);
    }

    impl GenerationJob {
        pub fn spawn(request: &GenerationRequest) -> Result<Self, String> {
            let request = encode(request)?;
            let options = WorkerOptions::new();
            options.set_type(WorkerType::Module);
            let worker = Worker::new_with_options("./worker.js", &options)
                .map_err(|error| format!("could not start preparation worker: {error:?}"))?;
            let result: Completion = Rc::default();
            let mut resolver = None;
            let completion = Promise::new(&mut |resolve, _reject| resolver = Some(resolve));
            let resolve = resolver.expect("Promise constructor runs its executor immediately");
            let on_message = {
                let result = result.clone();
                let resolve = resolve.clone();
                Closure::new(move |event: MessageEvent| {
                    let data = event.data();
                    let value = if data.is_instance_of::<ArrayBuffer>() {
                        let bytes = Uint8Array::new(&data);
                        if u64::from(bytes.length()) > super::MAX_PAYLOAD_BYTES {
                            Err("preparation payload exceeds the browser size limit".into())
                        } else {
                            decode_result(&bytes.to_vec())
                        }
                    } else {
                        let error = Reflect::get(&data, &JsValue::from_str("error"))
                            .ok()
                            .and_then(|value| value.as_string())
                            .unwrap_or_else(|| {
                                "preparation worker returned an invalid response".into()
                            });
                        Err(error)
                    };
                    complete(&result, &resolve, value);
                })
            };
            let on_error = {
                let result = result.clone();
                let resolve = resolve.clone();
                Closure::new(move |event: ErrorEvent| {
                    event.prevent_default();
                    complete(
                        &result,
                        &resolve,
                        Err(format!("preparation worker failed: {}", event.message())),
                    );
                })
            };
            let on_message_error = {
                let result = result.clone();
                let resolve = resolve.clone();
                Closure::new(move |_event: MessageEvent| {
                    complete(
                        &result,
                        &resolve,
                        Err("preparation worker response could not be transferred".into()),
                    );
                })
            };
            let on_timeout = {
                let result = result.clone();
                let worker = worker.clone();
                Closure::new(move || {
                    if result.borrow().is_none() {
                        worker.terminate();
                        complete(
                            &result,
                            &resolve,
                            Err("Preparation timed out after two minutes. Reload the page to try again.".into()),
                        );
                    }
                })
            };
            worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
            worker.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
            let mut job = Self {
                worker,
                result,
                completion,
                _on_message: on_message,
                _on_error: on_error,
                _on_message_error: on_message_error,
                on_timeout,
                timeout_handle: None,
            };
            let window = web_sys::window()
                .ok_or_else(|| "preparation jobs must be started from the page".to_owned())?;
            job.timeout_handle = Some(
                window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        job.on_timeout.as_ref().unchecked_ref(),
                        PREPARATION_TIMEOUT_MS,
                    )
                    .map_err(|error| format!("could not schedule worker timeout: {error:?}"))?,
            );
            // Copy out of WASM memory once, then transfer the JS allocation.
            // Independent worker memories require no SharedArrayBuffer or
            // cross-origin-isolation headers.
            let bytes = Uint8Array::from(request.as_slice());
            let buffer = bytes.buffer();
            let transfer = Array::of1(&buffer);
            job.worker
                .post_message_with_transfer(&buffer, &transfer)
                .map_err(|error| format!("could not send preparation request: {error:?}"))?;
            Ok(job)
        }

        pub fn poll(&mut self) -> Option<Result<GenerationResult, String>> {
            self.result.borrow_mut().take()
        }

        pub async fn finish(mut self) -> Result<GenerationResult, String> {
            JsFuture::from(self.completion.clone())
                .await
                .map_err(|error| format!("preparation worker completion failed: {error:?}"))?;
            self.poll()
                .unwrap_or_else(|| Err("preparation worker finished without a result".into()))
        }
    }

    impl Drop for GenerationJob {
        fn drop(&mut self) {
            if let Some(handle) = self.timeout_handle
                && let Some(window) = web_sys::window()
            {
                window.clear_timeout_with_handle(handle);
            }
            self.worker.set_onmessage(None);
            self.worker.set_onerror(None);
            self.worker.set_onmessageerror(None);
            self.worker.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lawn_core::{
        GameMode, RunState,
        config::{JobConfig, VehicleTuning},
        input::InputSnapshot,
        profile::AccessibilitySettings,
    };

    #[test]
    fn worker_roundtrip_preserves_roots_mowing_and_race_simulation() {
        let config = GeneratorConfig {
            grass_roots_per_square_meter: 0.8,
            ..GeneratorConfig::test_quality()
        };
        let seed = WorldSeed(55);
        let request = encode(&GenerationRequest::World {
            config: config.clone(),
            seed,
        })
        .unwrap();
        let payload = prepare_payload(&request).unwrap();
        let GenerationResult::World(prepared) = decode_result(&payload).unwrap() else {
            panic!("world request must return world data");
        };
        let accessibility = AccessibilitySettings::default();
        let mut received = prepared.into_run(
            GameMode::TurfRace,
            VehicleTuning::default(),
            JobConfig::default(),
            &accessibility,
            false,
        );
        let mut reference = RunState::new(
            PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config)
                .generate(seed)
                .unwrap(),
            GameMode::TurfRace,
            VehicleTuning::default(),
            JobConfig::default(),
            &accessibility,
            false,
        );
        assert!(!received.planet.grass_roots.is_empty());
        assert_eq!(received.planet.terrain, reference.planet.terrain);
        assert_eq!(received.planet.grass_roots, reference.planet.grass_roots);
        assert_eq!(
            received.planet.grass_patches,
            reference.planet.grass_patches
        );
        assert_eq!(
            received.planet.deterministic_hash,
            reference.planet.deterministic_hash
        );
        assert_eq!(
            received.mowing.total_mowable_weight(),
            reference.mowing.total_mowable_weight()
        );
        for tick in 0..360 {
            let input = InputSnapshot {
                accelerate: 0.8,
                steer: if tick < 180 { 0.2 } else { -0.2 },
                boost_held: tick >= 240,
                ..InputSnapshot::default()
            };
            received.tick(input, &accessibility);
            reference.tick(input, &accessibility);
            assert_eq!(received.vehicle.state, reference.vehicle.state);
            assert_eq!(
                received.rival.as_ref().unwrap().state,
                reference.rival.as_ref().unwrap().state
            );
            assert_eq!(received.mowing.coverage(), reference.mowing.coverage());
            assert_eq!(
                received.mowing.packed_cells(),
                reference.mowing.packed_cells()
            );
            assert_eq!(
                received.mowing.packed_owners(),
                reference.mowing.packed_owners()
            );
            assert_eq!(
                received.mowing.take_dirty_tiles(),
                reference.mowing.take_dirty_tiles()
            );
        }
    }

    #[test]
    fn worker_protocol_rejects_truncated_trailing_and_unknown_version_payloads() {
        let mut request = encode(&GenerationRequest::Sky).unwrap();
        assert!(prepare_payload(&request[..request.len() - 1]).is_err());
        request.push(0);
        assert!(decode::<GenerationRequest>(&request).is_err());
        let unknown = bincode::DefaultOptions::new()
            .serialize(&Envelope {
                version: WIRE_VERSION + 1,
                value: GenerationRequest::Sky,
            })
            .unwrap();
        assert!(
            decode::<GenerationRequest>(&unknown)
                .unwrap_err()
                .contains("different payload versions")
        );
    }

    #[test]
    fn worker_reports_invalid_generation_config() {
        let request = encode(&GenerationRequest::World {
            config: GeneratorConfig {
                mowing_resolution: 0,
                ..GeneratorConfig::test_quality()
            },
            seed: WorldSeed(55),
        })
        .unwrap();
        assert!(prepare_payload(&request).is_err());
    }
}
