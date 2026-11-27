mod protocol;

use protocol::MAX_FRAME_BYTES;
use protocol::{
    ClientMessage, DiskMetric, MetricsPayload, NetworkMetric, ServerReply, DEFAULT_PASSWORD,
    DEFAULT_PORT, DEFAULT_USERNAME,
};
use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
struct CpuCounters {
    total: u64,
    idle: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct Memory {
    total_kib: u64,
    available_kib: u64,
    swap_total_kib: u64,
    swap_free_kib: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct NetworkCounters {
    received_bytes: u64,
    sent_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct NetworkRate {
    received_bytes_per_sec: f64,
    sent_bytes_per_sec: f64,
}

#[derive(Clone, Debug)]
struct Disk {
    mount: String,
    total_bytes: u64,
    used_bytes: u64,
    available_bytes: u64,
    used_percent: String,
}

#[derive(Debug)]
struct Sample {
    cpu: Option<CpuCounters>,
    memory: Memory,
    network: BTreeMap<String, NetworkCounters>,
    load: Option<[f64; 3]>,
    tasks: Option<(u64, u64)>,
    uptime_seconds: Option<f64>,
    disks: Vec<Disk>,
    cpu_count: usize,
}

#[derive(Clone, Debug)]
struct SenderConfig {
    address: String,
    server_name: String,
    ca_cert: PathBuf,
    username: String,
    password: String,
}

fn main() {
    let (interval, once, headless, sender) = match parse_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if sender.is_some() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    let mut previous = Sample::read();
    let mut previous_at = Instant::now();
    let mut last_headless_error: Option<Instant> = None;

    loop {
        thread::sleep(interval);
        let current = Sample::read();
        let elapsed = previous_at.elapsed().as_secs_f64().max(0.001);
        let cpu_usage = cpu_usage(previous.cpu, current.cpu);
        let network_rates = network_rates(&previous.network, &current.network, elapsed);
        let metrics = make_payload(&current, cpu_usage, &network_rates);
        let send_result = sender.as_ref().map(|config| send_metrics(config, &metrics));
        let send_status = send_result
            .as_ref()
            .map(|result| match result {
                Ok(()) => "envoyé".to_string(),
                Err(error) => format!("échec : {error}"),
            })
            .unwrap_or_else(|| "désactivé".to_string());

        if headless {
            if let Some(Err(error)) = send_result {
                let now = Instant::now();
                if last_headless_error.is_none_or(|last| {
                    now.duration_since(last) >= Duration::from_secs(30)
                }) {
                    eprintln!("Transmission des métriques impossible : {error}");
                    last_headless_error = Some(now);
                }
            }
        } else {
            render(
                &current,
                cpu_usage,
                &network_rates,
                interval,
                once,
                &send_status,
            );
        }

        previous = current;
        previous_at = Instant::now();
        if once {
            break;
        }
    }
}

fn parse_args() -> Result<(Duration, bool, bool, Option<SenderConfig>), String> {
    let mut interval_seconds = 1_u64;
    let mut once = false;
    let mut headless = false;
    let mut send_to: Option<String> = None;
    let mut ca_cert: Option<PathBuf> = None;
    let mut server_name: Option<String> = None;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: serveur-monitor [--interval SECONDES] [--once] [--headless] [--send-to HOTE[:PORT] --ca-cert FICHIER]\n\n\
                     --interval, -i  Intervalle de mesure (défaut : 1 seconde)\n\
                     --once          Affiche un rapport après une période de mesure\n\
                     --headless      N'affiche pas le tableau local (pour un service systemd)\n\
                     --send-to       Adresse Windows (port par défaut : 9050)\n\
                     --ca-cert       Certificat PEM du récepteur Windows, à copier sur Debian\n\
                     --server-name   Nom DNS ou IP inscrit dans le certificat (défaut : hôte de --send-to)\n\
                     MONITOR_USERNAME / MONITOR_PASSWORD définissent les identifiants (défaut : admin / root)\n\
                     --help, -h      Affiche cette aide"
                );
                std::process::exit(0);
            }
            "--once" => once = true,
            "--headless" => headless = true,
            "--send-to" => {
                send_to = Some(
                    args.next()
                        .ok_or("Il manque une adresse après --send-to.")?,
                );
            }
            "--ca-cert" => {
                ca_cert = Some(PathBuf::from(
                    args.next().ok_or("Il manque un chemin après --ca-cert.")?,
                ));
            }
            "--server-name" => {
                server_name = Some(args.next().ok_or("Il manque un nom après --server-name.")?);
            }
            "-i" | "--interval" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("Il manque le nombre de secondes après {arg}."))?;
                interval_seconds = value.parse::<u64>().map_err(|_| {
                    format!("Intervalle invalide : {value}. Utilise un nombre entier de secondes.")
                })?;
            }
            _ => {
                return Err(format!(
                    "Option inconnue : {arg}. Utilise --help pour l'aide."
                ))
            }
        }
    }

    if interval_seconds == 0 {
        return Err("L'intervalle doit être supérieur à zéro.".to_string());
    }

    let sender = match (send_to, ca_cert) {
        (Some(address), Some(ca_cert)) => {
            let address = normalize_address(&address);
            let derived_name = address_host(&address).ok_or_else(|| {
                "Adresse de récepteur invalide. Utilise par exemple 203.0.113.10:9050.".to_string()
            })?;
            let username =
                env::var("MONITOR_USERNAME").unwrap_or_else(|_| DEFAULT_USERNAME.to_string());
            let password =
                env::var("MONITOR_PASSWORD").unwrap_or_else(|_| DEFAULT_PASSWORD.to_string());
            if username.is_empty() || password.is_empty() {
                return Err(
                    "MONITOR_USERNAME et MONITOR_PASSWORD ne peuvent pas être vides.".to_string(),
                );
            }
            Some(SenderConfig {
                address,
                server_name: server_name.unwrap_or(derived_name),
                ca_cert,
                username,
                password,
            })
        }
        (None, None) if server_name.is_none() => None,
        (None, None) => {
            return Err("--server-name doit être utilisé avec --send-to et --ca-cert.".to_string())
        }
        (Some(_), None) => return Err("--send-to doit être accompagné de --ca-cert.".to_string()),
        (None, Some(_)) => return Err("--ca-cert doit être accompagné de --send-to.".to_string()),
    };

    Ok((Duration::from_secs(interval_seconds), once, headless, sender))
}

