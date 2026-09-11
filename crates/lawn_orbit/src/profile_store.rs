use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
#[cfg(not(target_arch = "wasm32"))]
use directories::ProjectDirs;
use lawn_core::profile::{PROFILE_VERSION, Profile};

#[derive(Clone, Debug)]
pub struct ProfileStore {
    path: PathBuf,
}

impl ProfileStore {
    #[cfg(test)]
    pub(crate) fn temporary(name: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self {
            path: std::env::temp_dir().join(format!(
                "lawn-orbit-{name}-{}-{nonce}.ron",
                std::process::id()
            )),
        }
    }

    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn discover() -> Self {
        let path = ProjectDirs::from("games", "OpenAI", "Lawn Orbit").map_or_else(
            || PathBuf::from(".lawn-orbit/profile.ron"),
            |project| project.config_dir().join("profile.ron"),
        );
        Self { path }
    }

    #[must_use]
    #[cfg(target_arch = "wasm32")]
    pub fn discover() -> Self {
        Self {
            // This is a diagnostic location, not a filesystem path on the web.
            path: PathBuf::from("localStorage").join(BROWSER_PROFILE_KEY),
        }
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load(&self) -> Result<Profile> {
        let source = match fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Profile::default()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read {}", self.path.display()));
            }
        };
        decode_profile(&source).with_context(|| format!("could not load {}", self.path.display()))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn load(&self) -> Result<Profile> {
        let storage = browser_storage()?;
        let source = read_browser_profile(&storage, self.browser_key()?)?;
        source.map_or_else(|| Ok(Profile::default()), |source| decode_profile(&source))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn save(&self, profile: &Profile) -> Result<()> {
        let storage = browser_storage()?;
        let key = self.browser_key()?;
        // A newer build or another browser tab may have written since load.
        // Refuse to replace an unreadable profile even if this session began
        // with a valid one. localStorage.setItem commits a whole string.
        if let Some(source) = read_browser_profile(&storage, key)? {
            decode_profile(&source).context("existing browser profile will be preserved")?;
        }
        storage
            .set_item(key, &encode_profile(profile)?)
            .map_err(|error| anyhow::anyhow!("could not save browser profile: {error:?}"))
    }

    #[cfg(target_arch = "wasm32")]
    fn browser_key(&self) -> Result<&str> {
        self.path
            .file_name()
            .and_then(|key| key.to_str())
            .context("browser profile has an invalid storage key")
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn save(&self, profile: &Profile) -> Result<()> {
        static NEXT_SAVE: AtomicU64 = AtomicU64::new(0);
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let data = encode_profile(profile)?;
        // Each writer owns its staging file. A shared profile.ron.tmp can be
        // truncated or renamed underneath another running copy of the game.
        let (temporary, mut file) = loop {
            let sequence = NEXT_SAVE.fetch_add(1, Ordering::Relaxed);
            let path = self
                .path
                .with_extension(format!("ron.{}.{sequence}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (TemporaryProfile(path), file),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("could not create {}", path.display()));
                }
            }
        };
        file.write_all(data.as_bytes())
            .with_context(|| format!("could not write {}", temporary.0.display()))?;
        file.sync_all().context("could not flush profile data")?;
        drop(file);
        fs::rename(&temporary.0, &self.path)
            .with_context(|| format!("could not commit {}", self.path.display()))?;
        #[cfg(unix)]
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::File::open(parent)?
                .sync_all()
                .context("could not flush profile directory")?;
        }
        Ok(())
    }
}

fn decode_profile(source: &str) -> Result<Profile> {
    let mut profile: Profile = ron::from_str(source).context("could not parse profile")?;
    if profile.version > PROFILE_VERSION {
        anyhow::bail!(
            "profile version {} is newer than supported version {}",
            profile.version,
            PROFILE_VERSION
        );
    }
    profile.sanitize();
    Ok(profile)
}

fn encode_profile(profile: &Profile) -> Result<String> {
    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(4)
        .separate_tuple_members(true)
        .enumerate_arrays(true);
    ron::ser::to_string_pretty(profile, pretty).context("could not serialize profile")
}

#[cfg(target_arch = "wasm32")]
const BROWSER_PROFILE_KEY: &str = "lawn-orbit.profile.ron";

#[cfg(target_arch = "wasm32")]
fn browser_storage() -> Result<web_sys::Storage> {
    web_sys::window()
        .context("browser window is unavailable")?
        .local_storage()
        .map_err(|error| anyhow::anyhow!("browser storage is unavailable: {error:?}"))?
        .context("browser storage is disabled")
}

#[cfg(target_arch = "wasm32")]
fn read_browser_profile(storage: &web_sys::Storage, key: &str) -> Result<Option<String>> {
    storage
        .get_item(key)
        .map_err(|error| anyhow::anyhow!("could not read browser profile: {error:?}"))
}

#[cfg(not(target_arch = "wasm32"))]
struct TemporaryProfile(PathBuf);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TemporaryProfile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn profile_is_atomically_saved_and_loaded() {
        let store = ProfileStore::temporary("roundtrip");
        let mut profile = Profile {
            tutorial_completed: true,
            ..Profile::default()
        };
        profile.settings.render_scale = 0.73;
        store.save(&profile).unwrap();
        assert_eq!(store.load().unwrap(), profile);
        assert!(!store.path.with_extension("ron.tmp").exists());
        std::fs::remove_file(&store.path).unwrap();
    }

