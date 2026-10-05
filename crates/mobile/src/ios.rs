//! iOS : ce que `KiAndroid` (Kotlin) fait pour la page sur Android, fait ici
//! en Rust.
//!
//! La page appelle `window.KiIos`, posé par le script d'amorce ci-dessous
//! avant qu'elle ne se charge, avec les mêmes méthodes que `KiAndroid` :
//!
//! - `service(mode, texte)` : sur iOS, pas de service au premier plan. Le
//!   vocal écran éteint tient par le mode d'arrière-plan « audio »
//!   (Info.ios.plist) tant que la session audio est active ; « connecté »
//!   seul n'existe pas (iOS suspend l'appli), la page ne le propose pas ;
//! - `retenirSecret`, `relireSecret`, `oublierSecret` : le Trousseau. Seule
//!   différence avec Android : `relireSecret` rend une promesse ;
//! - `notifier`, `effacerNotifications` : notifications locales.
//!
//! Hors iOS, les commandes existent (le gestionnaire de Tauri les liste
//! toutes) mais ne font rien, et le script n'est pas posé.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::Runtime;

/// Le pont, côté page. `__TAURI__` n'existe pas encore quand ce script
/// tourne : on le cherche à chaque appel.
#[cfg(target_os = "ios")]
const AMORCE: &str = r#"
(() => {
  const appel = (cmd, args) => window.__TAURI__.core.invoke(cmd, args);
  const sansSuite = (cmd, args) => { appel(cmd, args).catch((e) => console.warn(cmd, e)); };
  window.KiIos = {
    service: (mode, texte) => sansSuite("ios_service", { mode, texte }),
    retenirSecret: (nom, valeur) => sansSuite("ios_retenir_secret", { nom, valeur }),
    relireSecret: (nom) => appel("ios_relire_secret", { nom }).catch(() => null),
    oublierSecret: (nom) => sansSuite("ios_oublier_secret", { nom }),
    notifier: (salon, titre, texte, mention) => sansSuite("ios_notifier", { salon, titre, texte, mention }),
    effacerNotifications: () => sansSuite("ios_effacer_notifications", {}),
  };
})();
"#;

/// Le greffon qui pose `window.KiIos` (iOS seulement) et prépare l'audio et
/// les notifications au lancement.
pub fn greffon<R: Runtime>() -> TauriPlugin<R> {
    let b = Builder::new("ki-ios");
    #[cfg(target_os = "ios")]
    let b = b.js_init_script(AMORCE.to_string()).setup(|_app, _api| {
        apple::session_audio();
        apple::demander_notifications();
        Ok(())
    });
    b.build()
}

#[tauri::command]
pub fn ios_service(mode: String, texte: String) {
    let _ = &texte;
    #[cfg(target_os = "ios")]
    apple::activer_audio(mode == "vocal");
    #[cfg(not(target_os = "ios"))]
    let _ = mode;
}

#[tauri::command]
pub fn ios_retenir_secret(nom: String, valeur: String) {
    #[cfg(target_os = "ios")]
    if let Err(e) = security_framework::passwords::set_generic_password(
        apple::SERVICE_TROUSSEAU,
        &nom,
        valeur.as_bytes(),
    ) {
        tracing::warn!("Trousseau : {nom} non retenu : {e}");
    }
    #[cfg(not(target_os = "ios"))]
    let _ = (nom, valeur);
}

#[tauri::command]
pub fn ios_relire_secret(nom: String) -> Option<String> {
    #[cfg(target_os = "ios")]
    {
        security_framework::passwords::get_generic_password(apple::SERVICE_TROUSSEAU, &nom)
            .ok()
            .and_then(|v| String::from_utf8(v).ok())
    }
    #[cfg(not(target_os = "ios"))]
    {
        let _ = nom;
        None
    }
}

#[tauri::command]
pub fn ios_oublier_secret(nom: String) {
    #[cfg(target_os = "ios")]
    let _ = security_framework::passwords::delete_generic_password(apple::SERVICE_TROUSSEAU, &nom);
    #[cfg(not(target_os = "ios"))]
    let _ = nom;
}

