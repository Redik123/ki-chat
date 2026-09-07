# Fuzzing du protocole

Le serveur lit ce que n'importe quel client connecté lui envoie ; le client
lit ce qu'un serveur lui répond sans avoir à le croire. Tout cela passe par
`ki-protocol` : les en-têtes binaires des datagrammes, les messages JSON, le
nettoyage des textes, le contrôle des vignettes PNG. Ce harnais leur jette
des millions d'entrées aléatoires, guidées par la couverture (libFuzzer), et
vérifie pour chacune que rien ne panique **et** que les promesses tiennent
(longueurs bornées, aller-retour écrire → lire exact).

Quatre cibles, dans `fuzz_targets/` :

| Cible | Ce qu'elle exerce |
| :--- | :--- |
| `texte` | `clean_chat`, `safe_display`, `excerpt_of`, `clean_emoji`, `hex_decode` |
| `image` | `check_png`, `check_thumbnail` |
| `datagrammes` | en-têtes voix, vidéo (KF) et son du jeu (KA), nonces |
| `messages` | `ClientMsg` et `ServerMsg` en JSON, aller-retour |

## Lancer

Une seule fois : la chaîne nightly (libFuzzer a besoin des sanitizers) et
l'outil.

```bash
rustup toolchain install nightly && cargo install cargo-fuzz
```

Puis, depuis `crates/protocol` :

```bash
cargo fuzz run messages fuzz/corpus/messages fuzz/seeds/messages
```

Ça tourne jusqu'à un plantage ou jusqu'à Ctrl+C. `-- -max_total_time=300`
borne la durée. Le corpus (`fuzz/corpus/`) grossit au fil des exécutions et
n'est pas versionné ; `fuzz/seeds/` contient quelques entrées valides de
départ, elles le sont.

Un plantage laisse un fichier dans `fuzz/artifacts/<cible>/` ; le rejouer :

```bash
cargo fuzz run messages fuzz/artifacts/messages/crash-…
```

## En intégration continue

`ci.yml` fait tourner chaque cible une minute à chaque poussée. Ce n'est pas
pour explorer — le corpus repart de zéro — mais pour qu'une régression sur
une entrée déjà trouvée ne repasse pas. L'exploration, c'est ici, sur une
machine qu'on laisse tourner.
