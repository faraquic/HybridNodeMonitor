use log::info;

mod config;
mod docker_monitor;
mod health_check;
mod system_status;

#[tokio::main]
async fn main() {
    env_logger::init();

    let config = config::watch_config();

    let cfg = config.read().unwrap();
    info!("Worker ID: '{}'", cfg.agent.id);

    let ssm = system_status::SystemStatusManager::new();
    let hcm = health_check::HealthCheckManager::new(&config);
    let dm = docker_monitor::DockerMonitor::new(&cfg.docker);

    drop(cfg);

    std::thread::sleep(std::time::Duration::from_secs_f32(1f32));
    log::debug!(
        "{:#?}\n{:#?}\n{:#?}",
        dm.get_metrics(),
        ssm.get_system_status(),
        hcm.get_health_check_results()
    );
}
