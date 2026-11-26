# Installation de Serveur Monitor

Ce dossier contient deux programmes : le récepteur Windows avec son tableau de bord local, et le collecteur à installer sur Debian 13.

## Windows

Le ZIP doit garder `monitor-installer.exe` et `monitor-receiver.exe` côte à côte. Lance `monitor-installer.exe` : il demande l’adresse d’écoute, le port, l’adresse IP ou DNS que Debian utilisera pour vérifier le certificat, l’identifiant et le mot de passe. La saisie du mot de passe est masquée.

L’installateur place le récepteur et `credentials.json` dans `%LOCALAPPDATA%\Programs\ServeurMonitor`, puis crée `start-monitor.cmd`. Lance ce fichier pour démarrer le récepteur. Le premier démarrage génère un certificat TLS et affiche son empreinte SHA-256. Copie uniquement `receiver-cert.pem` sur Debian ; conserve `receiver-key.pem` sur Windows.

Le tableau de bord est accessible sur `http://127.0.0.1:8080/` (ou sur le port local choisi). Il reste limité à l’ordinateur Windows. Si le port est déjà utilisé, l’installateur permet d’en choisir un autre.

Le premier démarrage crée le certificat dans `%LOCALAPPDATA%\serveur-monitor\receiver-cert.pem`. Copie ce fichier public sur Debian lorsque l’installateur Debian le demande. La clé privée `receiver-key.pem` reste sur Windows et ne doit pas être partagée.

Autorise le port TCP choisi pour le collecteur dans le pare-feu Windows. Pour limiter les connexions, autorise seulement l’adresse IP de Debian.

## Debian 13

Ces command servent a installer les dossier depuis ce github

Copie le certificat public dans le dossier du ZIP ou garde son chemin sous la main, puis depuis le dossier extrait lance :

```bash
sudo apt update
sudo apt install -y git unzip
git clone https://github.com/Nater203/ServeurMonn.git
cd ServeurMonn
unzip serveurM.zip
cd serveur
bash install-debian.sh
```

Le script demande l’adresse Windows, le port, le nom TLS du certificat, le chemin du certificat public, les mêmes identifiants et l’intervalle. Si Cargo manque, il propose de l’installer avec `apt`. Il construit ensuite le collecteur, l’installe dans `/usr/local/bin` et crée le service `serveur-monitor.service`. Lance-le avec `bash install-debian.sh` depuis un compte avec `sudo`, ou directement en tant que `root`.

Commandes utiles :

```bash
sudo systemctl status serveur-monitor
sudo journalctl -u serveur-monitor -f
```

Les identifiants du service sont conservés dans `/etc/serveur-monitor/monitor.env`, lisible uniquement par root. Le certificat copié dans `/etc/serveur-monitor/receiver-cert.pem` est public.

## Fichiers à garder dans le ZIP source

Garde `Cargo.toml`, `Cargo.lock`, `main.rs`, `protocol.rs`, `windows_receiver.rs`, `windows_installer.rs`, `dashboard.html`, `install-debian.sh` et ce fichier. Ajoute les exécutables Windows `monitor-installer.exe` et `monitor-receiver.exe` côte à côte pour que l’assistant puisse installer le récepteur sans demander Rust à l’utilisateur Windows.