    #[test]
    fn concurrent_saves_commit_complete_profiles() {
        let store = ProfileStore::temporary("concurrent");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for seed in 0..8 {
                let store = &store;
                let barrier = &barrier;
                scope.spawn(move || {
                    let mut profile = Profile::default();
                    profile.record_seed(
                        lawn_core::planet::CURRENT_GENERATOR_VERSION,
                        lawn_core::WorldSeed(seed),
                    );
                    barrier.wait();
                    store.save(&profile).unwrap();
                });
            }
        });
        let saved = store.load().unwrap();
        assert_eq!(saved.recent_seeds.len(), 1);
        assert!(saved.recent_seeds[0].seed.0 < 8);
        fs::remove_file(store.path()).unwrap();
    }

    #[test]
    fn failed_commit_removes_its_staging_file() {
        let store = ProfileStore::temporary("failed-commit");
        fs::create_dir(&store.path).unwrap();
        assert!(store.save(&Profile::default()).is_err());
        let prefix = store.path.file_stem().unwrap().to_string_lossy();
        let matching: Vec<_> = fs::read_dir(store.path.parent().unwrap())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(prefix.as_ref())
            })
            .collect();
        assert_eq!(matching.len(), 1, "temporary file must not survive failure");
        fs::remove_dir(&store.path).unwrap();
    }

    #[test]
    fn future_profile_version_is_rejected_without_overwrite() {
        let store = ProfileStore::temporary("future");
        let source = ron::to_string(&Profile {
            version: PROFILE_VERSION + 1,
            ..Profile::default()
        })
        .unwrap();
        std::fs::write(&store.path, &source).unwrap();
        assert!(store.load().is_err());
        assert_eq!(std::fs::read_to_string(&store.path).unwrap(), source);
        std::fs::remove_file(&store.path).unwrap();
    }

    #[test]
    fn profile_codec_preserves_records_and_sanitizes_settings() {
        let mut profile = Profile {
            tutorial_completed: true,
            ..Profile::default()
        };
        profile.settings.render_scale = 20.0;
        profile.record_seed(
            lawn_core::planet::CURRENT_GENERATOR_VERSION,
            lawn_core::WorldSeed(u64::MAX),
        );
        profile.favorite_seeds.insert((3, u64::MAX));
        let source = encode_profile(&profile).unwrap();
        profile.sanitize();
        assert_eq!(decode_profile(&source).unwrap(), profile);
    }

    #[test]
    fn profile_codec_rejects_corrupt_or_future_sources() {
        assert!(decode_profile("not valid RON").is_err());
        let future = encode_profile(&Profile {
            version: PROFILE_VERSION + 1,
            ..Profile::default()
        })
        .unwrap();
        assert!(decode_profile(&future).is_err());
        assert_eq!(decode_profile("()").unwrap(), Profile::default());
    }
}
