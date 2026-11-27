mod protocol;

use protocol::{
    ClientMessage, MetricsPayload, ServerReply, DEFAULT_PASSWORD, DEFAULT_PORT, DEFAULT_USERNAME,
    MAX_FRAME_BYTES,
};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const MAX_ACTIVE_CONNECTIONS: usize = 32;
const FAILED_AUTH_LIMIT: u32 = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const BLOCK_DURATION: Duration = Duration::from_secs(60);
const DEFAULT_WEB_PORT: u16 = 8080;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug)]
struct Options {
    bind: SocketAddr,
    server_name: String,
    web_port: u16,
    data_dir: PathBuf,
    credentials_file: Option<PathBuf>,
}

#[derive(serde::Deserialize)]
struct CredentialsFile {
    username: String,
    password: String,
}

#[derive(Debug)]
struct FailureState {
    window_started: Instant,
    failed_attempts: u32,
    blocked_until: Option<Instant>,
}

type FailureLimiter = Arc<Mutex<HashMap<IpAddr, FailureState>>>;

fn main() {
    if let Err(error) = run() {
        eprintln!("Erreur : {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let options = parse_args()?;
    fs::create_dir_all(&options.data_dir)
        .map_err(|error| format!("création du dossier de données impossible ({error})"))?;
    let (certificate_path, key_path) = ensure_certificate(&options.data_dir, &options.server_name)?;

    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificates = read_certificates(&certificate_path)?;
    let key_file = File::open(&key_path).map_err(|error| {
        format!(
            "lecture de la clé {} impossible ({error})",
            key_path.display()
        )
    })?;
    let private_key = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|error| format!("clé privée PEM invalide ({error})"))?
        .ok_or_else(|| "aucune clé privée trouvée dans le fichier PEM".to_string())?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| format!("certificat ou clé refusé par TLS ({error})"))?;
    let config = Arc::new(config);

    let (file_username, file_password) = match options.credentials_file.as_deref() {
        Some(path) => {
            let credentials = read_credentials(path)?;
            (credentials.username, credentials.password)
        }
        None => (DEFAULT_USERNAME.to_string(), DEFAULT_PASSWORD.to_string()),
    };
    let username = env::var("MONITOR_USERNAME").unwrap_or(file_username);
    let password = env::var("MONITOR_PASSWORD").unwrap_or(file_password);
    if username.is_empty() || password.is_empty() {
        return Err("MONITOR_USERNAME et MONITOR_PASSWORD ne peuvent pas être vides.".to_string());
    }

    let listener = TcpListener::bind(options.bind)
        .map_err(|error| format!("écoute sur {} impossible ({error})", options.bind))?;
    let latest_metrics_path = options.data_dir.join("latest_metrics.json");
    let latest_metrics = Arc::new(RwLock::new(load_latest_metrics(&latest_metrics_path)));
    let web_bind = SocketAddr::from(([127, 0, 0, 1], options.web_port));
    let web_listener = TcpListener::bind(web_bind).map_err(|error| {
        format!("démarrage du tableau de bord sur {web_bind} impossible ({error})")
    })?;
    let actual_web_bind = web_listener
        .local_addr()
        .map_err(|error| format!("lecture de l'adresse web impossible ({error})"))?;
    let web_metrics = Arc::clone(&latest_metrics);
    thread::spawn(move || run_web_server(web_listener, web_metrics));
    let active_connections = Arc::new(AtomicUsize::new(0));
    let failure_limiter: FailureLimiter = Arc::new(Mutex::new(HashMap::new()));

    println!(
        "Récepteur de monitoring Windows démarré sur {}",
        options.bind
    );
    println!("Nom TLS du certificat : {}", options.server_name);
    println!(
        "Certificat à copier sur Debian : {}",
        certificate_path.display()
    );
    println!(
        "Empreinte SHA-256 : {}",
        certificate_fingerprint(&certificate_path)?
    );
    println!(
        "Dernière mesure enregistrée dans : {}",
        latest_metrics_path.display()
    );
    println!("Tableau de bord local : http://{actual_web_bind}/");
    if username == DEFAULT_USERNAME && password == DEFAULT_PASSWORD {
        println!("Attention : identifiants par défaut admin/root. Change-les avec MONITOR_USERNAME et MONITOR_PASSWORD avant une exposition Internet.");
    }
    println!(
        "En attente des connexions sur le port {} (Ctrl+C pour arrêter).",
        options.bind.port()
    );

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("Connexion entrante impossible : {error}");
                continue;
            }
        };
        let peer = match stream.peer_addr() {
            Ok(peer) => peer,
            Err(_) => continue,
        };

        if is_blocked(&failure_limiter, peer.ip()) {
            drop(stream);
            continue;
        }
        if active_connections.fetch_add(1, Ordering::SeqCst) >= MAX_ACTIVE_CONNECTIONS {
            active_connections.fetch_sub(1, Ordering::SeqCst);
            drop(stream);
            continue;
        }

        let config = Arc::clone(&config);
        let username = username.clone();
        let password = password.clone();
        let latest_metrics_path = latest_metrics_path.clone();
        let latest_metrics = Arc::clone(&latest_metrics);
        let active_connections = Arc::clone(&active_connections);
        let failure_limiter = Arc::clone(&failure_limiter);
        thread::spawn(move || {
            let result = handle_connection(
                stream,
                config,
                &username,
                &password,
                &latest_metrics_path,
                peer.ip(),
                &failure_limiter,
            );
            match result {
                Ok(metrics) => {
                    *latest_metrics
                        .write()
                        .unwrap_or_else(|error| error.into_inner()) = Some(metrics.clone());
                    println!(
                        "Mesure reçue de {} : CPU {}, RAM {}, RX {}/s, TX {}/s",
                        metrics.hostname,
                        metrics
                            .cpu_percent
                            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.1}%")),
                        format_bytes(metrics.memory_used_bytes),
                        format_bytes(metrics.network_received_bytes_per_sec as u64),
                        format_bytes(metrics.network_sent_bytes_per_sec as u64),
                    );
                }
                Err(error) if error == "identifiants incorrects" => {
                    eprintln!("Authentification refusée depuis {peer}.");
                }
                Err(_) => {}
            }
            active_connections.fetch_sub(1, Ordering::SeqCst);
        });
    }

    Ok(())
}

