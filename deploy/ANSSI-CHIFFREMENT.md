# ki-chat — informations techniques sur la cryptologie (déclaration ANSSI)

Ce document rassemble, tirés du code (version 0.1.59), les éléments techniques
que demande une déclaration de fourniture ou d'importation d'un moyen de
cryptologie auprès de l'ANSSI, et qu'Apple exige pour distribuer l'appli iOS
en France (App Store Connect → conformité à l'exportation).

Il ne remplace pas la lecture du formulaire de l'ANSSI ni un avis juridique :
c'est la matière technique à y reporter.

---

## 1. Le produit

| | |
|---|---|
| **Nom** | ki-chat |
| **Nature** | Messagerie de groupe privée : chat texte, vocal, partage d'écran, partage de photos et vidéos |
| **Composants** | Un serveur (auto-hébergé par l'administrateur du groupe) ; des clients Windows, macOS, Android et iOS |
| **Distribution iOS** | TestFlight puis App Store, identifiant `fun.baws.kichat`, éditeur DRION KLLOKOQI (équipe Apple `9NK7XZ7V9K`) |
| **Public** | Grand public, petits groupes d'amis (~30 personnes par serveur) |
| **Gratuité** | Gratuit, code sous licence MIT |
| **Langages et bibliothèques** | Rust ; bibliothèques cryptographiques publiques et largement diffusées (voir § 3) |

## 2. Fonctions de cryptologie

ki-chat utilise la cryptologie **uniquement pour protéger ses propres
communications** entre client et serveur :

1. **Confidentialité et intégrité du transport** : toutes les communications
   (contrôle, chat, voix, vidéo, fichiers) passent par TLS 1.3, en QUIC ou en
   HTTPS.
2. **Seconde couche sur la voix et la vidéo** : les paquets audio et vidéo
   sont aussi scellés en XChaCha20-Poly1305 avec une clé de salon distribuée
   par le serveur.
3. **Authentification** :
   - du serveur, par épinglage de l'empreinte de son certificat ;
   - de l'utilisateur, par mot de passe haché côté serveur ;
   - des mises à jour (PC et Android), par signature.

Ce que ki-chat **ne fait pas** :

- **Pas de chiffrement de bout en bout.** Le serveur détient les clés de
  salon : il en a besoin pour le bot musique et pour les invités web.
- **Pas de chiffrement de fichiers ou de données arbitraires** à la demande
  de l'utilisateur. L'utilisateur ne choisit ni les algorithmes, ni les clés,
  ni ce qui est chiffré.
- **Aucun algorithme propriétaire ou maison.**
- **Pas de chiffrement des données au repos** par l'appli. Sur iOS, le mot
  de passe retenu est confié au Trousseau du système, chiffré par iOS
  lui-même.

## 3. Algorithmes, longueurs de clé, bibliothèques

| Usage | Algorithme | Clé | Bibliothèque |
|---|---|---|---|
| Transport (QUIC et HTTPS) | TLS 1.3 uniquement | — | rustls 0.23, fournisseur *ring* 0.17 ; QUIC : quinn 0.11 |
| ↳ suites de chiffrement | AES-256-GCM-SHA384, AES-128-GCM-SHA256, ChaCha20-Poly1305-SHA256 | 256 / 128 / 256 bits | ring |
| ↳ échange de clés | ECDHE : X25519, NIST P-256, NIST P-384 | 255 / 256 / 384 bits | ring |
| ↳ certificat du serveur | ECDSA P-256 avec SHA-256, auto-signé, généré par le serveur | 256 bits | rcgen |
| Épinglage du serveur | Empreinte SHA-256 du certificat, retenue à la première connexion | — | sha2 0.10 |
| Voix et vidéo (2ᵉ couche) | XChaCha20-Poly1305 (AEAD), nonce de 192 bits dérivé de l'émetteur et d'un compteur | 256 bits | chacha20poly1305 0.10 (RustCrypto) |
| Mots de passe (serveur) | Argon2id, format PHC, sel aléatoire | — | argon2 0.5 |
| Signature des mises à jour (PC, Android) | Ed25519 sur un manifeste contenant le SHA-256 du fichier | 256 bits | ed25519-dalek 2.2 |
| Stockage du mot de passe (iOS) | Trousseau iOS (chiffrement du système d'exploitation) | — | Security.framework d'Apple |

**Normes de référence :**

| Algorithme | Norme |
|---|---|
| TLS 1.3 | RFC 8446 |
| QUIC | RFC 9000 / 9001 |
| AES-GCM | NIST SP 800-38D |
| ChaCha20-Poly1305 | RFC 8439 |
| XChaCha20 | draft-irtf-cfrg-xchacha |
| X25519, Ed25519 | RFC 7748 / RFC 8032 |
| ECDSA P-256 / P-384 | FIPS 186 |
| SHA-256 | FIPS 180-4 |
| Argon2 | RFC 9106 |

## 4. Gestion des clés

- **TLS** : clés de session éphémères, négociées par ECDHE à chaque
  connexion (confidentialité persistante).
- **Certificat du serveur** : généré une fois par le serveur. Le client
  retient son empreinte à la première connexion et refuse ensuite tout autre
  certificat.
- **Clé de salon (voix et vidéo)** : 256 bits, tirés au démarrage du
  serveur par un générateur aléatoire cryptographique (`rand::rng()`,
  ChaCha12 alimenté par l'aléa du système). Elle est transmise aux clients
  dans le message d'accueil, par le canal TLS, une fois l'utilisateur
  authentifié ; le client la garde en mémoire.
- **Mot de passe** : transmis au serveur dans le canal TLS. Le serveur n'en
  conserve que le hachage Argon2id.
- **Aucun séquestre, aucune clé maître** côté éditeur : chaque serveur est
  autonome et appartient à celui qui l'héberge.

## 5. Démarches

- **ANSSI** : déclaration de fourniture ou d'importation d'un moyen de
  cryptologie, sur le site de l'ANSSI, rubrique « Contrôle réglementaire sur
  la cryptologie ». Le formulaire et le délai d'instruction sont à vérifier
  sur place.
- **Apple** : téléverser le récépissé de l'ANSSI dans App Store Connect
  (conformité à l'exportation), puis ajouter la France dans « Tarifs et
  disponibilité ».
- **Mise à jour du document** : si les algorithmes changent (nouvelle
  bibliothèque, nouvelle suite), mettre ce document à jour et vérifier s'il
  faut une nouvelle déclaration.