impl Sample {
    fn read() -> Self {
        let cpu = read_cpu_counters();
        let cpu_count = read_cpu_count()
            .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, usize::from));
        let memory = read_memory();
        let (load, tasks) = read_load();
        let uptime_seconds = read_uptime();
        let network = read_network();
        let disks = read_disks();

        Self {
            cpu,
            memory,
            network,
            load,
            tasks,
            uptime_seconds,
            disks,
            cpu_count,
        }
    }
}

fn read_cpu_counters() -> Option<CpuCounters> {
    let contents = std::fs::read_to_string("/proc/stat").ok()?;
    let line = contents.lines().find(|line| line.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }

    // Linux reports guest time inside user/nice, so sum only the first eight fields.
    let total = values.iter().take(8).copied().sum();
    let idle = values[3].saturating_add(values.get(4).copied().unwrap_or(0));
    Some(CpuCounters { total, idle })
}

fn read_cpu_count() -> Option<usize> {
    let contents = std::fs::read_to_string("/proc/stat").ok()?;
    let count = contents
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu")
                .and_then(|suffix| suffix.parse::<usize>().ok())
                .is_some()
        })
        .count();
    (count > 0).then_some(count)
}

fn read_memory() -> Memory {
    let Ok(contents) = std::fs::read_to_string("/proc/meminfo") else {
        return Memory::default();
    };

    let mut values = BTreeMap::new();
    for line in contents.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        if let Some(value) = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        {
            values.insert(key, value);
        }
    }

    let total_kib = values.get("MemTotal").copied().unwrap_or(0);
    let available_kib = values
        .get("MemAvailable")
        .copied()
        .unwrap_or_else(|| values.get("MemFree").copied().unwrap_or(0));
    Memory {
        total_kib,
        available_kib: available_kib.min(total_kib),
        swap_total_kib: values.get("SwapTotal").copied().unwrap_or(0),
        swap_free_kib: values.get("SwapFree").copied().unwrap_or(0),
    }
}

