use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context, Result};
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
    pub fn discover() -> Self {
        let path = ProjectDirs::from("games", "OpenAI", "Lawn Orbit").map_or_else(
            || PathBuf::from(".lawn-orbit/profile.ron"),
            |project| project.config_dir().join("profile.ron"),
        );
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn load(&self) -> Result<Profile> {
        let source = match fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Profile::default()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("could not read {}", self.path.display()));
            }
        };
        let mut profile: Profile = ron::from_str(&source)
            .with_context(|| format!("could not parse {}", self.path.display()))?;
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
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true)
            .enumerate_arrays(true);
        let data =
            ron::ser::to_string_pretty(profile, pretty).context("could not serialize profile")?;
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

struct TemporaryProfile(PathBuf);

impl Drop for TemporaryProfile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
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
        std::fs::write(&store.path, source).unwrap();
        assert!(store.load().is_err());
        std::fs::remove_file(&store.path).unwrap();
    }
}
