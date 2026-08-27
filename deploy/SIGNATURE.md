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

**La vérification est écrite mais pas encore armée.** Tant que
`RELEASE_PUBKEY_HEX` est vide dans `crates/client-gui/src/update.rs`, le client
consigne « mise à jour non vérifiée » dans ses traces et poursuit — exactement
le comportement d'avant, ni meilleur ni pire.

C'est délibéré : armer la vérification avant que la chaîne de publication ne
signe couperait toute mise à jour, y compris celle qui apporterait le
correctif.

## Mettre en place, une fois

### 1. Fabriquer la paire de clés

Sur ta machine, hors du dépôt :

```bash
# Clé privée : 32 octets aléatoires, en hexadécimal.
openssl rand -hex 32 > ki-release.key
```

La clé publique se déduit de la privée. Le plus simple est de la faire calculer
par le même outil qui signera — voir l'étape 3, qui l'imprime.

> **Cette clé privée ne se retrouve pas.** Perdue, il faut regraver une
> nouvelle clé publique dans le client, donc publier une version que les
> anciennes installations refuseront de vérifier… et qu'elles installeront
> quand même, puisqu'elles portent l'ancienne clé. Sauvegarde-la comme tu
> sauvegardes `data/quic-key.der`.

### 2. La déposer dans GitHub

*Settings* → *Secrets and variables* → *Actions* → *New repository secret* :

| Nom | Contenu |
| :--- | :--- |
| `RELEASE_SIGNING_KEY` | le contenu de `ki-release.key` (64 caractères hexadécimaux) |

### 3. Graver la clé publique dans le client

L'étape de signature de `release.yml` imprime la clé publique correspondante
dans le journal du workflow, à chaque exécution. Lance-le une fois
(*Actions* → *release* → *Run workflow*), relève la ligne
`clé publique : …`, et colle-la dans `crates/client-gui/src/update.rs` :

```rust
const RELEASE_PUBKEY_HEX: &str = "collée ici";
```

Puis publie une version. **À partir de cette version**, les clients vérifient.

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
