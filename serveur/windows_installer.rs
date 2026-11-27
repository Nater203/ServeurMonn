use rustls::pki_types::ServerName;
use serde::Serialize;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Serialize)]
struct CredentialsFile<'a> {
    username: &'a str,
    password: &'a str,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("\nInstallation interrompue : {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    println!("====================================================");
    println!(" Assistant d'installation — Serveur Monitor");
    println!(" Récepteur Windows + tableau de bord local");
    println!("====================================================\n");

    let package_dir = env::current_exe()
        .map_err(|error| format!("localisation de l'installateur impossible ({error})"))?
        .parent()
        .ok_or_else(|| "dossier de l'installateur introuvable".to_string())?
        .to_path_buf();
    let receiver_source = package_dir.join("monitor-receiver.exe");
    if !receiver_source.is_file() {
        return Err(format!(
            "monitor-receiver.exe est absent de {}. Garde l'installateur et le récepteur dans le même dossier du ZIP.",
            package_dir.display()
        ));
    }

    println!("1/6 — Adresse d'écoute du récepteur");
    println!("IP locale à écouter. 0.0.0.0 accepte les connexions sur toutes les interfaces réseau.");
    let bind_ip = loop {
        let value = prompt("IP d'écoute", Some("0.0.0.0"))?;
        match value.parse::<IpAddr>() {
            Ok(ip) => break ip,
            Err(_) => println!("Adresse IP invalide. Exemples : 0.0.0.0 ou 192.168.1.20."),
        }
    };

    let port = prompt_port("Port du collecteur", 9050)?;
    let bind_address = SocketAddr::new(bind_ip, port);

    println!("\n2/6 — Adresse utilisée par Debian pour vérifier le certificat TLS");
    let server_name = loop {
        let suggested = if bind_ip.is_unspecified() {
            None
        } else {
            Some(bind_ip.to_string())
        };
        let value = prompt(
            "IP publique ou nom DNS du PC Windows",
            suggested.as_deref(),
        )?;
        if ServerName::try_from(value.clone()).is_ok() {
            break value;
        }
        println!("Adresse TLS invalide. Indique une adresse IP ou un nom DNS joignable depuis Debian.");
    };

    println!("\n3/6 — Identifiants partagés avec Debian");
    let username = prompt("Identifiant", Some("admin"))?;
    if username.is_empty() || username.len() > 128 {
        return Err("l'identifiant doit contenir entre 1 et 128 caractères".to_string());
    }
    let password = loop {
        let first = rpassword::prompt_password("Mot de passe (saisie masquée) : ")
            .map_err(|error| format!("lecture du mot de passe impossible ({error})"))?;
        let second = rpassword::prompt_password("Confirmer le mot de passe : ")
            .map_err(|error| format!("lecture de la confirmation impossible ({error})"))?;
        if first != second {
            println!("Les mots de passe ne correspondent pas. Réessaie.");
            continue;
        }
        if first.is_empty() || first.len() > 1024 {
            println!("Le mot de passe doit contenir entre 1 et 1024 caractères.");
            continue;
        }
        break first;
    };

    let web_port = prompt_port("Port du tableau de bord local", 8080)?;
    let install_dir = default_install_dir();

    println!("\n4/6 — Résumé");
    println!("Récepteur : {bind_address}");
    println!("Nom TLS   : {server_name}");
    println!("Web local : http://127.0.0.1:{web_port}/");
    println!("Identifiant : {username}");
    println!("Dossier   : {}", install_dir.display());
    println!("Le mot de passe ne sera pas affiché.");
    if !confirm("Installer avec ces paramètres", true)? {
        println!("Installation annulée.");
        return Ok(());
    }

    println!("\n5/6 — Copie des fichiers");
    fs::create_dir_all(&install_dir)
        .map_err(|error| format!("création de {} impossible ({error})", install_dir.display()))?;
    let receiver_target = install_dir.join("monitor-receiver.exe");
    fs::copy(&receiver_source, &receiver_target)
        .map_err(|error| format!("copie du récepteur impossible ({error})"))?;

    let credentials_path = install_dir.join("credentials.json");
    let credentials_json = serde_json::to_vec_pretty(&CredentialsFile {
        username: &username,
        password: &password,
    })
    .map_err(|error| format!("création du fichier de configuration impossible ({error})"))?;
    fs::write(&credentials_path, credentials_json)
        .map_err(|error| format!("écriture des identifiants impossible ({error})"))?;

    let launcher_path = install_dir.join("start-monitor.cmd");
    let launcher = format!(
        "@echo off\r\nsetlocal\r\nset \"MONITOR_USERNAME=\"\r\nset \"MONITOR_PASSWORD=\"\r\n\"%~dp0monitor-receiver.exe\" --bind \"{bind_address}\" --server-name \"{server_name}\" --web-port {web_port} --credentials-file \"%~dp0credentials.json\"\r\n"
    );
    fs::write(&launcher_path, launcher)
        .map_err(|error| format!("création du lanceur impossible ({error})"))?;

    println!("\n6/6 — Installation terminée");
    println!("Lance ce fichier pour démarrer le récepteur : {}", launcher_path.display());
    println!("Puis ouvre http://127.0.0.1:{web_port}/ dans le navigateur Windows.");
    println!("Au premier démarrage, le certificat sera créé et son empreinte affichée.");
    println!("Copie receiver-cert.pem sur Debian ; ne copie jamais receiver-key.pem.");
    println!("Autorise le port TCP {port} dans le pare-feu Windows, de préférence depuis l'adresse IP de Debian uniquement.");
    println!("Le tableau de bord reste local à ce PC Windows.");
    wait_for_enter()?;
    Ok(())
}

fn default_install_dir() -> PathBuf {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data)
            .join("Programs")
            .join("ServeurMonitor");
    }
    env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("ServeurMonitor")
}

fn prompt(label: &str, default: Option<&str>) -> Result<String, String> {
    match default {
        Some(value) => print!("{label} [{value}] : "),
        None => print!("{label} : "),
    }
    io::stdout()
        .flush()
        .map_err(|error| format!("affichage du prompt impossible ({error})"))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| format!("lecture de la réponse impossible ({error})"))?;
    let input = input.trim().to_string();
    if input.is_empty() {
        if let Some(value) = default {
            return Ok(value.to_string());
        }
        return Err(format!("{label} est obligatoire"));
    }
    Ok(input)
}

fn prompt_port(label: &str, default: u16) -> Result<u16, String> {
    loop {
        let default_text = default.to_string();
        let value = prompt(label, Some(&default_text))?;
        match value.parse::<u16>() {
            Ok(port) if port > 0 => return Ok(port),
            _ => println!("Saisis un port compris entre 1 et 65535."),
        }
    }
}

fn confirm(label: &str, default_yes: bool) -> Result<bool, String> {
    let default_text = if default_yes { "O/n" } else { "o/N" };
    let value = prompt(label, Some(default_text))?;
    match value.to_ascii_lowercase().as_str() {
        "o" | "oui" | "y" | "yes" => Ok(true),
        "n" | "non" | "no" => Ok(false),
        _ if default_yes => Ok(true),
        _ => Ok(false),
    }
}

fn wait_for_enter() -> Result<(), String> {
    print!("\nAppuie sur Entrée pour fermer l'installateur…");
    io::stdout()
        .flush()
        .map_err(|error| format!("affichage impossible ({error})"))?;
    let mut ignored = String::new();
    io::stdin()
        .read_line(&mut ignored)
        .map_err(|error| format!("lecture impossible ({error})"))?;
    Ok(())
}