fn parse_args() -> Result<Options, String> {
    let mut bind = SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT));
    let mut server_name: Option<String> = None;
    let mut web_port = DEFAULT_WEB_PORT;
    let mut data_dir: Option<PathBuf> = None;
    let mut credentials_file: Option<PathBuf> = None;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: monitor-receiver [--bind IP:PORT] [--server-name DNS_OU_IP] [--web-port PORT] [--data-dir DOSSIER] [--credentials-file FICHIER]\n\n\
                     --bind         Adresse d'écoute (défaut : 0.0.0.0:9050)\n\
                     --server-name  Adresse DNS/IP utilisée par Debian (défaut : localhost)\n\
                     --web-port     Port du tableau de bord local (défaut : 8080)\n\
                     --data-dir     Dossier du certificat et du dernier JSON reçu\n\
                     --credentials-file Fichier JSON username/password du collecteur\n\
                     MONITOR_USERNAME / MONITOR_PASSWORD définissent les identifiants (défaut : admin / root)"
                );
                std::process::exit(0);
            }
            "--bind" => {
                let value = args.next().ok_or("Il manque une adresse après --bind.")?;
                bind = value
                    .parse()
                    .map_err(|_| format!("Adresse d'écoute invalide : {value}"))?;
            }
            "--server-name" => {
                server_name = Some(args.next().ok_or("Il manque un nom après --server-name.")?);
            }
            "--web-port" => {
                let value = args.next().ok_or("Il manque un port après --web-port.")?;
                web_port = value
                    .parse::<u16>()
                    .map_err(|_| format!("Port web invalide : {value}"))?;
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(
                    args.next().ok_or("Il manque un chemin après --data-dir.")?,
                ));
            }
            "--credentials-file" => {
                credentials_file = Some(PathBuf::from(
                    args.next().ok_or("Il manque un chemin après --credentials-file.")?,
                ));
            }
            _ => {
                return Err(format!(
                    "Option inconnue : {arg}. Utilise --help pour l'aide."
                ))
            }
        }
    }

    let server_name = server_name.unwrap_or_else(|| "localhost".to_string());
    if server_name.trim().is_empty() {
        return Err("--server-name ne peut pas être vide.".to_string());
    }
    Ok(Options {
        bind,
        server_name,
        web_port,
        data_dir: data_dir.unwrap_or_else(default_data_dir),
        credentials_file,
    })
}