fn read_load() -> (Option<[f64; 3]>, Option<(u64, u64)>) {
    let Ok(contents) = std::fs::read_to_string("/proc/loadavg") else {
        return (None, None);
    };
    let mut fields = contents.split_whitespace();
    let load = match (fields.next(), fields.next(), fields.next()) {
        (Some(a), Some(b), Some(c)) => match (a.parse(), b.parse(), c.parse()) {
            (Ok(a), Ok(b), Ok(c)) => Some([a, b, c]),
            _ => None,
        },
        _ => None,
    };
    let tasks = fields.next().and_then(|value| {
        let (running, total) = value.split_once('/')?;
        Some((running.parse().ok()?, total.parse().ok()?))
    });
    (load, tasks)
}

fn read_uptime() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn read_network() -> BTreeMap<String, NetworkCounters> {
    let Ok(contents) = std::fs::read_to_string("/proc/net/dev") else {
        return BTreeMap::new();
    };
    let mut result = BTreeMap::new();

    for line in contents.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let values: Vec<u64> = counters
            .split_whitespace()
            .filter_map(|value| value.parse().ok())
            .collect();
        if values.len() >= 9 {
            result.insert(
                name.to_string(),
                NetworkCounters {
                    received_bytes: values[0],
                    sent_bytes: values[8],
                },
            );
        }
    }
    result
}

fn read_disks() -> Vec<Disk> {
    let output = Command::new("df")
        .args([
            "-P", "-B1", "-x", "tmpfs", "-x", "devtmpfs", "-x", "squashfs",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let contents = String::from_utf8_lossy(&output.stdout);
    let mut disks = Vec::new();
    for line in contents.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let (Ok(total_bytes), Ok(used_bytes), Ok(available_bytes)) = (
            fields[1].parse::<u64>(),
            fields[2].parse::<u64>(),
            fields[3].parse::<u64>(),
        ) else {
            continue;
        };
        disks.push(Disk {
            mount: fields[5..].join(" "),
            total_bytes,
            used_bytes,
            available_bytes,
            used_percent: fields[4].to_string(),
        });
    }

    disks.sort_by(|left, right| {
        (left.mount != "/")
            .cmp(&(right.mount != "/"))
            .then_with(|| left.mount.cmp(&right.mount))
    });
    disks
}

fn cpu_usage(previous: Option<CpuCounters>, current: Option<CpuCounters>) -> Option<f64> {
    let (previous, current) = (previous?, current?);
    let total_delta = current.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return None;
    }
    let idle_delta = current.idle.saturating_sub(previous.idle).min(total_delta);
    Some((1.0 - idle_delta as f64 / total_delta as f64) * 100.0)
}

fn network_rates(
    previous: &BTreeMap<String, NetworkCounters>,
    current: &BTreeMap<String, NetworkCounters>,
    elapsed_seconds: f64,
) -> BTreeMap<String, NetworkRate> {
    let mut result = BTreeMap::new();
    for (name, counters) in current {
        if let Some(old) = previous.get(name) {
            result.insert(
                name.clone(),
                NetworkRate {
                    received_bytes_per_sec: counters
                        .received_bytes
                        .saturating_sub(old.received_bytes)
                        as f64
                        / elapsed_seconds,
                    sent_bytes_per_sec: counters.sent_bytes.saturating_sub(old.sent_bytes) as f64
                        / elapsed_seconds,
                },
            );
        }
    }
    result
}

