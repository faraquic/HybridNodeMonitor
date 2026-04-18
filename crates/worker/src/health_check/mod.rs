use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};

use log::{debug, error, info, trace, warn};
use tokio::task::JoinSet;

use crate::config::{Application, CheckType, Config, Id};

mod check;

#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub id: Id,
    pub app_name: String,
    pub is_available: bool,
    pub status_code: Option<u16>,
    pub error: Option<String>,
    pub cert_warning: Option<String>,
}

struct IntervalState {
    pub current: Duration,
    base: Duration,
}

impl IntervalState {
    fn new(base: Duration) -> Self {
        trace!("Creating new IntervalState with base={base:?}");
        Self {
            current: base,
            base,
        }
    }

    fn decrease(&mut self) {
        self.current = self.current.saturating_sub(Duration::from_secs(1));
        trace!("IntervalState decreased: current={:?}", self.current);
    }

    fn reset(&mut self) {
        trace!("IntervalState reset: {:?} -> {:?}", self.current, self.base);
        self.current = self.base;
    }

    fn current(&self) -> Duration {
        self.current
    }
}

type TAS = Arc<RwLock<HashMap<Id, (IntervalState, Option<HealthCheckResult>)>>>;

pub struct HealthCheckManager {
    table_application_status: TAS,
    // config: &'c Arc<RwLock<Config>>,
}

