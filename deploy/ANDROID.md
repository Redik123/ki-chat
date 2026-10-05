# L'appli Android : clé, release, installation

## La clé de signature Android

Android n'accepte une mise à jour que si elle est signée avec **la même clé**
que l'appli installée. Perdre cette clé, c'est obliger chaque téléphone à
désinstaller ki-chat (réglages et mot de passe retenu perdus) avant de
réinstaller ; se la faire voler, c'est permettre à quelqu'un d'autre de
publier une « mise à jour ».

- Le fichier : `D:\DEV\KI-Chat-cles\ki-chat-android.jks`, alias `ki-chat`,
  RSA 4096, valable cent ans. Son mot de passe est à côté, dans
  `ki-chat-android.mdp`. **Hors du dépôt**, exprès.
- Empreinte SHA-256 du certificat :
  `5A:4F:49:B0:F0:99:37:65:8B:4B:B5:44:42:7C:A2:96:28:ED:66:12:B5:0C:18:A1:FB:7E:43:94:34:59:DF:94`
- **Une copie de secours** des deux fichiers, ailleurs que sur ce PC (clé USB
  rangée, gestionnaire de mots de passe). C'est la seule chose qui ne se
  refait pas.

Pour compiler une version signée sur le PC, `crates/mobile/gen/android/keystore.properties`
(ignoré par Git) dit où est la clé :

```
storeFile=D:/DEV/KI-Chat-cles/ki-chat-android.jks
keyAlias=ki-chat
password=<le contenu de ki-chat-android.mdp>
```

## Les secrets du dépôt

La release (`.github/workflows/release.yml`, travail `android`) signe l'APK
avec deux secrets de l'environnement `release` :

- `ANDROID_KEYSTORE_B64` : le fichier `.jks` en base64 ;
- `ANDROID_KEYSTORE_PASSWORD` : son mot de passe.

```
base64 -w0 D:/DEV/KI-Chat-cles/ki-chat-android.jks | gh secret set ANDROID_KEYSTORE_B64 --env release
gh secret set ANDROID_KEYSTORE_PASSWORD --env release < D:/DEV/KI-Chat-cles/ki-chat-android.mdp
```

Le manifeste de l'APK est en plus signé par la clé Ed25519 des releases
(`RELEASE_SIGNING_KEY`, voir SIGNATURE.md), comme les actifs du PC : l'appli
le vérifie avant de proposer l'installation.

## Monter la version

Trois endroits, que la release vérifie : `Cargo.toml` (le dépôt),
`crates/mobile/Cargo.toml` et `crates/mobile/tauri.conf.json`.

## Installer la première fois

L'APK de la dernière release, toujours à la même adresse :
https://github.com/Redik123/ki-chat/releases/latest/download/ki-chat-android.apk

Le téléphone demande d'autoriser l'installation depuis le navigateur ; ensuite
l'appli se met à jour d'elle-même (un appui sur « Mettre à jour »). Un APK
d'essai (signé par la clé de débogage) doit être désinstallé une fois avant :
Android refuse de mélanger les deux signatures.
