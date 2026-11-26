# Serveur Monitor — tutoriel complet

Ce guide explique l’installation depuis le téléchargement jusqu’au tableau de bord. Le récepteur et le tableau de bord tournent sur Windows ; le collecteur mesure le serveur Linux et lui envoie les données.

Le guide principal décrit une connexion entre deux machines sur le même réseau local. Une section à la fin explique les éléments à ajouter pour une connexion depuis Internet.

> Dans les commandes ci-dessous, remplace les valeurs en majuscules par les tiennes. Ne partage jamais le fichier receiver-key.pem : il contient la clé privée du récepteur.

## 1. Télécharger et extraire les fichiers

### Depuis Windows

1. Ouvre [le dépôt ServeurMonn sur GitHub](https://github.com/Nater203/ServeurMonn).
2. Clique sur **Code**, puis **Download ZIP**.
3. Décompresse l’archive téléchargée.
4. Dans le dossier créé, repère **serveurM.zip** et décompresse-le à son tour.
5. Ouvre le dossier **serveur**. Il doit contenir **monitor-installer.exe** et **monitor-receiver.exe**, ainsi que les fichiers sources.

### Depuis Debian ou Ubuntu

Ouvre un terminal. Installe les outils nécessaires au téléchargement et à la compilation :

```bash
sudo apt update
sudo apt install -y git unzip curl build-essential
```

Récupère le dépôt et extrait le paquet du projet :

```bash
git clone https://github.com/Nater203/ServeurMonn.git
cd ServeurMonn
unzip serveurM.zip
cd serveur
```

Si tu as déjà téléchargé et extrait les archives, ouvre simplement un terminal dans le dossier **serveur** qui contient Cargo.toml et install-debian.sh.

## 2. Installer et démarrer le récepteur Windows

Fais cette étape avant l’installation Linux, car Windows doit créer le certificat que Linux utilisera.

1. Dans le dossier **serveur**, lance **monitor-installer.exe**.
2. Réponds à ses questions :

   - **IP d’écoute** : garde **0.0.0.0**. Cela permet à Windows d’écouter sur ses interfaces réseau. Ce n’est pas l’adresse que tu donneras au collecteur Linux.
   - **Port du collecteur** : garde **9050**, sauf si tu as une raison de le changer.
   - **Nom TLS** : pour suivre ce guide, saisis **localhost**. Il faudra indiquer le même nom sur Linux.
   - **Identifiant et mot de passe** : choisis tes propres valeurs et note-les. Tu devras saisir exactement les mêmes sur Linux. Évite le mot de passe par défaut **root**.
   - **Port du tableau de bord** : garde **8080**, sauf s’il est déjà utilisé.

3. Confirme l’installation. Elle copie le récepteur dans :

   **%LOCALAPPDATA%\Programs\ServeurMonitor**

4. À la fin, l’installateur affiche le chemin du lanceur **start-monitor.cmd**. Lance ce fichier pour démarrer le récepteur. Garde sa fenêtre ouverte pendant l’utilisation.
5. N’ouvre pas directement **monitor-receiver.exe** depuis le dossier extrait. Le lanceur start-monitor.cmd charge les paramètres et les identifiants enregistrés par l’installateur. L’exécutable lancé directement utilise les réglages par défaut.
6. Au premier démarrage, le récepteur crée son certificat. Son chemin ressemble à :

   **C:\Users\TON_NOM\AppData\Local\serveur-monitor\receiver-cert.pem**

   Le récepteur affiche aussi le chemin exact et l’empreinte du certificat.

Le tableau de bord est disponible sur le PC Windows à :

```text
http://127.0.0.1:8080/
```

Il reste accessible uniquement depuis ce PC. La fenêtre du récepteur doit rester ouverte : la fermer arrête le récepteur.

### Autoriser la connexion dans le pare-feu Windows

Autorise une règle entrante TCP sur le port **9050** (ou le port que tu as choisi). Dans « Pare-feu Windows Defender avec fonctions avancées de sécurité », crée une règle **Entrant > Port > TCP > 9050 > Autoriser**. Si possible, limite l’adresse IP distante à celle du serveur Linux.

## 3. Trouver les adresses réseau

Les deux adresses à ne pas confondre sont :

- **IP Windows** : adresse IPv4 du PC Windows, par exemple **192.168.1.25**. Le collecteur Linux s’y connectera.
- **IP d’écoute Windows** : **0.0.0.0**. Cette valeur est utilisée uniquement par le récepteur Windows pour écouter sur toutes ses interfaces.

Pour trouver l’adresse IPv4 Windows, ouvre PowerShell et saisis :

```powershell
ipconfig
```

Repère « Adresse IPv4 » sous la carte Ethernet ou Wi-Fi connectée. N’utilise pas 0.0.0.0 ni localhost comme adresse de destination Linux.

Pour trouver l’adresse Linux, tu peux saisir :

```bash
hostname -I
```

Cette adresse est utile pour limiter le pare-feu Windows à la machine Linux.

## 4. Copier le certificat public de Windows vers Linux

L’installateur Linux a besoin du certificat créé à l’étape 2. Un chemin Windows comme C:\Users\... n’est pas lisible directement par Linux : le fichier doit d’abord être transféré.

Depuis PowerShell sur Windows, exécute cette commande en remplaçant **UTILISATEUR_LINUX** et **IP_LINUX** :

```powershell
scp "$env:LOCALAPPDATA\serveur-monitor\receiver-cert.pem" UTILISATEUR_LINUX@IP_LINUX:/tmp/receiver-cert.pem
```

Exemple :

```powershell
scp "$env:LOCALAPPDATA\serveur-monitor\receiver-cert.pem" destonix@192.168.1.176:/tmp/receiver-cert.pem
```

Saisis le mot de passe du compte Linux si SSH le demande. Si la commande scp n’est pas disponible, utilise une clé USB ou une application de transfert comme WinSCP pour placer le certificat dans **/tmp/receiver-cert.pem** sur Linux.

Vérifie sur Linux que le fichier est arrivé :

```bash
ls -l /tmp/receiver-cert.pem
```

Copie uniquement **receiver-cert.pem**. Ne transfère pas **receiver-key.pem**.

## 5. Préparer Rust sur Debian ou Ubuntu

Le projet doit être compilé sur la machine Linux. Il nécessite Rust 1.85 ou plus récent et un Cargo qui sait lire le fichier Cargo.lock v4. Le moyen recommandé est d’installer Rust stable avec rustup, plutôt que d’utiliser un Cargo ancien fourni par la distribution.

Si Rust n’est pas déjà installé avec rustup, exécute :

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
```

À la question de rustup, choisis l’installation par défaut. Vérifie ensuite les versions :

```bash
rustc --version
cargo --version
```

Si tu viens d’installer rustup et que le terminal ne trouve pas cargo, ferme et rouvre le terminal, ou exécute de nouveau :

```bash
. "$HOME/.cargo/env"
```

Ubuntu convient aussi pour essayer le programme : l’installateur nécessite systemd et les outils de compilation installés plus haut.

## 6. Installer le collecteur Linux

Dans le terminal Linux, place-toi dans le dossier extrait qui contient **install-debian.sh**, **Cargo.toml** et **Cargo.lock**. Si tu as suivi la commande git plus haut, saisis :

```bash
cd ~/ServeurMonn/serveur
bash install-debian.sh
```

Réponds aux questions de l’assistant :

1. **IP ou nom DNS du PC Windows** : l’adresse IPv4 Windows trouvée avec ipconfig, par exemple **192.168.1.25**. N’indique pas 0.0.0.0.
2. **Port TCP** : le même port que sous Windows, normalement **9050**.
3. **Nom TLS** : **localhost**, car c’est le nom choisi à l’étape Windows et celui du certificat.
4. **Chemin du certificat** : **/tmp/receiver-cert.pem**.
5. **Identifiant et mot de passe** : exactement ceux choisis dans l’installateur Windows. La saisie du mot de passe est masquée.
6. **Intervalle** : **1** seconde est une bonne valeur pour commencer.

Relis le résumé et confirme. Le script compile le collecteur, le copie dans **/usr/local/bin**, enregistre les paramètres dans **/etc/serveur-monitor/monitor.env** et crée le service systemd **serveur-monitor**. Le dossier target est créé automatiquement pendant la compilation ; il n’a pas besoin d’être téléchargé depuis GitHub.

À la fin, vérifie le service :

```bash
sudo systemctl status serveur-monitor
sudo journalctl -u serveur-monitor -n 30 --no-pager
```

Le tableau de bord est sur le PC Windows, dans son navigateur, à **http://127.0.0.1:8080/**. Il affichera les mesures après la première connexion du collecteur.

## 7. Modifier les paramètres plus tard

### Modifier les paramètres Linux

Relance l’assistant dans le dossier du projet :

```bash
cd ~/ServeurMonn/serveur
bash install-debian.sh
```

Il sauvegarde l’ancienne configuration avant de la remplacer. Après l’installation, redémarre le service pour appliquer les nouvelles valeurs :

```bash
sudo systemctl restart serveur-monitor
```

Si tu modifies l’identifiant ou le mot de passe, les valeurs doivent toujours correspondre à Windows.

### Modifier les paramètres Windows

1. Dans la fenêtre du récepteur, appuie sur **Ctrl+C** pour l’arrêter.
2. Relance **monitor-installer.exe** depuis le dossier extrait, avec **monitor-receiver.exe** à côté.
3. Relance le fichier installé **%LOCALAPPDATA%\Programs\ServeurMonitor\start-monitor.cmd**.

Si tu changes le port ou le mot de passe Windows, relance aussi l’installateur Linux et redémarre son service. Si tu changes le nom TLS, il faut générer un nouveau certificat, le recopier sur Linux et relancer l’installation Linux. Pour éviter cette manipulation, garde **localhost** comme nom TLS.

## 8. Dépannage

### Windows affiche « Authentification refusée »

L’identifiant ou le mot de passe Linux ne correspond pas à ceux configurés sous Windows. Vérifie aussi que tu as démarré le lanceur installé **start-monitor.cmd**, pas monitor-receiver.exe depuis le dossier extrait. Si le terminal Windows affiche « identifiants par défaut admin/root », le récepteur en cours utilise ces valeurs par défaut.

Une ancienne version de install-debian.sh capturait aussi les retours à la ligne affichés pendant la saisie masquée et les ajoutait au mot de passe. Si tu as installé depuis une ancienne archive et que l’authentification échoue encore, télécharge le ZIP actualisé et relance l’installation Linux pour enregistrer à nouveau le mot de passe.

### Le collecteur ne peut pas joindre Windows

Vérifie que l’adresse saisie sur Linux est l’IPv4 Windows, que le récepteur affiche « En attente des connexions sur le port 9050 », que le port est autorisé dans le pare-feu Windows et que les deux machines peuvent communiquer sur le réseau.

### Erreur de certificat ou de nom TLS

Le nom TLS saisi sur Linux doit être identique au nom TLS choisi dans l’installateur Windows. Avec ce guide, c’est **localhost**. Le fichier fourni à Linux doit être le certificat créé par le récepteur Windows actuellement lancé. Si tu régénères le certificat, transfère le nouveau receiver-cert.pem.

### Erreur « lock file version 4 »

Cargo est trop ancien. Installe Rust stable avec rustup en suivant l’étape 5, puis relance install-debian.sh.

## Connexion entre réseaux différents

Les adresses privées comme 192.168.1.25 ne sont joignables que depuis le réseau local ou un VPN. Pour une connexion depuis Internet, il faut aussi configurer le routeur pour transférer le port TCP choisi vers Windows, autoriser ce port dans le pare-feu et utiliser une adresse publique ou un nom DNS accessible depuis Linux. Le nom TLS doit correspondre au nom choisi dans le certificat. Utilise des identifiants personnalisés et limite les connexions entrantes à l’adresse du serveur Linux si possible.