fn render(
    sample: &Sample,
    cpu_usage: Option<f64>,
    network: &BTreeMap<String, NetworkRate>,
    interval: Duration,
    once: bool,
    send_status: &str,
) {
    let stdout_is_terminal = io::stdout().is_terminal();
    let mut output = io::BufWriter::new(io::stdout().lock());
    if stdout_is_terminal {
        let _ = write!(output, "\x1B[2J\x1B[H");
    }

    let _ = writeln!(output, "=== Surveillance du serveur Linux ===");
    let _ = writeln!(
        output,
        "Intervalle : {} s | Rafraîchissement : {}",
        interval.as_secs(),
        if once {
            "rapport unique"
        } else {
            "continu (Ctrl+C pour quitter)"
        }
    );

    match cpu_usage {
        Some(value) => {
            let _ = writeln!(
                output,
                "CPU         : {:>5.1}%  ({} cœurs logiques)",
                value, sample.cpu_count
            );
        }
        None => {
            let _ = writeln!(
                output,
                "CPU         : indisponible  ({} cœurs logiques)",
                sample.cpu_count
            );
        }
    }

    let memory_used = sample
        .memory
        .total_kib
        .saturating_sub(sample.memory.available_kib);
    let memory_percent = percent(memory_used, sample.memory.total_kib);
    let _ = writeln!(
        output,
        "RAM         : {} / {} utilisés ({}) | disponible : {}",
        format_bytes(memory_used.saturating_mul(1024)),
        format_bytes(sample.memory.total_kib.saturating_mul(1024)),
        format_percent(memory_percent),
        format_bytes(sample.memory.available_kib.saturating_mul(1024))
    );

    let swap_used = sample
        .memory
        .swap_total_kib
        .saturating_sub(sample.memory.swap_free_kib);
    let _ = writeln!(
        output,
        "Swap        : {} / {} utilisés ({})",
        format_bytes(swap_used.saturating_mul(1024)),
        format_bytes(sample.memory.swap_total_kib.saturating_mul(1024)),
        format_percent(percent(swap_used, sample.memory.swap_total_kib))
    );

    if let Some(load) = sample.load {
        let tasks = sample
            .tasks
            .map(|(running, total)| format!(" | tâches : {running} actives / {total}"))
            .unwrap_or_default();
        let _ = writeln!(
            output,
            "Charge      : {:.2} / {:.2} / {:.2} (1 / 5 / 15 min){}",
            load[0], load[1], load[2], tasks
        );
    } else {
        let _ = writeln!(output, "Charge      : indisponible");
    }

    let uptime = sample
        .uptime_seconds
        .map(format_uptime)
        .unwrap_or_else(|| "indisponible".to_string());
    let _ = writeln!(output, "Fonctionnement : {uptime}");
    let _ = writeln!(output, "Transmission : {send_status}");

    let total_received: f64 = network
        .values()
        .map(|rate| rate.received_bytes_per_sec)
        .sum();
    let total_sent: f64 = network.values().map(|rate| rate.sent_bytes_per_sec).sum();
    let _ = writeln!(
        output,
        "Réseau      : réception {} | émission {} (hors interface lo)",
        format_rate(total_received),
        format_rate(total_sent)
    );
    if network.is_empty() {
        let _ = writeln!(output, "  Interfaces : aucune interface réseau mesurable");
    } else {
        for (name, rate) in network.iter().take(8) {
            let _ = writeln!(
                output,
                "  {name:<12} RX {:>12} | TX {:>12}",
                format_rate(rate.received_bytes_per_sec),
                format_rate(rate.sent_bytes_per_sec)
            );
        }
    }

    let _ = writeln!(output, "Stockage :");
    if sample.disks.is_empty() {
        let _ = writeln!(
            output,
            "  informations indisponibles (commande df absente ou erreur)"
        );
    } else {
        for disk in &sample.disks {
            let _ = writeln!(
                output,
                "  {:<24} {:>10} / {:>10} utilisés ({:>4}) | libre {:>10}",
                truncate(&disk.mount, 24),
                format_bytes(disk.used_bytes),
                format_bytes(disk.total_bytes),
                disk.used_percent,
                format_bytes(disk.available_bytes)
            );
        }
    }

    let _ = output.flush();
}

