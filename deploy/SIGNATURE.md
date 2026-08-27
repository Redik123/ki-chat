# Signer les mises à jour

L'application **remplace son propre exécutable**. Jusqu'à la mise en place
décrite ici, la seule garantie d'intégrité était TLS jusqu'à GitHub : quiconque
obtenait le droit de publier une release — compte compromis, jeton d'action
fuité, actif remplacé après coup — exécutait du code arbitraire sur les
machines de tout le monde. Le contrôle de taille qui existait n'attrape qu'un
téléchargement tronqué, pas un binaire hostile.

Une signature Ed25519 ferme cette porte. La clé **privée** ne quitte jamais ta
machine et le coffre de GitHub ; la **publique** est gravée dans le binaire déjà
installé chez les gens. Un attaquant qui contrôle les releases ne peut alors
plus signer : il ne peut que faire échouer la mise à jour, ce qui se voit.

## État actuel

**Armé depuis la version 0.1.12.** Le secret `RELEASE_SIGNING_KEY` est déposé
dans le dépôt, la clé publique correspondante est gravée dans
`crates/client-gui/src/update.rs`, et chaque release publie `ki-chat.exe.sig`
à côté de l'exécutable.

À partir de 0.1.12, un client refuse d'installer une mise à jour qu'il ne peut
pas vérifier, et le dit. Les installations plus anciennes (0.1.11 et avant) ne
portent pas la clé : elles continuent d'installer sans vérifier, jusqu'à ce
qu'elles passent en 0.1.12.

> **La clé privée ne se retrouve pas.** Perdue, il faudrait graver une nouvelle
> clé publique dans le client — donc publier une version que les installations
> existantes refuseraient de vérifier, et qu'elles n'installeraient donc
> jamais. Sauvegarde-la comme tu sauvegardes `data/quic-key.der`.

## Comment cela a été mis en place

Pour mémoire, et pour le jour où il faudra recommencer sur un autre dépôt.

### 1. Fabriquer la clé privée

Sur ta machine, hors du dépôt. En PowerShell, sans outil externe — c'est le
générateur cryptographique de Windows, celui qui sert à TLS :

```powershell
$b = New-Object byte[] 32
(New-Object System.Security.Cryptography.RNGCryptoServiceProvider).GetBytes($b)
(($b | ForEach-Object { $_.ToString('x2') }) -join '') |
  Set-Content "$env:USERPROFILE\ki-release.key" -Encoding ascii -NoNewline
```

Avec `openssl` sous la main, `openssl rand -hex 32` fait la même chose. Il
n'est pas dans le `PATH` de `cmd.exe` sur une installation Git pour Windows
ordinaire — d'où la variante ci-dessus.

### 2. La déposer dans GitHub

*Settings* → *Secrets and variables* → *Actions* → *New repository secret* :

| Nom | Contenu |
| :--- | :--- |
| `RELEASE_SIGNING_KEY` | le contenu de `ki-release.key` (64 caractères hexadécimaux) |

### 3. Graver la clé publique dans le client

Le signeur imprime la clé publique à chaque exécution — inutile de la dériver à
la main. En local :

```powershell
$env:SIGNING_KEY = (Get-Content "$env:USERPROFILE\ki-release.key" -Raw).Trim()
cargo run -p ki-client-gui --example signer -- Cargo.toml "$env:TEMP\essai.sig"
```

Relève la ligne `clé publique : …` et colle-la dans
`crates/client-gui/src/update.rs` :

```rust
const RELEASE_PUBKEY_HEX: &str = "collée ici";
```

**L'ordre compte.** Le secret doit être dans GitHub *avant* de publier la
version qui porte la clé publique : dans le cas contraire, on livre des clients
qui exigent une signature que la chaîne ne produit pas encore, et ils refusent
alors toutes les mises à jour suivantes — y compris celle qui corrigerait le
problème.

## Ce qui se passe ensuite

- Chaque release porte `ki-chat.exe` **et** `ki-chat.exe.sig`.
- Le client télécharge les deux, vérifie, et n'installe que si ça correspond.
- Une signature absente ou fausse fait échouer la mise à jour avec un message
  explicite, sans rien remplacer.

## Ce que ça ne protège pas

- **L'installation initiale.** `ki-chat-setup.exe` est téléchargé à la main
  depuis GitHub : c'est TLS et rien d'autre. Un certificat de signature de code
  Windows y répondrait, mais il coûte quelques centaines d'euros par an et
  supprimerait au passage l'avertissement SmartScreen.
- **Le serveur.** L'image Docker se vérifie par son étiquette et son empreinte
  de registre, ce qui est un autre sujet.
