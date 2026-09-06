#!/bin/sh
# Assemble le paquet macOS de ki-chat et ce qui se publie avec.
#
#   installer/macos/build-app.sh [exécutable]
#
# Sans argument, prend `target/release/ki-chat` (la compilation native) ; le
# workflow de release lui passe l'exécutable universel qu'il vient de
# fabriquer avec `lipo`. Produit dans `installer/Output/` :
#
#   ki-chat.app             le paquet, prêt à glisser dans Applications
#   ki-chat-macos.tar.gz    le même, archivé : c'est l'actif que le client
#                           télécharge pour se mettre à jour (update.rs)
#   ki-chat-macos.pkg       l'assistant d'installation, pour qui préfère
#                           double-cliquer (voir Distribution.xml)
#
# Rien ici ne demande Xcode : `iconutil`, `codesign`, `pkgbuild` et
# `productbuild` sont livrés avec macOS.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)
BIN=${1:-$ROOT/target/release/ki-chat}
OUT=${OUT:-$ROOT/installer/Output}
APP="$OUT/ki-chat.app"
ID=com.github.redik123.ki-chat

[ -x "$BIN" ] || { echo "exécutable introuvable : $BIN" >&2; exit 1; }
[ -n "$VERSION" ] || { echo "version introuvable dans Cargo.toml" >&2; exit 1; }
echo "ki-chat $VERSION, depuis $BIN"

# ---- le paquet ---------------------------------------------------------
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/ki-chat"
chmod 755 "$APP/Contents/MacOS/ki-chat"
sed "s/@VERSION@/$VERSION/g" "$ROOT/installer/macos/Info.plist" > "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# L'icône : build.rs a rendu un `.iconset` dans OUT_DIR, dont le nom porte
# un hachage — on le retrouve au lieu de le deviner, comme le workflow
# Windows retrouve son `.ico`. Absent (compilation sans ce build.rs), le
# paquet part sans icône : ça ne vaut pas un échec.
ICONSET=$(find "$ROOT/target" -type d -name ki-chat.iconset 2>/dev/null | head -1)
if [ -n "$ICONSET" ]; then
    iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/ki-chat.icns"
else
    echo "avertissement : aucun ki-chat.iconset sous target/, paquet sans icône" >&2
fi

# Signature ad hoc : pas d'identité Apple (un certificat de développeur
# coûte 99 $/an), mais les puces Apple refusent de lancer un exécutable qui
# n'a aucune signature, même sans identité. `lipo` ayant recollé deux
# tranches, on signe le paquet fini, d'un bloc.
codesign --force --sign - --timestamp=none "$APP"

# ---- l'archive (actif de mise à jour) ------------------------------------
# COPYFILE_DISABLE : sans lui, tar y glisse des fichiers `._*` (métadonnées
# HFS) que le déballage côté client n'a que faire.
rm -f "$OUT/ki-chat-macos.tar.gz"
( cd "$OUT" && COPYFILE_DISABLE=1 tar -czf ki-chat-macos.tar.gz ki-chat.app )

# ---- l'assistant d'installation -----------------------------------------
PKGROOT="$OUT/pkgroot"
rm -rf "$PKGROOT"
mkdir -p "$PKGROOT"
cp -R "$APP" "$PKGROOT/"

# `BundleIsRelocatable` : par défaut, l'installeur *cherche* une copie déjà
# posée n'importe où sur le disque et la met à jour là où elle est, au lieu
# d'installer où on lui dit. Désactivé : on installe dans ~/Applications,
# point.
pkgbuild --analyze --root "$PKGROOT" "$OUT/component.plist" >/dev/null
plutil -replace BundleIsRelocatable -bool NO "$OUT/component.plist"
pkgbuild --root "$PKGROOT" \
    --component-plist "$OUT/component.plist" \
    --identifier "$ID" \
    --version "$VERSION" \
    --install-location /Applications \
    "$OUT/ki-chat-component.pkg" >/dev/null
sed "s/@VERSION@/$VERSION/g" "$ROOT/installer/macos/Distribution.xml" > "$OUT/Distribution.xml"
productbuild --distribution "$OUT/Distribution.xml" \
    --package-path "$OUT" \
    "$OUT/ki-chat-macos.pkg" >/dev/null
rm -rf "$PKGROOT" "$OUT/component.plist" "$OUT/Distribution.xml" "$OUT/ki-chat-component.pkg"

echo "produit dans $OUT :"
ls -la "$OUT/ki-chat-macos.tar.gz" "$OUT/ki-chat-macos.pkg"
