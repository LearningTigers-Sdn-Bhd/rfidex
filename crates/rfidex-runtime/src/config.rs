//! Persisted application setup: server address, API key, and the stations this
//! PC runs. Everything is validated before it reaches the disk, and a save
//! replaces the file atomically so a crash cannot leave half a config.

use std::path::{Path, PathBuf};

use rfidex_core::contract::{Role, StationKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A message written for the operator.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub struct AppPaths {
    root: PathBuf,
    pub config_file: PathBuf,
}

impl AppPaths {
    pub fn new(root: PathBuf) -> Self {
        AppPaths {
            config_file: root.join("config.json"),
            root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn station_db(&self, id: Uuid) -> PathBuf {
        self.root.join("stations").join(format!("{id}.db"))
    }

    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }

    pub fn load(&self) -> Result<Option<AppConfig>, ConfigError> {
        let text = match std::fs::read_to_string(&self.config_file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let config: AppConfig = serde_json::from_str(&text)?;
        config.validate()?;
        Ok(Some(config))
    }

    pub fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        config.validate()?;
        std::fs::create_dir_all(&self.root)?;
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer_pretty(temp.as_file_mut(), config)?;
        std::io::Write::write_all(temp.as_file_mut(), b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(&self.config_file).map_err(|e| e.error)?;
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub server_url: String,
    pub api_key: String,
    pub stations: Vec<StationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StationConfig {
    pub id: Uuid,
    pub name: String,
    pub kind: StationKind,
    pub role: Option<Role>,
    pub device: DeviceChoice,
    #[serde(default = "default_debounce")]
    pub debounce_secs: u64,
    #[serde(default)]
    pub write_start_block: u8,
}

fn default_debounce() -> u64 {
    5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeviceChoice {
    SimDesk,
    SimGate {
        gate_kind: rfidex_core::device::GateKind,
        release_verified: bool,
    },
}

#[derive(Clone, Deserialize)]
pub struct SetupInput {
    pub server_url: String,
    /// Empty on edit keeps the saved key.
    pub api_key: String,
    pub stations: Vec<StationConfig>,
}

#[derive(Serialize)]
pub struct SetupView {
    pub server_url: String,
    pub has_api_key: bool,
    pub stations: Vec<StationConfig>,
}

impl AppConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_connection(&self.server_url, &self.api_key)?;
        validate_stations(&self.stations)
    }

    pub fn from_input(old: Option<&Self>, input: SetupInput) -> Result<Self, ConfigError> {
        let api_key = if input.api_key.trim().is_empty() {
            match old {
                Some(old) => old.api_key.clone(),
                None => {
                    return Err(ConfigError::Invalid(
                        "Enter the API key from the event.".into(),
                    ))
                }
            }
        } else {
            input.api_key
        };
        let mut stations = input.stations;
        for station in &mut stations {
            station.name = station.name.trim().to_string();
        }
        let config = AppConfig {
            server_url: input.server_url.trim().trim_end_matches('/').to_string(),
            api_key,
            stations,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn setup_view(&self) -> SetupView {
        SetupView {
            server_url: self.server_url.clone(),
            has_api_key: !self.api_key.is_empty(),
            stations: self.stations.clone(),
        }
    }
}

pub fn validate_connection(url: &str, key: &str) -> Result<(), ConfigError> {
    let parsed = reqwest::Url::parse(url.trim())
        .map_err(|_| ConfigError::Invalid("Enter the server address the event gave you.".into()))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ConfigError::Invalid(
            "The server address cannot contain a username or password.".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(ConfigError::Invalid(
            "The server address cannot contain a query or fragment.".into(),
        ));
    }
    let host = parsed.host_str().unwrap_or_default();
    match parsed.scheme() {
        "https" if !host.is_empty() => {}
        "http" if matches!(host, "localhost" | "127.0.0.1" | "[::1]") => {}
        _ => {
            return Err(ConfigError::Invalid(
                "Use https:// for the server address. Plain http:// works only on this computer."
                    .into(),
            ))
        }
    }
    if key.chars().count() <= 30 {
        return Err(ConfigError::Invalid(
            "The API key must be longer than 30 characters.".into(),
        ));
    }
    if key.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid(
            "The API key cannot contain spaces.".into(),
        ));
    }
    reqwest::header::HeaderValue::from_str(key).map_err(|_| {
        ConfigError::Invalid("The API key contains characters the server cannot read.".into())
    })?;
    Ok(())
}

fn validate_stations(stations: &[StationConfig]) -> Result<(), ConfigError> {
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for station in stations {
        if !ids.insert(station.id) {
            return Err(ConfigError::Invalid(
                "Two stations share the same id. Remove one and add it again.".into(),
            ));
        }
        let name = station.name.trim();
        if name.is_empty() {
            return Err(ConfigError::Invalid("Give every station a name.".into()));
        }
        if !names.insert(name.to_string()) {
            return Err(ConfigError::Invalid(
                "Every station needs its own name.".into(),
            ));
        }
        if !(1..=60).contains(&station.debounce_secs) {
            return Err(ConfigError::Invalid(
                "The wait between repeats must be between 1 and 60 seconds.".into(),
            ));
        }
        match (station.kind, station.role, &station.device) {
            (StationKind::Desk, None, DeviceChoice::SimDesk) => {}
            (StationKind::Gate, Some(_), DeviceChoice::SimGate { .. }) => {}
            (StationKind::Desk, Some(_), _) => {
                return Err(ConfigError::Invalid(
                    "A desk does not have an entry or exit direction.".into(),
                ))
            }
            (StationKind::Gate, None, _) => {
                return Err(ConfigError::Invalid(
                    "Choose whether this gate is for entry or exit.".into(),
                ))
            }
            _ => {
                return Err(ConfigError::Invalid(
                    "The chosen device does not match the station type.".into(),
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rfidex_core::device::GateKind;

    const KEY: &str = "rfidex_demo_key_0123456789abcdefghij";

    fn desk(id: u128, name: &str) -> StationConfig {
        StationConfig {
            id: Uuid::from_u128(id),
            name: name.into(),
            kind: StationKind::Desk,
            role: None,
            device: DeviceChoice::SimDesk,
            debounce_secs: 5,
            write_start_block: 0,
        }
    }

    fn gate(id: u128, name: &str) -> StationConfig {
        StationConfig {
            id: Uuid::from_u128(id),
            name: name.into(),
            kind: StationKind::Gate,
            role: Some(Role::Entry),
            device: DeviceChoice::SimGate {
                gate_kind: GateKind::Records,
                release_verified: false,
            },
            debounce_secs: 5,
            write_start_block: 0,
        }
    }

    fn config(url: &str, key: &str, stations: Vec<StationConfig>) -> AppConfig {
        AppConfig {
            server_url: url.into(),
            api_key: key.into(),
            stations,
        }
    }

    fn rejected(config: &AppConfig) -> String {
        match config.validate() {
            Err(ConfigError::Invalid(message)) => message,
            other => panic!("expected a readable rejection, got {other:?}"),
        }
    }

    fn input(url: &str, key: &str, stations: Vec<StationConfig>) -> SetupInput {
        SetupInput {
            server_url: url.into(),
            api_key: key.into(),
            stations,
        }
    }

    #[test]
    fn accepts_https_and_loopback_http() {
        for url in [
            "https://example.test",
            "https://example.test/api",
            "http://localhost:4010",
            "http://127.0.0.1:4010",
            "http://[::1]:4010",
        ] {
            let c = config(url, KEY, vec![desk(1, "Desk")]);
            assert!(c.validate().is_ok(), "{url} should be accepted");
        }
    }

    #[test]
    fn rejects_unsafe_or_malformed_addresses() {
        for url in [
            "http://example.test",
            "http://localhost.evil.test",
            "http://127.0.0.1.evil.test",
            "not a url",
            "",
            "ftp://example.test",
            "https://user:pass@example.test",
            "https://example.test?event=2",
            "https://example.test#frag",
        ] {
            let c = config(url, KEY, vec![desk(1, "Desk")]);
            assert!(config_url_rejected(&c), "{url} should be rejected");
        }
    }

    fn config_url_rejected(c: &AppConfig) -> bool {
        matches!(c.validate(), Err(ConfigError::Invalid(_)))
    }

    #[test]
    fn key_length_whitespace_and_header_safety() {
        let ok = "a".repeat(31);
        assert!(config("https://example.test", &ok, vec![])
            .validate()
            .is_ok());
        assert!(config("https://example.test", &"a".repeat(30), vec![])
            .validate()
            .is_err());
        for bad in ["short", &format!("{} ", "a".repeat(31))] {
            assert!(
                config("https://example.test", bad, vec![])
                    .validate()
                    .is_err(),
                "{bad:?} should be rejected"
            );
        }
        assert!(
            validate_connection("https://example.test", "xxxxxxxxxx\txxxxxxxxxx\txxx").is_err()
        );
        assert!(
            validate_connection(
                "https://example.test",
                "0123456789012345678901234567890\u{1}"
            )
            .is_err(),
            "a byte an HTTP header cannot carry must fail at setup, not at first request"
        );
    }

    #[test]
    fn station_identity_rules() {
        let mut c = config(
            "https://example.test",
            KEY,
            vec![desk(1, "Desk"), desk(1, "Other")],
        );
        assert!(rejected(&c).contains("id"));
        c = config(
            "https://example.test",
            KEY,
            vec![desk(1, "Desk"), desk(2, " Desk ")],
        );
        assert!(rejected(&c).contains("name"));
        c = config("https://example.test", KEY, vec![desk(1, "   ")]);
        assert!(rejected(&c).contains("name"));
    }

    #[test]
    fn station_kind_role_and_device_must_agree() {
        let mut gate_without_role = gate(2, "Entry");
        gate_without_role.role = None;
        assert!(rejected(&config(
            "https://example.test",
            KEY,
            vec![gate_without_role]
        ))
        .contains("entry or exit"));

        let mut desk_with_role = desk(1, "Desk");
        desk_with_role.role = Some(Role::Exit);
        assert!(
            rejected(&config("https://example.test", KEY, vec![desk_with_role]))
                .contains("direction")
        );

        let mismatched = StationConfig {
            device: DeviceChoice::SimGate {
                gate_kind: GateKind::Records,
                release_verified: false,
            },
            ..desk(1, "Desk")
        };
        assert!(
            rejected(&config("https://example.test", KEY, vec![mismatched])).contains("device")
        );

        assert!(config(
            "https://example.test",
            KEY,
            vec![desk(1, "Desk"), gate(2, "Entry")]
        )
        .validate()
        .is_ok());
        assert!(
            config("https://example.test", KEY, vec![])
                .validate()
                .is_ok(),
            "an empty station list is valid setup"
        );
    }

    #[test]
    fn debounce_bounds_and_default() {
        let mut c = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        c.stations[0].debounce_secs = 0;
        assert!(rejected(&c).contains("1 and 60"));
        c.stations[0].debounce_secs = 61;
        assert!(rejected(&c).contains("1 and 60"));
        for ok in [1, 60] {
            c.stations[0].debounce_secs = ok;
            assert!(c.validate().is_ok());
        }

        let omitted: StationConfig = serde_json::from_str(
            r#"{"id":"00000000-0000-0000-0000-000000000001","name":"Desk",
                "kind":"desk","role":null,"device":{"type":"sim_desk"}}"#,
        )
        .unwrap();
        assert_eq!(omitted.debounce_secs, 5);
        assert_eq!(omitted.write_start_block, 0);
    }

    #[test]
    fn blank_key_on_edit_keeps_the_saved_one() {
        let saved = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        let edited = AppConfig::from_input(
            Some(&saved),
            input("https://other.test", "   ", vec![desk(1, "Desk")]),
        )
        .unwrap();
        assert_eq!(edited.api_key, KEY);
        assert_eq!(edited.server_url, "https://other.test");

        let fresh = AppConfig::from_input(None, input("https://example.test", "  ", vec![]));
        assert!(matches!(fresh, Err(ConfigError::Invalid(message)) if message.contains("API key")));
    }

    #[test]
    fn from_input_trims_and_rejects_like_validate() {
        let c = AppConfig::from_input(
            None,
            input("https://example.test/", KEY, vec![desk(1, " Desk ")]),
        )
        .unwrap();
        assert_eq!(c.server_url, "https://example.test");
        assert_eq!(c.stations[0].name, "Desk");

        assert!(AppConfig::from_input(None, input("http://example.test", KEY, vec![])).is_err());
    }

    #[test]
    fn setup_view_never_returns_the_key() {
        let c = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        let view = c.setup_view();
        assert_eq!(view.server_url, "https://example.test");
        assert!(view.has_api_key);
        assert_eq!(view.stations.len(), 1);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains(KEY), "the saved key must not leave Rust");
        assert!(
            !json.contains("\"api_key\""),
            "the key field is replaced by a yes/no flag"
        );

        let none = config("https://example.test", "", vec![]).setup_view();
        assert!(!none.has_api_key);
    }

    #[test]
    fn file_round_trip_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().to_path_buf());
        assert!(paths.load().unwrap().is_none());

        let first = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        paths.save(&first).unwrap();
        let loaded = paths.load().unwrap().unwrap();
        assert_eq!(loaded.server_url, first.server_url);
        assert_eq!(loaded.api_key, KEY);
        assert_eq!(loaded.stations.len(), 1);

        let second = config(
            "https://other.test",
            &"b".repeat(31),
            vec![gate(2, "Entry")],
        );
        paths.save(&second).unwrap();
        let loaded = paths.load().unwrap().unwrap();
        assert_eq!(loaded.server_url, "https://other.test");
        assert_eq!(loaded.api_key, "b".repeat(31));
        assert_eq!(loaded.stations[0].name, "Entry");
        assert_eq!(
            loaded.stations[0].id,
            Uuid::from_u128(2),
            "station UUIDs survive an edit"
        );
    }

    #[test]
    fn failed_save_preserves_the_previous_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().to_path_buf());
        let good = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        paths.save(&good).unwrap();

        let bad = config("http://example.test", KEY, vec![desk(1, "Desk")]);
        assert!(paths.save(&bad).is_err());
        let loaded = paths.load().unwrap().unwrap();
        assert_eq!(loaded.server_url, "https://example.test");
        assert_eq!(
            paths.station_db(Uuid::from_u128(1)).file_name().unwrap(),
            "00000000-0000-0000-0000-000000000001.db"
        );
        assert_eq!(paths.exports(), dir.path().join("exports"));
    }

    #[test]
    fn corrupt_config_is_an_error_not_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().to_path_buf());
        std::fs::write(&paths.config_file, "{ not json").unwrap();
        assert!(
            paths.load().is_err(),
            "a corrupt config must not look like first run"
        );

        let unknown_state = config("https://example.test", KEY, vec![desk(1, "Desk")]);
        paths.save(&unknown_state).unwrap();
        let text = std::fs::read_to_string(&paths.config_file).unwrap();
        std::fs::write(
            &paths.config_file,
            text.replace("https://example.test", "http://example.test"),
        )
        .unwrap();
        assert!(
            paths.load().is_err(),
            "a config that no longer validates is reported, not silently reloaded"
        );
    }
}
