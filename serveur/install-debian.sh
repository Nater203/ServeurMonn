#!/usr/bin/env bash
set -Eeuo pipefail
umask 077

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
APP_NAME="serveur-monitor"
INSTALL_PATH="/usr/local/bin/serveur-monitor"
CONFIG_DIR="/etc/serveur-monitor"
ENV_FILE="${CONFIG_DIR}/monitor.env"
SERVICE_FILE="/etc/systemd/system/serveur-monitor.service"
CERT_INSTALL_PATH="${CONFIG_DIR}/receiver-cert.pem"

as_root() {
    if (( EUID == 0 )); then
        "$@"
    else
        sudo "$@"
    fi
}

prompt_value() {
    local label="$1"
    local default_value="$2"
    local answer
    read -r -p "${label} [${default_value}]: " answer
    printf '%s' "${answer:-$default_value}"
}

prompt_password() {
    local first second
    while true; do
        read -r -s -p "Mot de passe partagé avec Windows (saisie masquée): " first
        printf '\n' >&2
        read -r -s -p "Confirme le mot de passe: " second
        printf '\n' >&2
        if [[ -z "$first" ]]; then
            printf 'Le mot de passe ne peut pas être vide.\n'
        elif [[ "$first" != "$second" ]]; then
            printf 'Les mots de passe ne correspondent pas.\n'
        else
            printf '%s' "$first"
            return 0
        fi
    done
}

confirm_yes() {
    local prompt="$1"
    local answer
    read -r -p "${prompt} [O/n]: " answer
    [[ -z "$answer" || "$answer" =~ ^([oOyY]|oui|yes)$ ]]
}

