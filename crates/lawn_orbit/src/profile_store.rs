use std::{fs, path::PathBuf};

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
        if !self.path.exists() {
            return Ok(Profile::default());
        }
        let source = fs::read_to_string(&self.path)
            .with_context(|| format!("could not read {}", self.path.display()))?;
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
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(4)
            .separate_tuple_members(true)
            .enumerate_arrays(true);
        let data =
            ron::ser::to_string_pretty(profile, pretty).context("could not serialize profile")?;
        let temporary = self.path.with_extension("ron.tmp");
        fs::write(&temporary, data)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        fs::rename(&temporary, &self.path)
            .with_context(|| format!("could not commit {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_store(name: &str) -> ProfileStore {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        ProfileStore {
            path: std::env::temp_dir().join(format!(
                "lawn-orbit-{name}-{}-{nonce}.ron",
                std::process::id()
            )),
        }
    }

    #[test]
    fn profile_is_atomically_saved_and_loaded() {
        let store = temporary_store("roundtrip");
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
    fn future_profile_version_is_rejected_without_overwrite() {
        let store = temporary_store("future");
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