fn percent(part: u64, total: u64) -> Option<f64> {
    (total > 0).then_some(part as f64 * 100.0 / total as f64)
}

fn format_percent(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}%"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second.max(0.0) as u64))
}

fn format_uptime(seconds: f64) -> String {
    let mut remaining = seconds.max(0.0) as u64;
    let days = remaining / 86_400;
    remaining %= 86_400;
    let hours = remaining / 3_600;
    remaining %= 3_600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;
    if days > 0 {
        format!("{days} j {hours} h {minutes} min")
    } else if hours > 0 {
        format!("{hours} h {minutes} min {seconds} s")
    } else {
        format!("{minutes} min {seconds} s")
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!(
            "{}…",
            prefix
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    } else {
        prefix
    }
}

fn make_payload(
    sample: &Sample,
    cpu_percent: Option<f64>,
    network: &BTreeMap<String, NetworkRate>,
) -> MetricsPayload {
    let memory_used_kib = sample
        .memory
        .total_kib
        .saturating_sub(sample.memory.available_kib);
    let swap_used_kib = sample
        .memory
        .swap_total_kib
        .saturating_sub(sample.memory.swap_free_kib);
    let network_interfaces: BTreeMap<String, NetworkMetric> = network
        .iter()
        .map(|(name, rate)| {
            (
                name.clone(),
                NetworkMetric {
                    received_bytes_per_sec: rate.received_bytes_per_sec,
                    sent_bytes_per_sec: rate.sent_bytes_per_sec,
                },
            )
        })
        .collect();
    let network_received_bytes_per_sec = network_interfaces
        .values()
        .map(|rate| rate.received_bytes_per_sec)
        .sum();
    let network_sent_bytes_per_sec = network_interfaces
        .values()
        .map(|rate| rate.sent_bytes_per_sec)
        .sum();
    let (tasks_running, tasks_total) = sample.tasks.map_or((None, None), |(running, total)| {
        (Some(running), Some(total))
    });

    MetricsPayload {
        schema_version: 1,
        timestamp_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        hostname: read_hostname(),
        cpu_percent,
        cpu_logical_cores: sample.cpu_count,
        memory_total_bytes: sample.memory.total_kib.saturating_mul(1024),
        memory_used_bytes: memory_used_kib.saturating_mul(1024),
        memory_available_bytes: sample.memory.available_kib.saturating_mul(1024),
        swap_total_bytes: sample.memory.swap_total_kib.saturating_mul(1024),
        swap_used_bytes: swap_used_kib.saturating_mul(1024),
        load_average_1_5_15_min: sample.load,
        tasks_running,
        tasks_total,
        uptime_seconds: sample.uptime_seconds.map(|value| value as u64),
        network_received_bytes_per_sec,
        network_sent_bytes_per_sec,
        network_interfaces,
        disks: sample
            .disks
            .iter()
            .map(|disk| DiskMetric {
                mount: disk.mount.clone(),
                total_bytes: disk.total_bytes,
                used_bytes: disk.used_bytes,
                available_bytes: disk.available_bytes,
                used_percent: disk.used_percent.trim_end_matches('%').parse().ok(),
            })
            .collect(),
    }
}

fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "linux-server".to_string())
}