fn read_credentials(path: &Path) -> Result<CredentialsFile, String> {
    let contents = fs::read(path)
        .map_err(|error| format!("lecture des identifiants {} impossible ({error})", path.display()))?;
    let credentials = serde_json::from_slice::<CredentialsFile>(&contents)
        .map_err(|error| format!("fichier d'identifiants JSON invalide ({error})"))?;
    if credentials.username.is_empty() || credentials.password.is_empty() {
        return Err("le nom d'utilisateur et le mot de passe doivent être renseignés".to_string());
    }
    Ok(credentials)
}

fn run_web_server(listener: TcpListener, metrics: Arc<RwLock<Option<MetricsPayload>>>) {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let metrics = Arc::clone(&metrics);
                thread::spawn(move || handle_web_request(stream, metrics));
            }
            Err(error) => eprintln!("Requête web impossible : {error}"),
        }
    }
}

fn handle_web_request(mut stream: TcpStream, latest_metrics: Arc<RwLock<Option<MetricsPayload>>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let request = match read_http_request(&stream) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_http_response(
                &mut stream,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                b"Bad Request",
            );
            return;
        }
    };

    if request.0 != "GET" {
        let _ = write_http_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"Method Not Allowed",
        );
        return;
    }

    match request.1.as_str() {
        "/" | "/index.html" => {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                include_str!("dashboard.html").as_bytes(),
            );
        }
        "/api/metrics" => {
            let metrics = latest_metrics
                .read()
                .unwrap_or_else(|error| error.into_inner());
            let body = match metrics.as_ref() {
                Some(value) => serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec()),
                None => b"null".to_vec(),
            };
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                &body,
            );
        }
        "/favicon.ico" => {
            let _ = write_http_response(&mut stream, "204 No Content", "image/x-icon", b"");
        }
        _ => {
            let _ = write_http_response(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"Not Found",
            );
        }
    }
}

fn read_http_request(stream: &TcpStream) -> Result<(String, String), String> {
    let cloned = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(cloned);
    let mut remaining = MAX_HTTP_HEADER_BYTES;
    let request_line = read_http_line(&mut reader, &mut remaining)?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().ok_or_else(|| "ligne HTTP vide".to_string())?;
    let target = fields
        .next()
        .ok_or_else(|| "cible HTTP manquante".to_string())?;
    let version = fields
        .next()
        .ok_or_else(|| "version HTTP manquante".to_string())?;
    if fields.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err("ligne HTTP invalide".to_string());
    }

    loop {
        let line = read_http_line(&mut reader, &mut remaining)?;
        if line.is_empty() {
            break;
        }
        if !line.contains(':') {
            return Err("en-tête HTTP invalide".to_string());
        }
    }

    let path = target.split('?').next().unwrap_or(target);
    Ok((method.to_string(), path.to_string()))
}

fn read_http_line<R: BufRead>(reader: &mut R, remaining: &mut usize) -> Result<String, String> {
    let mut line = String::new();
    let bytes = reader
        .by_ref()
        .take(*remaining as u64)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if bytes == 0 || !line.ends_with('\n') {
        return Err("en-têtes HTTP incomplets ou trop grands".to_string());
    }
    *remaining = remaining.saturating_sub(bytes);
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn default_data_dir() -> PathBuf {
    if let Some(path) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(path).join("serveur-monitor");
    }
    if let Some(path) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(path).join("serveur-monitor");
    }
    if let Some(path) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")) {
        return PathBuf::from(path)
            .join(".local")
            .join("share")
            .join("serveur-monitor");
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("serveur-monitor-data")
}