validate_port() {
    [[ "$1" =~ ^[0-9]{1,5}$ ]] && (( 10#$1 >= 1 && 10#$1 <= 65535 ))
}

systemd_quote() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "$value"
}

echo "===================================================="
echo " Assistant d'installation — Serveur Monitor"
echo " Collecteur Debian 13 en service systemd"
echo "===================================================="
echo

if [[ ! -d /run/systemd/system ]] || ! command -v systemctl >/dev/null 2>&1; then
    echo "systemd n'est pas actif. Ce programme d'installation configure un service systemd." >&2
    exit 1
fi

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
    echo "Rust et Cargo sont nécessaires pour construire le collecteur sur Debian."
    if ! confirm_yes "Installer rustc et cargo avec apt ?"; then
        echo "Installation annulée. Installe rustc/cargo puis relance ce script."
        exit 1
    fi
    as_root apt-get update
    as_root apt-get install -y rustc cargo
fi

if [[ ! -f "${SCRIPT_DIR}/Cargo.toml" || ! -f "${SCRIPT_DIR}/Cargo.lock" || ! -f "${SCRIPT_DIR}/main.rs" ]]; then
    echo "Le ZIP doit contenir Cargo.toml, Cargo.lock et main.rs dans le même dossier que ce script." >&2
    exit 1
fi

echo
echo "1/6 — Connexion au récepteur Windows"
while true; do
    WINDOWS_HOST="$(prompt_value "IP ou nom DNS du PC Windows" "")"
    if [[ -n "$WINDOWS_HOST" && "$WINDOWS_HOST" =~ ^[A-Za-z0-9.:-]+$ ]]; then
        break
    fi
    echo "Saisis une adresse IP ou un nom DNS (sans protocole http://)."
done

while true; do
    WINDOWS_PORT="$(prompt_value "Port TCP du récepteur" "9050")"
    if validate_port "$WINDOWS_PORT"; then break; fi
    echo "Le port doit être compris entre 1 et 65535."
done

WINDOWS_TLS_NAME="$(prompt_value "Nom TLS du certificat Windows" "$WINDOWS_HOST")"
if [[ ! "$WINDOWS_TLS_NAME" =~ ^[A-Za-z0-9.:-]+$ ]]; then
    echo "Le nom TLS doit être une adresse IP ou un nom DNS." >&2
    exit 1
fi

echo
echo "2/6 — Certificat public du récepteur"
DEFAULT_CERT_PATH="${SCRIPT_DIR}/receiver-cert.pem"
if [[ ! -f "$DEFAULT_CERT_PATH" ]]; then DEFAULT_CERT_PATH=""; fi
while true; do
    CERT_SOURCE="$(prompt_value "Chemin du receiver-cert.pem copié depuis Windows" "$DEFAULT_CERT_PATH")"
    if [[ -f "$CERT_SOURCE" && -r "$CERT_SOURCE" ]]; then break; fi
    echo "Certificat introuvable ou illisible : ${CERT_SOURCE}"
done

echo
echo "3/6 — Identifiants (ils doivent être identiques à ceux configurés sous Windows)"
USERNAME="$(prompt_value "Identifiant" "admin")"
if [[ -z "$USERNAME" || "$USERNAME" == *$'\n'* ]]; then
    echo "Identifiant invalide." >&2
    exit 1
fi
PASSWORD="$(prompt_password)"

echo
echo "4/6 — Intervalle des mesures"
while true; do
    INTERVAL="$(prompt_value "Intervalle en secondes" "1")"
    if [[ "$INTERVAL" =~ ^[0-9]{1,4}$ ]] && (( 10#$INTERVAL >= 1 && 10#$INTERVAL <= 3600 )); then break; fi
    echo "Choisis un intervalle entre 1 et 3600 secondes."
done

if [[ "$WINDOWS_HOST" == *:* && "$WINDOWS_HOST" != \[*\] ]]; then
    WINDOWS_ADDRESS="[${WINDOWS_HOST}]:${WINDOWS_PORT}"
else
    WINDOWS_ADDRESS="${WINDOWS_HOST}:${WINDOWS_PORT}"
fi

echo
echo "5/6 — Résumé"
echo "Windows    : ${WINDOWS_ADDRESS}"
echo "Nom TLS    : ${WINDOWS_TLS_NAME}"
echo "Identifiant: ${USERNAME}"
echo "Intervalle : ${INTERVAL} seconde(s)"
echo "Certificat : ${CERT_SOURCE}"
echo "Service    : ${APP_NAME}.service"
if [[ -e "$ENV_FILE" || -e "$SERVICE_FILE" ]]; then
    echo "Une installation existe déjà. Ses fichiers de configuration seront sauvegardés avant remplacement."
fi
if ! confirm_yes "Construire et installer le collecteur ?"; then
    echo "Installation annulée."
    exit 0
fi

echo
echo "Construction du collecteur Linux…"
cd "$SCRIPT_DIR"
cargo build --release --locked --bin serveur-monitor

if [[ ! -x "${SCRIPT_DIR}/target/release/serveur-monitor" ]]; then
    echo "Le binaire n'a pas été créé dans target/release." >&2
    exit 1
fi

if [[ -e "$ENV_FILE" || -e "$SERVICE_FILE" ]]; then
    BACKUP_SUFFIX="$(date +%Y%m%d-%H%M%S)"
    if as_root test -e "$ENV_FILE"; then as_root cp -a "$ENV_FILE" "${ENV_FILE}.bak.${BACKUP_SUFFIX}"; fi
    if as_root test -e "$SERVICE_FILE"; then as_root cp -a "$SERVICE_FILE" "${SERVICE_FILE}.bak.${BACKUP_SUFFIX}"; fi
fi

as_root install -d -o root -g root -m 0755 "$CONFIG_DIR"
as_root install -o root -g root -m 0755 "${SCRIPT_DIR}/target/release/serveur-monitor" "$INSTALL_PATH"
as_root install -o root -g root -m 0644 "$CERT_SOURCE" "$CERT_INSTALL_PATH"

ENV_TEMP="$(mktemp)"
SERVICE_TEMP="$(mktemp)"
trap 'rm -f -- "$ENV_TEMP" "$SERVICE_TEMP"' EXIT
{
    printf 'MONITOR_USERNAME=%s\n' "$(systemd_quote "$USERNAME")"
    printf 'MONITOR_PASSWORD=%s\n' "$(systemd_quote "$PASSWORD")"
    printf 'MONITOR_SERVER=%s\n' "$(systemd_quote "$WINDOWS_ADDRESS")"
    printf 'MONITOR_SERVER_NAME=%s\n' "$(systemd_quote "$WINDOWS_TLS_NAME")"
    printf 'MONITOR_INTERVAL=%s\n' "$INTERVAL"
} > "$ENV_TEMP"
as_root install -o root -g root -m 0600 "$ENV_TEMP" "$ENV_FILE"

cat > "$SERVICE_TEMP" <<'UNIT'
[Unit]
Description=Serveur Monitor — collecte et envoi TLS des métriques
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=serveur-monitor
Group=serveur-monitor
EnvironmentFile=/etc/serveur-monitor/monitor.env
ExecStart=/usr/local/bin/serveur-monitor --interval ${MONITOR_INTERVAL} --send-to ${MONITOR_SERVER} --ca-cert /etc/serveur-monitor/receiver-cert.pem --server-name ${MONITOR_SERVER_NAME} --headless
Restart=on-failure
RestartSec=5s
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6

[Install]
WantedBy=multi-user.target
UNIT

if ! id -u serveur-monitor >/dev/null 2>&1; then
    as_root useradd --system --no-create-home --shell /usr/sbin/nologin --user-group serveur-monitor
fi
as_root install -o root -g root -m 0644 "$SERVICE_TEMP" "$SERVICE_FILE"
as_root systemctl daemon-reload
as_root systemctl enable --now serveur-monitor.service

unset PASSWORD
echo
echo "6/6 — Installation terminée"
echo "Le service tourne en arrière-plan."
if (( EUID == 0 )); then
    echo "État : systemctl status serveur-monitor"
    echo "Journaux : journalctl -u serveur-monitor -f"
else
    echo "État : sudo systemctl status serveur-monitor"
    echo "Journaux : sudo journalctl -u serveur-monitor -f"
fi
echo "Le tableau de bord se trouve sur le PC Windows, à son adresse locale."
echo "Si Windows n'est pas encore démarré, le collecteur réessaiera automatiquement."