fn normalize_address(address: &str) -> String {
    if address.parse::<std::net::SocketAddr>().is_ok() {
        return address.to_string();
    }
    if address.starts_with('[') {
        if address.contains("]:") {
            return address.to_string();
        }
        return format!("{address}:{DEFAULT_PORT}");
    }
    if address.matches(':').count() > 1 {
        return format!("[{address}]:{DEFAULT_PORT}");
    }
    if address
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok())
        .is_some()
    {
        address.to_string()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    }
}

fn address_host(address: &str) -> Option<String> {
    if let Ok(socket) = address.parse::<std::net::SocketAddr>() {
        return Some(socket.ip().to_string());
    }
    if let Some(host) = address
        .strip_prefix('[')
        .and_then(|value| value.split_once(']'))
    {
        return Some(host.0.to_string());
    }
    address.rsplit_once(':').map(|(host, _)| host.to_string())
}

fn send_metrics(config: &SenderConfig, metrics: &MetricsPayload) -> Result<(), String> {
    let endpoints = config
        .address
        .to_socket_addrs()
        .map_err(|error| format!("adresse invalide ({error})"))?;
    let mut connected = None;
    let mut connect_error = None;
    for endpoint in endpoints {
        match TcpStream::connect_timeout(&endpoint, Duration::from_secs(5)) {
            Ok(stream) => {
                connected = Some(stream);
                break;
            }
            Err(error) => connect_error = Some(error),
        }
    }
    let stream = connected.ok_or_else(|| {
        connect_error
            .map(|error| format!("connexion impossible ({error})"))
            .unwrap_or_else(|| "aucune adresse IP trouvée".to_string())
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;

    let mut roots = RootCertStore::empty();
    let cert_file = File::open(&config.ca_cert).map_err(|error| {
        format!(
            "lecture du certificat {} impossible ({error})",
            config.ca_cert.display()
        )
    })?;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("certificat PEM invalide ({error})"))?;
    if certificates.is_empty() {
        return Err("le fichier PEM ne contient aucun certificat".to_string());
    }
    for certificate in certificates {
        roots
            .add(certificate)
            .map_err(|error| format!("certificat racine invalide ({error})"))?;
    }

    let server_name = ServerName::try_from(config.server_name.clone())
        .map_err(|_| "nom TLS invalide; utilise un nom DNS ou une adresse IP".to_string())?;
    let client_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connection = ClientConnection::new(Arc::new(client_config), server_name)
        .map_err(|error| format!("initialisation TLS impossible ({error})"))?;
    let mut tls = StreamOwned::new(connection, stream);

    write_json_line(
        &mut tls,
        &ClientMessage::Auth {
            username: config.username.clone(),
            password: config.password.clone(),
        },
    )
    .map_err(|error| format!("envoi de l'authentification impossible ({error})"))?;
    let auth_reply: ServerReply = read_json_line(&mut tls)
        .map_err(|error| format!("réponse d'authentification invalide ({error})"))?;
    if !auth_reply.ok {
        return Err(auth_reply
            .message
            .unwrap_or_else(|| "identifiants refusés".to_string()));
    }

    write_json_line(
        &mut tls,
        &ClientMessage::Metrics {
            metrics: metrics.clone(),
        },
    )
    .map_err(|error| format!("envoi des métriques impossible ({error})"))?;
    let reply: ServerReply = read_json_line(&mut tls)
        .map_err(|error| format!("accusé de réception invalide ({error})"))?;
    if reply.ok {
        Ok(())
    } else {
        Err(reply
            .message
            .unwrap_or_else(|| "métriques refusées".to_string()))
    }
}

fn write_json_line<T: Serialize>(stream: &mut impl Write, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn read_json_line<T: DeserializeOwned>(stream: &mut impl Read) -> Result<T, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let bytes = reader
        .by_ref()
        .take(MAX_FRAME_BYTES as u64)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if bytes == 0 || !line.ends_with('\n') {
        return Err("trame vide ou trop grande".to_string());
    }
    serde_json::from_str(line.trim_end()).map_err(|error| format!("JSON invalide ({error})"))
}