fn load_latest_metrics(path: &Path) -> Option<MetricsPayload> {
    let contents = fs::read(path).ok()?;
    let metrics = serde_json::from_slice::<MetricsPayload>(&contents).ok()?;
    validate_metrics(&metrics).ok()?;
    Some(metrics)
}

fn ensure_certificate(data_dir: &Path, server_name: &str) -> Result<(PathBuf, PathBuf), String> {
    let certificate_path = data_dir.join("receiver-cert.pem");
    let key_path = data_dir.join("receiver-key.pem");
    let name_path = data_dir.join("receiver-server-name.txt");
    let certificate_exists = certificate_path.exists();
    let key_exists = key_path.exists();

    if certificate_exists && key_exists {
        let saved_name = fs::read_to_string(&name_path)
            .map_err(|error| format!("lecture de {} impossible ({error})", name_path.display()))?;
        if saved_name.trim() != server_name {
            return Err(format!(
                "le certificat existant est créé pour '{}', pas '{}'. Garde --server-name identique ou déplace le dossier de données pour en créer un nouveau.",
                saved_name.trim(),
                server_name
            ));
        }
        return Ok((certificate_path, key_path));
    }
    if certificate_exists || key_exists || name_path.exists() {
        return Err(format!(
            "dossier de certificat incomplet dans {}. Vérifie receiver-cert.pem, receiver-key.pem et receiver-server-name.txt.",
            data_dir.display()
        ));
    }

    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec![server_name.to_string()])
            .map_err(|error| format!("génération du certificat impossible ({error})"))?;
    fs::write(&certificate_path, cert.pem())
        .map_err(|error| format!("écriture du certificat impossible ({error})"))?;
    fs::write(&key_path, key_pair.serialize_pem())
        .map_err(|error| format!("écriture de la clé privée impossible ({error})"))?;
    fs::write(&name_path, server_name)
        .map_err(|error| format!("écriture du nom du certificat impossible ({error})"))?;
    Ok((certificate_path, key_path))
}

fn read_certificates(
    path: &Path,
) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "lecture du certificat {} impossible ({error})",
            path.display()
        )
    })?;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("certificat PEM invalide ({error})"))?;
    if certificates.is_empty() {
        return Err("aucun certificat trouvé dans le fichier PEM".to_string());
    }
    Ok(certificates)
}

fn certificate_fingerprint(path: &Path) -> Result<String, String> {
    let certificates = read_certificates(path)?;
    let digest = Sha256::digest(certificates[0].as_ref());
    Ok(digest
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":"))
}

fn handle_connection(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    expected_username: &str,
    expected_password: &str,
    latest_metrics_path: &Path,
    peer_ip: IpAddr,
    limiter: &FailureLimiter,
) -> Result<MetricsPayload, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let connection = ServerConnection::new(config).map_err(|error| error.to_string())?;
    let mut tls = StreamOwned::new(connection, stream);

    let auth: ClientMessage = read_json_line(&mut tls)?;
    let (username, password) = match auth {
        ClientMessage::Auth { username, password } => (username, password),
        ClientMessage::Metrics { .. } => {
            let _ = write_json_line(
                &mut tls,
                &ServerReply {
                    ok: false,
                    message: Some("authentification requise".to_string()),
                },
            );
            return Err("authentification requise".to_string());
        }
    };

    if username != expected_username
        || !constant_time_equal(password.as_bytes(), expected_password.as_bytes())
    {
        record_failure(limiter, peer_ip);
        thread::sleep(Duration::from_millis(300));
        let _ = write_json_line(
            &mut tls,
            &ServerReply {
                ok: false,
                message: Some("identifiants incorrects".to_string()),
            },
        );
        return Err("identifiants incorrects".to_string());
    }
    clear_failures(limiter, peer_ip);
    write_json_line(
        &mut tls,
        &ServerReply {
            ok: true,
            message: Some("authentifié".to_string()),
        },
    )
    .map_err(|error| format!("réponse d'authentification impossible ({error})"))?;

    let message: ClientMessage = read_json_line(&mut tls)?;
    let metrics = match message {
        ClientMessage::Metrics { metrics } => metrics,
        ClientMessage::Auth { .. } => return Err("message de métriques attendu".to_string()),
    };
    validate_metrics(&metrics)?;

    let json = serde_json::to_vec_pretty(&metrics)
        .map_err(|error| format!("sérialisation du JSON impossible ({error})"))?;
    fs::write(latest_metrics_path, json)
        .map_err(|error| format!("enregistrement de la dernière mesure impossible ({error})"))?;
    write_json_line(
        &mut tls,
        &ServerReply {
            ok: true,
            message: Some("métriques reçues".to_string()),
        },
    )
    .map_err(|error| format!("accusé de réception impossible ({error})"))?;
    Ok(metrics)
}

