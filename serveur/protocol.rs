use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const DEFAULT_USERNAME: &str = "admin";
pub const DEFAULT_PASSWORD: &str = "root";
pub const DEFAULT_PORT: u16 = 9050;
pub const MAX_FRAME_BYTES: usize = 1_048_576;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Auth { username: String, password: String },
    Metrics { metrics: MetricsPayload },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsPayload {
    pub schema_version: u32,
    pub timestamp_unix_seconds: u64,
    pub hostname: String,
    pub cpu_percent: Option<f64>,
    pub cpu_logical_cores: usize,
    pub memory_total_bytes: u64,
    pub memory_used_bytes: u64,
    pub memory_available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_used_bytes: u64,
    pub load_average_1_5_15_min: Option<[f64; 3]>,
    pub tasks_running: Option<u64>,
    pub tasks_total: Option<u64>,
    pub uptime_seconds: Option<u64>,
    pub network_received_bytes_per_sec: f64,
    pub network_sent_bytes_per_sec: f64,
    pub network_interfaces: BTreeMap<String, NetworkMetric>,
    pub disks: Vec<DiskMetric>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkMetric {
    pub received_bytes_per_sec: f64,
    pub sent_bytes_per_sec: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskMetric {
    pub mount: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: Option<f64>,
}