#[tauri::command]
pub fn ios_notifier(salon: u32, titre: String, texte: String, mention: bool) {
    #[cfg(target_os = "ios")]
    apple::notifier(salon, &titre, &texte);
    let _ = mention;
    #[cfg(not(target_os = "ios"))]
    let _ = (salon, titre, texte, mention);
}

#[tauri::command]
pub fn ios_effacer_notifications() {
    #[cfg(target_os = "ios")]
    objc2_user_notifications::UNUserNotificationCenter::currentNotificationCenter()
        .removeAllDeliveredNotifications();
}

#[cfg(target_os = "ios")]
mod apple {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_avf_audio::{
        AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayAndRecord,
        AVAudioSessionModeDefault, AVAudioSessionSetActiveOptions,
    };
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent,
        UNNotificationRequest, UNNotificationSound, UNUserNotificationCenter,
    };

    /// Le « service » des entrées du Trousseau : l'identifiant de l'appli.
    pub const SERVICE_TROUSSEAU: &str = "fun.baws.kichat";

    /// Par défaut, la session audio d'une appli iOS est en lecture seule
    /// (SoloAmbient) : le micro ne rend que du silence, et le son se coupe
    /// écran verrouillé. PlayAndRecord ouvre le micro ; le haut-parleur
    /// plutôt que l'écouteur d'oreille (on tient le téléphone devant soi, pas
    /// contre la joue) ; casques Bluetooth acceptés, micro compris.
    pub fn session_audio() {
        // SAFETY : appels Objective-C sur l'instance partagée, paramètres
        // valides ; les constantes sont fournies par AVFAudio.
        unsafe {
            let session = AVAudioSession::sharedInstance();
            let (Some(categorie), Some(mode)) =
                (AVAudioSessionCategoryPlayAndRecord, AVAudioSessionModeDefault)
            else {
                return;
            };
            let options = AVAudioSessionCategoryOptions::DefaultToSpeaker
                | AVAudioSessionCategoryOptions::AllowBluetoothHFP
                | AVAudioSessionCategoryOptions::AllowBluetoothA2DP;
            if let Err(e) = session.setCategory_mode_options_error(categorie, mode, options) {
                tracing::warn!("session audio : catégorie refusée : {e:?}");
            }
        }
    }

    /// En vocal : la session active, c'est elle qui garde l'appli vivante
    /// écran éteint. En sortant, on la rend, et la musique d'une autre appli
    /// peut reprendre.
    pub fn activer_audio(oui: bool) {
        // SAFETY : comme ci-dessus.
        unsafe {
            let session = AVAudioSession::sharedInstance();
            let r = if oui {
                session.setActive_error(true)
            } else {
                session.setActive_withOptions_error(
                    false,
                    AVAudioSessionSetActiveOptions::NotifyOthersOnDeactivation,
                )
            };
            if let Err(e) = r {
                tracing::debug!("session audio active={oui} : {e:?}");
            }
        }
    }

    /// L'accord pour les notifications, demandé une fois (iOS ne repose plus
    /// la question ensuite : c'est dans les Réglages).
    pub fn demander_notifications() {
        let centre = UNUserNotificationCenter::currentNotificationCenter();
        let fin = RcBlock::new(|accorde: Bool, _err: *mut NSError| {
            tracing::info!("notifications : accord {}", accorde.as_bool());
        });
        centre.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &fin,
        );
    }

    /// Une notification de message, une par salon (la dernière remplace,
    /// comme sur Android : même identifiant).
    pub fn notifier(salon: u32, titre: &str, texte: &str) {
        let contenu = UNMutableNotificationContent::new();
        contenu.setTitle(&NSString::from_str(titre));
        contenu.setBody(&NSString::from_str(texte));
        contenu.setThreadIdentifier(&NSString::from_str(&format!("salon-{salon}")));
        contenu.setSound(Some(&UNNotificationSound::defaultSound()));
        let requete = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&format!("salon-{salon}")),
            &contenu,
            None,
        );
        UNUserNotificationCenter::currentNotificationCenter()
            .addNotificationRequest_withCompletionHandler(&requete, None);
    }
}
