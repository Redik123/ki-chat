//! Empêche la mise en veille du système pendant qu'on est en salon vocal.
//!
//! Pour Windows, « actif » veut dire clavier ou souris : parler ne compte
//! pas. Un portable dont on ne touche pas le clavier — on discute, on écoute
//! — atteint donc son délai d'inactivité en pleine conversation et s'endort,
//! coupant la voix au passage. Les applications d'appel le déclarent
//! explicitement ; nous ne le faisions pas.
//!
//! L'écran garde le droit de s'éteindre : `ES_DISPLAY_REQUIRED` n'est pas
//! demandé, seule la machine doit rester debout. Et l'interdiction ne vaut
//! qu'en vocal : hors salon, ki-chat ouvert ne retient rien — une
//! application qui empêche un portable de dormir en permanence vide des
//! batteries sans avoir rien à y gagner.

/// Suit ce qui a été demandé à Windows, pour ne l'appeler qu'aux transitions.
///
/// `ES_CONTINUOUS` est porté par le **fil appelant** : tous les appels
/// doivent venir du même fil. C'est le cas — `actualiser` est appelée depuis
/// `update`, donc toujours du fil de l'interface, qui vit aussi longtemps que
/// l'application. À la fin du processus, Windows lève l'état de lui-même.
#[derive(Default)]
pub struct Garde {
    tenu: bool,
}

impl Garde {
    /// À appeler à chaque image : pose l'interdiction de veille en entrant en
    /// vocal, la lève en sortant. Windows n'est sollicité qu'aux changements.
    pub fn actualiser(&mut self, en_vocal: bool) {
        if en_vocal == self.tenu {
            return;
        }
        self.tenu = en_vocal;
        appliquer(en_vocal);
    }
}

#[cfg(windows)]
fn appliquer(actif: bool) {
    use windows::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };
    let etat = if actif {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    // SAFETY : pas de pointeur ni de ressource — l'appel règle un drapeau
    // d'alimentation attaché au fil courant.
    let precedent = unsafe { SetThreadExecutionState(etat) };
    if precedent.0 == 0 {
        // Refus rarissime ; on le note et la vie continue — au pire, on
        // retrouve le comportement d'avant.
        tracing::warn!("interdiction de veille refusée par Windows");
    } else {
        tracing::info!(
            "{}",
            if actif { "veille système suspendue (salon vocal)" } else { "veille système rendue" }
        );
    }
}

/// macOS : `caffeinate -i` tient la machine éveillée tant qu'il tourne —
/// c'est l'outil du système, celui qui pose l'assertion d'alimentation
/// (`IOPMAssertion`) qu'une application d'appel poserait elle-même. On le
/// lance en entrant en vocal, on l'arrête en sortant ; `-w` le lie à notre
/// processus, pour qu'il ne survive pas à un plantage.
#[cfg(target_os = "macos")]
fn appliquer(actif: bool) {
    use std::sync::Mutex;
    static ENFANT: Mutex<Option<std::process::Child>> = Mutex::new(None);
    let mut garde = ENFANT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut ancien) = garde.take() {
        let _ = ancien.kill();
        let _ = ancien.wait();
    }
    if !actif {
        tracing::info!("veille système rendue");
        return;
    }
    match std::process::Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(enfant) => {
            *garde = Some(enfant);
            tracing::info!("veille système suspendue (salon vocal)");
        }
        // Refus rarissime ; on le note et la vie continue — au pire, on
        // retrouve le comportement d'avant.
        Err(e) => tracing::warn!("caffeinate indisponible : {e}"),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn appliquer(_actif: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// La garde ne doit parler à Windows qu'aux transitions — vérifié par
    /// l'état interne, seul témoin observable sans intercepter l'appel
    /// système. Sous Windows, le test exerce aussi l'appel réel : poser puis
    /// lever l'interdiction sur le fil du test est sans effet durable.
    #[test]
    fn la_garde_suit_les_transitions() {
        let mut garde = Garde::default();
        assert!(!garde.tenu);
        garde.actualiser(true);
        assert!(garde.tenu);
        // Répéter ne change rien : même état, aucun nouvel appel.
        garde.actualiser(true);
        assert!(garde.tenu);
        garde.actualiser(false);
        assert!(!garde.tenu);
    }
}
