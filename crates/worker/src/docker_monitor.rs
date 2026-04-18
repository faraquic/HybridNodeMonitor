use bollard::{
    Docker,
    models::{ContainerStateStatusEnum, Health, HealthStatusEnum},
    plugin::ContainerStatsResponse,
    query_parameters::{ListContainersOptions, StatsOptions},
};
use futures_util::StreamExt;
use log::{error, info};
use std::{
    collections::HashMap,
    process::exit,
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::config::Docker as DockerCfg;

static DOCKER_SOCKET_TIMEOUT: u64 = 60 * 2;

#[derive(Debug, Clone)]
pub struct ContainerMetrics {
    pub id: String,
    pub name: String,
    pub status: ContainerStateStatusEnum,
    pub health: HealthStatusEnum,
    pub cpu_percentage: f64,
    pub mem_usage: u64,
    pub mem_limit: u64,
}

pub struct DockerMonitor(Arc<RwLock<Vec<ContainerMetrics>>>);

impl DockerMonitor {
    // [ ] TODO: Сделать docker не объязательным
    pub fn new(dkr_cfg: &DockerCfg) -> Self {
        let docker = Docker::connect_with_unix(
            dkr_cfg.socket_path.as_str(),
            DOCKER_SOCKET_TIMEOUT,
            bollard::API_DEFAULT_VERSION,
        )
        .unwrap_or_else(|_| {
            error!("Failed to connect to Docker socket");
            exit(1)
        });
        let metrics = Arc::new(RwLock::new(Vec::new()));

        let docker_clone = docker.clone();
        let metrics_ptr = metrics.clone();
        // We clone the flag so as not to pass &self into the static stream
        let use_filter = dkr_cfg.enable_label_filter;

        tokio::spawn(async move {
            info!("Docker monitoring task started");
            loop {
                let mut current_metrics = Vec::new();

                let mut filters = HashMap::new();
                if use_filter {
                    filters.insert("label".to_string(), vec!["hnm.enable=true".to_string()]);
                }

                let options = Some(ListContainersOptions {
                    all: true,
                    filters: Some(filters),
                    ..Default::default()
                });

                if let Ok(containers) = docker_clone.list_containers(options).await {
                    for container in containers {
                        let id: String = container.id.unwrap_or_default();
                        let name = container
                            .names
                            .and_then(|n| n.get(0).cloned())
                            .unwrap_or_default()[1..]
                            .to_string();

                        let stats_options = Some(StatsOptions {
                            stream: false,
                            one_shot: true,
                        });
                        let mut stats_stream = docker_clone.stats(&id, stats_options).take(1);

                        if let Some(Ok(stats)) = stats_stream.next().await {
                            let cpu_pct = calculate_cpu_percentage(&stats);

                            let (mem_usage, mem_limit) = if let Some(mem) = stats.memory_stats {
                                let usage = mem.usage.unwrap_or(0);
                                let limit = mem.limit.unwrap_or(1);
                                (usage, limit)
                            } else {
                                (0, 1)
                            };

                            let container_state =
                                docker.inspect_container(&id, None).await.unwrap().state;
                            let status = container_state.clone().unwrap().status.unwrap();
                            let health = container_state
                                .unwrap()
                                .health
                                .unwrap_or_else(|| Health {
                                    status: Some(HealthStatusEnum::NONE),
                                    failing_streak: None,
                                    log: None,
                                })
                                .status
                                .unwrap();

                            current_metrics.push(ContainerMetrics {
                                id,
                                name,
                                status,
                                health,
                                cpu_percentage: cpu_pct,
                                mem_usage,
                                mem_limit,
                            });
                        }
                    }
                }

                {
                    let mut w = metrics_ptr.write().unwrap();
                    *w = current_metrics;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Self(metrics)
    }

    pub fn get_metrics(&self) -> Vec<ContainerMetrics> {
        self.0.read().unwrap().clone()
    }
}

fn calculate_cpu_percentage(stats: &ContainerStatsResponse) -> f64 {
    // Используем .as_ref() для Option полей, чтобы не перемещать их
    let cpu_stats = &stats.cpu_stats;
    let precpu_stats = &stats.precpu_stats;

    if let (Some(cpu), Some(pre)) = (cpu_stats, precpu_stats) {
        let cpu_delta = (<std::option::Option<bollard::plugin::ContainerCpuUsage> as Clone>::clone(
            &cpu.cpu_usage,
        )
        .unwrap()
        .total_usage
        .unwrap() as f64)
            - (<std::option::Option<bollard::plugin::ContainerCpuUsage> as Clone>::clone(
                &pre.cpu_usage,
            )
            .unwrap()
            .total_usage
            .unwrap() as f64);
        let system_delta =
            (cpu.system_cpu_usage.unwrap_or(0) as f64) - (pre.system_cpu_usage.unwrap_or(0) as f64);

        let online_cpus = cpu.online_cpus.unwrap_or(1) as f64;

        if system_delta > 0.0 && cpu_delta > 0.0 {
            return (cpu_delta / system_delta) * online_cpus * 100.0;
        }
    }
    0.0
}