impl HealthCheckManager {
    pub fn new(config: &Arc<RwLock<Config>>) -> Self {
        info!("Initializing HealthCheckManager");

        let table_application_status = Arc::new(RwLock::new({
            let apps: Vec<(Id, String)> = {
                debug!("Reading application list from config");
                let cfg = config.read().unwrap();
                let apps: Vec<_> = cfg
                    .applications
                    .iter()
                    .map(|app| (app.id.clone(), app.check.interval.clone()))
                    .collect();
                debug!("Loaded {} application(s) from config", apps.len());
                apps
            };

            let mut hm = HashMap::with_capacity(apps.len());
            for (id, interval_str) in apps {
                match humantime::parse_duration(&interval_str.as_str()) {
                    Ok(duration) => {
                        debug!("Registering app id={id:?} with interval={duration:?}");
                        hm.insert(id, (IntervalState::new(duration), None));
                    }
                    Err(e) => {
                        error!("Failed to parse interval {interval_str:?} for app id={id:?}: {e}");
                    }
                }
            }

            info!("Initialized status table with {} entry(ies)", hm.len());
            hm
        }));

        let timeout: f32 = 5.0;

        // [ ] TODO: Move timeout to master conf
        let http_client = match reqwest::Client::builder()
            .timeout(Duration::from_secs_f32(timeout))
            .build()
        {
            Ok(c) => {
                debug!("Shared HTTP client built successfully (timeout={timeout}s)");
                Arc::new(c)
            }
            Err(e) => {
                error!("Failed to build shared HTTP client: {e}");
                panic!("Cannot initialize without HTTP client");
            }
        };

        let tas_clone = Arc::clone(&table_application_status);
        let config_for_precheck = Arc::clone(config);
        let config_for_thread = Arc::clone(config);
        let http_client_for_thread = Arc::clone(&http_client);
        let initial_apps: Vec<Application> = {
            debug!("Precheck: reading initial application list for first health check round");
            let cfg = config_for_precheck.read().unwrap();
            let apps = cfg.applications.clone();
            debug!(
                "Precheck: {} application(s) queued for initial check",
                apps.len()
            );
            apps
        };

        let rt_handle = tokio::runtime::Handle::current();

        if initial_apps.is_empty() {
            warn!("Precheck: no applications configured, skipping initial health check");
        } else {
            info!(
                "Precheck: scheduling initial health check for {} app(s): {:?}",
                initial_apps.len(),
                initial_apps.iter().map(|a| a.name()).collect::<Vec<_>>()
            );
            let tas_for_precheck = Arc::clone(&table_application_status);
            let client_for_precheck = Arc::clone(&http_client);

            rt_handle.spawn(async move {
                Self::health_check(
                    Arc::clone(&config_for_precheck),
                    tas_for_precheck,
                    initial_apps,
                    client_for_precheck,
                )
                .await;
            });
        }

        info!("Spawning health check scheduler thread");
        std::thread::spawn(move || {
            info!("Scheduler thread started");
            loop {
                trace!("Scheduler tick: decrementing intervals");

                let apps_to_check: Vec<Application> = {
                    let ids_to_check: HashSet<Id> = {
                        let mut tas = tas_clone.write().unwrap();
                        let due: HashSet<Id> = tas
                            .iter_mut()
                            .filter_map(|(id, v)| {
                                v.0.decrease();
                                if v.0.current() == Duration::ZERO {
                                    debug!(
                                        "App id={id:?} is due for health check, resetting interval"
                                    );
                                    v.0.reset();
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                            .collect();

                        if due.is_empty() {
                            trace!("No apps due for check this tick");
                        } else {
                            debug!("{} app(s) due: {due:?}", due.len());
                        }
                        due
                    };

                    let apps: Vec<Application> = config_for_thread
                        .read()
                        .unwrap()
                        .applications
                        .iter()
                        .filter(|app| ids_to_check.contains(&app.id))
                        .cloned()
                        .collect();

                    debug!("Resolved {} app(s) to check by name", apps.len());
                    apps
                };

                if !apps_to_check.is_empty() {
                    debug!(
                        "Dispatching health checks for: {:?}",
                        apps_to_check.iter().map(|a| a.name()).collect::<Vec<_>>()
                    );

                    let tas = Arc::clone(&tas_clone);
                    let client = Arc::clone(&http_client_for_thread);
                    let config = Arc::clone(&config_for_thread);

                    rt_handle.spawn(async move {
                        Self::health_check(config, tas, apps_to_check, client).await;
                    });
                }

                thread::sleep(Duration::from_secs_f32(1f32));
            }
        });

        info!("HealthCheckManager initialized successfully");
        Self {
            table_application_status,
        }
    }

    pub fn get_health_check_results(&self) -> Vec<HealthCheckResult> {
        self.table_application_status
            .read()
            .unwrap()
            .values()
            .filter_map(|v| v.1.clone())
            .collect()
    }

    async fn health_check(
        config: Arc<RwLock<Config>>,
        tas: TAS,
        apps: Vec<Application>,
        http_client: Arc<reqwest::Client>,
    ) {
        info!("Starting health check round for {} app(s)", apps.len());
        debug!(
            "Apps to check: {:?}",
            apps.iter().map(|a| a.name()).collect::<Vec<_>>()
        );

        let mut set: JoinSet<HealthCheckResult> = JoinSet::new();

        for app in apps {
            let app = app.clone();
            let client = Arc::clone(&http_client);

            match app.check.r#type {
                CheckType::Http => {
                    info!(
                        "Scheduling HTTP check for '{}' -> {}",
                        app.name(),
                        app.check.url
                    );
                    let config = Arc::clone(&config);
                    set.spawn(async move { check::http(config, client, app).await });
                }
                CheckType::Tcp => {
                    info!(
                        "Scheduling TCP check for '{}' -> {}",
                        app.name(),
                        app.check.url
                    );
                    set.spawn(async move { check::tcp(app).await });
                }
                CheckType::Exec => {
                    info!("Scheduling Exec check for '{}'", app.name());
                    set.spawn(async move { check::exec(app).await });
                } // [ ] TODO: Add gRPC health check
                  // CheckType::Grpc => {
                  //     info!(
                  //         "Scheduling gRPC check for '{}' -> {}",
                  //         app.name(),
                  //         app.check.url
                  //     );
                  //     set.spawn(async move { check::grpc(app).await });
                  // }
            }
        }

        debug!("All check tasks spawned, collecting results...");

        while let Some(res) = set.join_next().await {
            match res {
                Ok(result) => {
                    if let Some(ref cert_warn) = result.cert_warning {
                        warn!("'{}' TLS cert warning: {cert_warn}", result.app_name);
                    }
                    if result.is_available {
                        info!(
                            "'{}' is UP (status={})",
                            result.app_name,
                            result.status_code.map_or("N/A".into(), |c| c.to_string())
                        );
                    } else {
                        warn!(
                            "'{}' is DOWN (status={}, error={:?})",
                            result.app_name,
                            result.status_code.map_or("N/A".into(), |c| c.to_string()),
                            result.error
                        );
                    }

                    debug!("Writing result for '{}' into status table", result.id);
                    if let Some(entry) = tas.write().unwrap().get_mut(&result.id) {
                        entry.1 = Some(result);
                    } else {
                        warn!("No status table entry found for id={:?}", result.id);
                    }
                }
                Err(e) => {
                    error!("Health check task panicked: {e}");
                }
            }
        }

        info!("Health check round complete");
    }
}
