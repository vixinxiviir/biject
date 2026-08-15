use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// Used for both the config directory and the OS keychain service name.
// Changed from "datadiff" in 0.3.0 — see MIGRATING.md; pre-0.3.0 profiles and
// saved passwords are not read from the old location.
const APP_NAME: &str = "biject";
const PROFILES_FILE: &str = "profiles.json";

/// Connection profile metadata (no password — stored in OS keychain).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub name: String,
    pub db_type: String,
    pub host: String,
    pub port: Option<u16>,
    pub database: String,
    pub username: String,
}

#[derive(Debug)]
pub enum ProfileError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Keyring(String),
    NotFound(String),
    DuplicateName(String),
}

impl std::fmt::Display for ProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileError::Io(e) => write!(f, "IO error: {}", e),
            ProfileError::Json(e) => write!(f, "JSON error: {}", e),
            ProfileError::Keyring(m) => write!(f, "Keychain error: {}", m),
            ProfileError::NotFound(n) => write!(f, "Profile not found: {}", n),
            ProfileError::DuplicateName(n) => write!(f, "A profile named '{}' already exists", n),
        }
    }
}

impl From<std::io::Error> for ProfileError {
    fn from(e: std::io::Error) -> Self { ProfileError::Io(e) }
}

impl From<serde_json::Error> for ProfileError {
    fn from(e: serde_json::Error) -> Self { ProfileError::Json(e) }
}

fn profiles_path() -> Result<PathBuf, ProfileError> {
    let base = dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| ProfileError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Cannot locate app data directory",
        )))?;
    let dir = base.join(APP_NAME);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(PROFILES_FILE))
}

fn read_profiles() -> Result<Vec<ConnectionProfile>, ProfileError> {
    let path = profiles_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

fn write_profiles(profiles: &[ConnectionProfile]) -> Result<(), ProfileError> {
    let path = profiles_path()?;
    let data = serde_json::to_string_pretty(profiles)?;
    fs::write(&path, data)?;
    Ok(())
}

fn keyring_entry(profile_name: &str) -> Result<keyring::Entry, ProfileError> {
    keyring::Entry::new(APP_NAME, profile_name)
        .map_err(|e| ProfileError::Keyring(e.to_string()))
}

/// Return all saved profiles (no passwords).
pub fn list_profiles() -> Result<Vec<ConnectionProfile>, ProfileError> {
    read_profiles()
}

/// Save a new profile. Password goes to OS keychain; metadata to disk.
/// Returns an error if a profile with the same name already exists.
pub fn save_profile(profile: ConnectionProfile, password: &str) -> Result<(), ProfileError> {
    let mut profiles = read_profiles()?;
    if profiles.iter().any(|p| p.name == profile.name) {
        return Err(ProfileError::DuplicateName(profile.name));
    }
    // Store password in OS keychain first — if this fails we don't write disk
    let entry = keyring_entry(&profile.name)?;
    entry.set_password(password)
        .map_err(|e| ProfileError::Keyring(e.to_string()))?;
    profiles.push(profile);
    write_profiles(&profiles)
}

/// Overwrite an existing profile (by name). Updates keychain password if provided.
pub fn update_profile(profile: ConnectionProfile, password: Option<&str>) -> Result<(), ProfileError> {
    let mut profiles = read_profiles()?;
    let pos = profiles.iter().position(|p| p.name == profile.name)
        .ok_or_else(|| ProfileError::NotFound(profile.name.clone()))?;
    if let Some(pwd) = password {
        let entry = keyring_entry(&profile.name)?;
        entry.set_password(pwd)
            .map_err(|e| ProfileError::Keyring(e.to_string()))?;
    }
    profiles[pos] = profile;
    write_profiles(&profiles)
}

/// Delete a profile and remove its keychain entry.
pub fn delete_profile(name: &str) -> Result<(), ProfileError> {
    let mut profiles = read_profiles()?;
    let pos = profiles.iter().position(|p| p.name == name)
        .ok_or_else(|| ProfileError::NotFound(name.to_string()))?;
    // Remove from keychain (best-effort)
    if let Ok(entry) = keyring_entry(name) {
        let _ = entry.delete_credential();
    }
    profiles.remove(pos);
    write_profiles(&profiles)
}

/// Retrieve the password for a profile from the OS keychain.
pub fn get_password(profile_name: &str) -> Result<String, ProfileError> {
    let entry = keyring_entry(profile_name)?;
    entry.get_password()
        .map_err(|e| ProfileError::Keyring(e.to_string()))
}
