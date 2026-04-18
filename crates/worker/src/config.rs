use figment::{
    Figment,
    providers::{Format, Yaml},
};

use notify_debouncer_mini::{DebouncedEvent, new_debouncer, notify::RecursiveMode};
use serde::{Deserialize, Serialize};
use std::{
    fmt::Display,
    fs::{File, exists},
    path::Path,
    process::exit,
    sync::{Arc, RwLock},
};

use log::{error, info};

const CONFIG_PATH: &str = "./config.yaml";

fn load_config() -> Config {
    if !exists(CONFIG_PATH).unwrap_or_else(|e| {
        error!("Failed to verify the existence of the file '{CONFIG_PATH}': {e}");
        exit(1)
    }) {
        let file = File::create_new(CONFIG_PATH).unwrap_or_else(|e| {
            error!("Failed to create file '{CONFIG_PATH}': {e}");
            exit(1)
        });
        // [ ] TODO: Creating minimal work config
    }
    Figment::new()
        .merge(Yaml::file(CONFIG_PATH))
        .extract()
        .unwrap_or_else(|e| {
            error!("YAML parsing error in '{CONFIG_PATH}': {e}");
            exit(1)
        })
}

pub fn watch_config() -> Arc<RwLock<Config>> {
    let config = Arc::new(RwLock::new(load_config()));
    let config_clone = Arc::clone(&config);

    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<DebouncedEvent>, _>>();

        let mut debouncer = new_debouncer(std::time::Duration::from_millis(500), tx).unwrap();
        debouncer
            .watcher()
            .watch(Path::new(CONFIG_PATH), RecursiveMode::NonRecursive)
            .unwrap();

        info!("Watching '{CONFIG_PATH}' for changes...");

        for events in rx {
            match events {
                Ok(_) => {
                    match Figment::new()
                        .merge(Yaml::file(CONFIG_PATH))
                        .extract::<Config>()
                    {
                        Ok(new_config) => {
                            *config_clone.write().unwrap() = new_config;
                            info!("Config reloaded!");
                        }
                        Err(e) => error!("Error reloading config: {e}"),
                    }
                }
                Err(e) => error!("Watch error: {e:?}"),
            }
        }
    });

    config
}

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct Id(String);

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Agent {
    pub id: Id,
    pub master_url: String,
    pub auth_key: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

fn default_enable_label_filter() -> bool {
    false
}

fn default_socket_path() -> String {
    "/var/run/docker.sock".to_string()
}

#[derive(Serialize, Deserialize, Default)]
pub struct Docker {
    #[serde(default = "default_enable_label_filter")]
    pub enable_label_filter: bool,
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Protected,
    Private,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "lowercase")]
pub enum CheckType {
    Tcp,
    Http,
    Exec,
    // Grpc,
}

fn default_status_code() -> u16 {
    200u16
}

fn default_exit_code() -> i32 {
    0i32
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ExpectValue {
    #[serde(default = "default_status_code")]
    pub status_code: u16,
    #[serde(default = "default_exit_code")]
    pub exit_code: i32,
}

fn default_interval() -> String {
    String::from("30s")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Check {
    pub r#type: CheckType,
    pub url: String,
    #[serde(default = "default_interval")]
    pub interval: String,
    #[serde(default)]
    pub expect: ExpectValue,
}

fn default_visibility() -> Visibility {
    Visibility::Public
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Application {
    pub id: Id,
    name: Option<String>,
    #[serde(default = "default_visibility")]
    pub visibility: Visibility,
    #[serde(default)]
    pub required_tags: Option<Vec<Id>>,
    pub check: Check,
}

impl Application {
    pub fn name(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.id.0.clone())
    }
}

fn default_tls_warn_days() -> i64 {
    3i64
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub agent: Agent,
    #[serde(default)]
    pub docker: Docker,
    // [ ] TODO: Add `tls_warn_days` for every app
    #[serde(default = "default_tls_warn_days")]
    pub tls_warn_days: i64,
    pub applications: Vec<Application>,
}