fn validate_metrics(metrics: &MetricsPayload) -> Result<(), String> {
    if metrics.schema_version != 1 {
        return Err(format!(
            "version de données inconnue : {}",
            metrics.schema_version
        ));
    }
    if metrics.hostname.len() > 255
        || metrics.disks.len() > 4096
        || metrics.network_interfaces.len() > 4096
    {
        return Err("données reçues hors limites".to_string());
    }
    if metrics
        .cpu_percent
        .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
        || !metrics.network_received_bytes_per_sec.is_finite()
        || !metrics.network_sent_bytes_per_sec.is_finite()
        || metrics.network_interfaces.values().any(|rate| {
            !rate.received_bytes_per_sec.is_finite() || !rate.sent_bytes_per_sec.is_finite()
        })
    {
        return Err("valeur de mesure invalide".to_string());
    }
    Ok(())
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn is_blocked(limiter: &FailureLimiter, ip: IpAddr) -> bool {
    let now = Instant::now();
    let mut states = limiter.lock().unwrap_or_else(|error| error.into_inner());
    let Some(state) = states.get(&ip) else {
        return false;
    };
    if state.blocked_until.is_some_and(|until| now < until) {
        return true;
    }
    if state.window_started.elapsed() >= FAILURE_WINDOW
        || state.blocked_until.is_some_and(|until| now >= until)
    {
        states.remove(&ip);
    }
    false
}

fn record_failure(limiter: &FailureLimiter, ip: IpAddr) {
    let now = Instant::now();
    let mut states = limiter.lock().unwrap_or_else(|error| error.into_inner());
    let state = states.entry(ip).or_insert(FailureState {
        window_started: now,
        failed_attempts: 0,
        blocked_until: None,
    });
    if state.window_started.elapsed() >= FAILURE_WINDOW {
        state.window_started = now;
        state.failed_attempts = 0;
        state.blocked_until = None;
    }
    state.failed_attempts = state.failed_attempts.saturating_add(1);
    if state.failed_attempts >= FAILED_AUTH_LIMIT {
        state.blocked_until = Some(now + BLOCK_DURATION);
    }
}

fn clear_failures(limiter: &FailureLimiter, ip: IpAddr) {
    limiter
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&ip);
}

fn read_json_line<T: serde::de::DeserializeOwned>(stream: &mut impl Read) -> Result<T, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let bytes = reader
        .by_ref()
        .take(MAX_FRAME_BYTES as u64)
        .read_line(&mut line)
        .map_err(|error| format!("lecture du message impossible ({error})"))?;
    if bytes == 0 || !line.ends_with('\n') {
        return Err("message vide ou trop grand".to_string());
    }
    serde_json::from_str(line.trim_end()).map_err(|error| format!("JSON invalide ({error})"))
}

fn write_json_line<T: serde::Serialize>(stream: &mut impl Write, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, value)
        .map_err(|error| io::Error::other(error.to_string()))?;
    stream.write_all(b"\n")?;
    stream.flush()
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
