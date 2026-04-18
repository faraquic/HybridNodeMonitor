use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
    thread,
    time::{Duration, Instant},
};

use log::{debug, error, info, trace};
use sysinfo::{Components, Disks, Networks, System};

#[derive(Clone, Debug)]
struct StaticStat {
    name: Option<String>,
    kernel_version: Option<String>,
    os_version: Option<String>,
    host_name: Option<String>,
    boot_time: u64,
}

#[derive(Clone, Debug)]
struct MemStat {
    total: u64,
    used: u64,
}

#[derive(Clone, Debug)]
struct NetworkStat {
    rx_pkts_per_sec: f64,
    tx_pkts_per_sec: f64,
    rx_bytes_per_sec: f64,
    tx_bytes_per_sec: f64,
}

#[derive(Clone, Debug)]
struct ComponentStat {
    temperature: Option<f32>,
    critical: Option<f32>,
}

#[derive(Clone, Debug)]
struct DynamicStat {
    cpus: Vec<f32>,
    ram: MemStat,
    swap: MemStat,
    disks: Vec<(Option<String>, MemStat)>,
    network: NetworkStat,
    components: HashMap<String, ComponentStat>,
}

pub struct SystemStatusManager {
    static_stat: StaticStat,
    dynamic_stat: Arc<RwLock<DynamicStat>>,
}

#[derive(Debug)]
pub struct SystemStatusResult(StaticStat, DynamicStat);

impl SystemStatusManager {
    pub fn new() -> Self {
        info!("Initializing SystemStatusManager");

        let mut sys = System::new_all();
        let mut disks = Disks::new_with_refreshed_list();
        let mut networks = Networks::new_with_refreshed_list();
        let mut components = Components::new_with_refreshed_list();

        let static_stat = Self::collect_static();

        Self::refresh_all(&mut sys, &mut disks, &mut networks, &mut components);

        let initial =
            Self::collect_dynamic(&sys, &disks, &networks, &components, Duration::from_secs(1));

        let shared = Arc::new(RwLock::new(initial));

        Self::spawn_updater(Arc::clone(&shared), sys, disks, networks, components);

        info!("SystemStatusManager initialized");

        Self {
            static_stat,
            dynamic_stat: shared,
        }
    }

    pub fn get_system_status(&self) -> SystemStatusResult {
        SystemStatusResult(
            self.static_stat.clone(),
            self.dynamic_stat.read().unwrap().clone(),
        )
    }

    fn collect_static() -> StaticStat {
        debug!("Collecting static system info");

        StaticStat {
            name: System::name(),
            kernel_version: System::kernel_version(),
            os_version: System::os_version(),
            host_name: System::host_name(),
            boot_time: System::boot_time(),
        }
    }

    fn refresh_all(
        sys: &mut System,
        disks: &mut Disks,
        networks: &mut Networks,
        components: &mut Components,
    ) {
        trace!("Refreshing system data");

        sys.refresh_all();
        sys.refresh_cpu_usage();
        disks.refresh(true);
        networks.refresh(true);
        components.refresh(true);
    }

    fn collect_dynamic(
        sys: &System,
        disks: &Disks,
        networks: &Networks,
        components: &Components,
        elapsed: Duration,
    ) -> DynamicStat {
        debug!("Collecting dynamic stats");

        DynamicStat {
            cpus: Self::collect_cpu(sys),
            ram: Self::collect_ram(sys),
            swap: Self::collect_swap(sys),
            disks: Self::collect_disks(disks),
            network: Self::collect_network(networks, elapsed),
            components: Self::collect_components(components),
        }
    }

    fn collect_cpu(sys: &System) -> Vec<f32> {
        let cpus: Vec<f32> = sys.cpus().iter().map(|c| c.cpu_usage()).collect();
        trace!("CPU usage collected: {:?}", cpus);
        cpus
    }

    fn collect_ram(sys: &System) -> MemStat {
        let stat = MemStat {
            total: sys.total_memory(),
            used: sys.used_memory(),
        };
        trace!("RAM: {:?}", stat);
        stat
    }

    fn collect_swap(sys: &System) -> MemStat {
        let stat = MemStat {
            total: sys.total_swap(),
            used: sys.used_swap(),
        };
        trace!("Swap: {:?}", stat);
        stat
    }

    fn collect_disks(disks: &Disks) -> Vec<(Option<String>, MemStat)> {
        disks
            .iter()
            .map(|d| {
                let total = d.total_space();
                let stat = MemStat {
                    total,
                    used: total - d.available_space(),
                };
                (d.name().to_str().map(str::to_string), stat)
            })
            .collect()
    }

    fn collect_network(networks: &Networks, elapsed: Duration) -> NetworkStat {
        let mut rx_pkts = 0;
        let mut tx_pkts = 0;
        let mut rx_bytes = 0;
        let mut tx_bytes = 0;

        for (_, data) in networks.iter() {
            rx_pkts += data.packets_received();
            tx_pkts += data.packets_transmitted();
            rx_bytes += data.received();
            tx_bytes += data.transmitted();
        }

        let secs = elapsed.as_secs_f64().max(f64::EPSILON);

        let stat = NetworkStat {
            rx_pkts_per_sec: rx_pkts as f64 / secs,
            tx_pkts_per_sec: tx_pkts as f64 / secs,
            rx_bytes_per_sec: rx_bytes as f64 / secs,
            tx_bytes_per_sec: tx_bytes as f64 / secs,
        };

        trace!("Network: {:?}", stat);
        stat
    }

    fn collect_components(components: &Components) -> HashMap<String, ComponentStat> {
        components
            .iter()
            .filter(|c| {
                let l = c.label();
                l.starts_with("Core") || l.contains("Package")
            })
            .map(|c| {
                (
                    c.label().to_string(),
                    ComponentStat {
                        temperature: c.temperature(),
                        critical: c.critical(),
                    },
                )
            })
            .collect()
    }

    fn spawn_updater(
        shared: Arc<RwLock<DynamicStat>>,
        mut sys: System,
        mut disks: Disks,
        mut networks: Networks,
        mut components: Components,
    ) {
        thread::spawn(move || {
            info!("Background updater thread started");

            let mut last_tick = Instant::now();

            loop {
                if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    Self::refresh_all(&mut sys, &mut disks, &mut networks, &mut components);

                    let elapsed = last_tick.elapsed();
                    last_tick = Instant::now();

                    let new_data =
                        Self::collect_dynamic(&sys, &disks, &networks, &components, elapsed);

                    let mut guard = shared.write().unwrap();
                    *guard = new_data;

                    debug!("Dynamic stats updated");
                })) {
                    error!("Updater thread panicked: {:?}", e);
                }

                thread::sleep(Duration::from_secs(1));
            }
        });
    }
}
