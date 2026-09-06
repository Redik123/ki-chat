#!/bin/sh
# Installe (ou met à jour) ki-chat sur un Mac, en une ligne :
#
#   curl -fsSL https://raw.githubusercontent.com/Redik123/ki-chat/main/installer/macos/install.sh | sh
#
# Télécharge la dernière release, pose `ki-chat.app` dans ~/Applications et
# la lance. Pas de mot de passe : rien ne touche à /Applications ni au
# système. Le dossier est celui de l'utilisateur, donc l'application pourra
# s'y mettre à jour toute seule — comme sur Windows, où l'installeur la pose
# dans le profil et pas dans « Program Files ».
#
# Pourquoi un script plutôt que l'archive à double-cliquer : ce que `curl`
# télécharge ne porte pas la marque de quarantaine du navigateur, donc
# Gatekeeper ne s'en mêle pas. Le même paquet téléchargé depuis Safari
# afficherait « Apple ne peut pas vérifier… » — le binaire n'a pas
# d'identité Apple (99 $/an), l'équivalent du SmartScreen de Windows.
#
#   KI_CHAT_DIR=/autre/dossier   installe ailleurs que ~/Applications
#   KI_CHAT_NO_LAUNCH=1          n'ouvre pas l'application à la fin
set -eu

REPO=Redik123/ki-chat
DEST=${KI_CHAT_DIR:-$HOME/Applications}
URL="https://github.com/$REPO/releases/latest/download/ki-chat-macos.tar.gz"

case "$(uname -s)" in
    Darwin) ;;
    *) echo "ce script installe ki-chat sur macOS seulement" >&2; exit 1 ;;
esac

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "téléchargement de la dernière version…"
curl -fL --progress-bar "$URL" -o "$TMP/ki-chat.tar.gz"
tar -xzf "$TMP/ki-chat.tar.gz" -C "$TMP"
[ -x "$TMP/ki-chat.app/Contents/MacOS/ki-chat" ] || {
    echo "l'archive téléchargée ne contient pas ki-chat.app" >&2; exit 1
}

mkdir -p "$DEST"
# Remplacement par renommage, comme le fait la mise à jour automatique :
# une instance qui tourne continue sur l'ancien inode, et la prochaine
# ouverture prend la nouvelle version.
if [ -d "$DEST/ki-chat.app" ]; then
    rm -rf "$DEST/ki-chat.app.old"
    mv "$DEST/ki-chat.app" "$DEST/ki-chat.app.old"
fi
mv "$TMP/ki-chat.app" "$DEST/ki-chat.app"
rm -rf "$DEST/ki-chat.app.old"
# Par précaution : curl ne pose pas la quarantaine, mais un proxy ou un
# outil intermédiaire pourrait.
xattr -dr com.apple.quarantine "$DEST/ki-chat.app" 2>/dev/null || true

VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
    "$DEST/ki-chat.app/Contents/Info.plist" 2>/dev/null || echo "?")
echo "ki-chat $VERSION installé dans $DEST/ki-chat.app"

if [ -z "${KI_CHAT_NO_LAUNCH:-}" ]; then
    open "$DEST/ki-chat.app"
fi
