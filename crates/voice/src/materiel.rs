//! Les réglages matériels du casque et du micro : ce que la carte son où ils
//! sont branchés expose à Windows.
//!
//! Un casque analogique (en jack) n'a rien à régler en lui-même : pas de
//! puce, donc pas de logiciel possible. Tout vit dans la carte son — le
//! volume de la sortie casque, et côté micro deux gains à la suite : le
//! « niveau » et, souvent, une « amplification » (« Ampli microphone »,
//! « Microphone Boost »). C'est leur somme qui fait saturer un micro à
//! condensateur quand on parle fort : +12 dB de niveau et +10 dB
//! d'amplification chez drion, mesurés le 29/09. La page Casque les règle
//! ici, sans passer par le panneau de Windows.
//!
//! Les appels COM vivent sur un fil dédié : l'interface ne peut pas rester
//! suspendue derrière un pilote lent. Elle lit le dernier état relevé,
//! envoie des ordres, et tient le fil éveillé tant que la page est ouverte —
//! il relit alors l'état deux fois par seconde (un réglage changé ailleurs,
//! un casque rebranché), puis se rendort.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Un volume de la carte son, tel que Windows le présente.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Volume {
    /// Le « % » de Windows (0 à 1) — sa courbe n'est pas linéaire en dB.
    pub scalaire: f32,
    pub db: f32,
    pub min_db: f32,
    pub max_db: f32,
    /// Balance, de -1 (tout à gauche) à +1 (tout à droite), sur deux canaux.
    pub balance: Option<f32>,
}

/// Un gain supplémentaire sur le chemin du micro (l'amplification).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Gain {
    /// Le nom que lui donne le pilote (« Ampli microphone »).
    pub nom: String,
    pub db: f32,
    pub min_db: f32,
    pub max_db: f32,
    /// Le pas du réglage (souvent 10 dB) ; 0 s'il est continu.
    pub pas_db: f32,
    /// Son identifiant dans la topologie de la carte, pour le retrouver.
    pub id: String,
}

/// Ce que la carte son expose, au dernier relevé.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EtatMateriel {
    /// Faux hors Windows, et tant que rien n'a été relevé.
    pub disponible: bool,
    /// Le nom Windows de la sortie (« Casque (Realtek USB Audio) »).
    pub sortie_nom: Option<String>,
    pub sortie: Option<Volume>,
    pub entree_nom: Option<String>,
    /// Le niveau du micro.
    pub entree: Option<Volume>,
    /// Les gains en plus du niveau, dans l'ordre du chemin.
    pub amplis: Vec<Gain>,
    /// Les amplifications ont été cherchées : faux au tout premier relevé
    /// d'une carte, publié avant (voir `lire`).
    pub topologie_lue: bool,
    /// Le périphérique demandé est introuvable : ce qui est relevé (et
    /// réglé) est le défaut de Windows, pas lui.
    pub entree_repli: bool,
    pub sortie_repli: bool,
    /// Le dernier ordre que la carte a refusé, en clair — sinon l'interface
    /// affichait « réglé » sur un réglage qui n'avait pas pris.
    pub derniere_erreur: Option<String>,
}

/// Ce que l'interface demande au fil du matériel.
#[derive(Clone, Debug, PartialEq)]
pub enum Ordre {
    /// Les périphériques à suivre, par nom (`None` : le défaut de Windows).
    Suivre { entree: Option<String>, sortie: Option<String> },
    /// Volume du casque, en « % » Windows (0 à 1).
    VolumeSortie(f32),
    /// Balance du casque, de -1 à +1.
    Balance(f32),
    /// Niveau du micro, en « % » Windows (0 à 1).
    NiveauMicro(f32),
    /// Niveau du micro en dB (le calibrage raisonne en dB).
    NiveauMicroDb(f32),
    /// Un gain d'amplification du micro, en dB.
    Ampli { id: String, db: f32 },
    /// Réveil : la page vient de s'ouvrir.
    Eveil,
}

/// Le point d'entrée : l'état relevé, et le canal des ordres.
pub struct Materiel {
    etat: Arc<Mutex<EtatMateriel>>,
    ordres: Option<mpsc::Sender<Ordre>>,
    /// Dernier signe de vie de la page, en ms depuis `depart`.
    eveil: Arc<AtomicU64>,
    depart: Instant,
}

/// Au-delà, la page est considérée fermée : le fil cesse de relire.
const SOMMEIL: Duration = Duration::from_secs(2);

impl Materiel {
    /// L'instance du processus, créée (et son fil lancé) au premier appel.
    pub fn global() -> &'static Materiel {
        static MATERIEL: OnceLock<Materiel> = OnceLock::new();
        MATERIEL.get_or_init(Materiel::demarrer)
    }

    fn demarrer() -> Self {
        let etat = Arc::new(Mutex::new(EtatMateriel::default()));
        let eveil = Arc::new(AtomicU64::new(0));
        let depart = Instant::now();
        #[cfg(windows)]
        let ordres = {
            let (tx, rx) = mpsc::channel();
            let (etat_fil, eveil_fil) = (etat.clone(), eveil.clone());
            std::thread::Builder::new()
                .name("materiel-audio".into())
                .spawn(move || windows_impl::boucle(etat_fil, rx, eveil_fil, depart))
                .ok()
                .map(|_| tx)
        };
        #[cfg(not(windows))]
        let ordres = None;
        Self { etat, ordres, eveil, depart }
    }

    /// Le dernier état relevé.
    pub fn etat(&self) -> EtatMateriel {
        self.etat.lock().unwrap().clone()
    }

    /// Envoie un ordre au fil — et l'inscrit aussitôt dans l'état : un
    /// curseur qu'on glisse ne doit pas revenir en arrière le temps que le
    /// fil l'applique et relise la carte.
    pub fn ordonner(&self, ordre: Ordre) {
        let Some(tx) = &self.ordres else { return };
        {
            let mut e = self.etat.lock().unwrap();
            match &ordre {
                Ordre::VolumeSortie(s) => {
                    if let Some(v) = e.sortie.as_mut() {
                        v.scalaire = s.clamp(0.0, 1.0);
                    }
                }
                Ordre::Balance(b) => {
                    if let Some(v) = e.sortie.as_mut() {
                        v.balance = v.balance.map(|_| b.clamp(-1.0, 1.0));
                    }
                }
                Ordre::NiveauMicro(s) => {
                    if let Some(v) = e.entree.as_mut() {
                        v.scalaire = s.clamp(0.0, 1.0);
                    }
                }
                Ordre::NiveauMicroDb(db) => {
                    if let Some(v) = e.entree.as_mut() {
                        v.db = db.clamp(v.min_db, v.max_db);
                    }
                }
                Ordre::Ampli { id, db } => {
                    if let Some(g) = e.amplis.iter_mut().find(|g| &g.id == id) {
                        g.db = caler(*db, g.min_db, g.max_db, g.pas_db);
                    }
                }
                Ordre::Suivre { .. } | Ordre::Eveil => {}
            }
        }
        let _ = tx.send(ordre);
    }

    /// À appeler à chaque image tant que la page est ouverte : le fil relit
    /// l'état tant qu'on le lui demande, et se rendort ensuite.
    pub fn tenir_eveille(&self) {
        let maintenant = self.depart.elapsed().as_millis() as u64 + 1;
        let avant = self.eveil.swap(maintenant, Ordering::Relaxed);
        if avant == 0 || maintenant.saturating_sub(avant) > SOMMEIL.as_millis() as u64 {
            self.ordonner(Ordre::Eveil);
        }
    }
}

/// La balance d'après les volumes des deux canaux : 0 au centre, négative
/// quand la gauche domine.
pub fn balance_de(gauche: f32, droite: f32) -> f32 {
    if gauche <= 0.0 && droite <= 0.0 {
        return 0.0;
    }
    if gauche >= droite {
        -(1.0 - droite / gauche)
    } else {
        1.0 - gauche / droite
    }
}

/// Les volumes des deux canaux pour une balance et un volume d'ensemble
/// donnés : le côté fort garde le volume, l'autre baisse.
pub fn canaux_pour(balance: f32, volume: f32) -> (f32, f32) {
    let b = balance.clamp(-1.0, 1.0);
    let gauche = if b > 0.0 { volume * (1.0 - b) } else { volume };
    let droite = if b < 0.0 { volume * (1.0 + b) } else { volume };
    (gauche, droite)
}

/// Un gain ramené dans sa plage et sur son pas.
pub fn caler(db: f32, min: f32, max: f32, pas: f32) -> f32 {
    let db = db.clamp(min, max);
    if pas > 0.0 {
        (min + ((db - min) / pas).round() * pas).clamp(min, max)
    } else {
        db
    }
}

/// « Casque (Realtek USB Audio) » → (« Casque », « Realtek USB Audio ») : le
/// point de terminaison, et la carte son qui le porte — la parenthèse finale,
/// prise entière même quand le nom de la carte en contient d'autres.
pub fn scinder_nom(nom: &str) -> (&str, Option<&str>) {
    if !nom.ends_with(')') {
        return (nom, None);
    }
    let mut profondeur = 0i32;
    for (i, c) in nom.char_indices().rev() {
        match c {
            ')' => profondeur += 1,
            '(' => {
                profondeur -= 1;
                if profondeur == 0 {
                    let avant = nom[..i].trim_end();
                    if avant.is_empty() {
                        break;
                    }
                    return (avant, Some(&nom[i + 1..nom.len() - 1]));
                }
            }
            _ => {}
        }
    }
    (nom, None)
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::wasapi;
    use std::collections::{HashMap, HashSet};
    use windows::core::{Interface, GUID, PWSTR};
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        IAudioVolumeLevel, IConnector, IDeviceTopology, IMMDevice, IPart,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

    /// Le fil du matériel : dort tant que la page est fermée, relit l'état
    /// deux fois par seconde quand elle est ouverte, applique les ordres dès
    /// qu'ils arrivent — les plus récents seulement, quand un curseur en
    /// envoie un par image.
    pub(super) fn boucle(
        etat: Arc<Mutex<EtatMateriel>>,
        rx: mpsc::Receiver<Ordre>,
        eveil: Arc<AtomicU64>,
        depart: Instant,
    ) {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let mut suivis: (Option<String>, Option<String>) = (None, None);
        let mut prises = Prises::new();
        loop {
            let depuis = (depart.elapsed().as_millis() as u64).saturating_sub(eveil.load(Ordering::Relaxed));
            let eveille = depuis < SOMMEIL.as_millis() as u64;
            let attente = if eveille { Duration::from_millis(500) } else { Duration::from_secs(3600) };
            let mut ordres = match rx.recv_timeout(attente) {
                Ok(o) => {
                    // Un curseur envoie un ordre par image : on laisse les
                    // suivants arriver, et seul le dernier sera appliqué.
                    std::thread::sleep(Duration::from_millis(30));
                    vec![o]
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Vec::new(),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            };
            ordres.extend(rx.try_iter());
            if !eveille && ordres.is_empty() {
                continue;
            }
            let mut erreur = None;
            for ordre in dernier_de_chaque(ordres) {
                if let Ordre::Suivre { entree, sortie } = &ordre {
                    suivis = (entree.clone(), sortie.clone());
                    continue;
                }
                if let Err(e) = appliquer(&ordre, &suivis, &mut prises) {
                    tracing::warn!("réglage matériel refusé ({ordre:?}) : {e:#}");
                    erreur = Some(format!("la carte a refusé le réglage : {e}"));
                }
            }
            let mut releve = lire(&suivis, &mut prises, &etat);
            releve.derniere_erreur = erreur;
            *etat.lock().unwrap() = releve;
        }
    }

    /// Un curseur qu'on glisse envoie un ordre par image : seul le dernier de
    /// chaque sorte compte (et de chaque gain, pour les amplifications).
    fn dernier_de_chaque(ordres: Vec<Ordre>) -> Vec<Ordre> {
        let mut garde: Vec<Ordre> = Vec::new();
        for o in ordres.into_iter().rev() {
            let meme = |a: &Ordre| match (a, &o) {
                (Ordre::Ampli { id: x, .. }, Ordre::Ampli { id: y, .. }) => x == y,
                _ => std::mem::discriminant(a) == std::mem::discriminant(&o),
            };
            if !garde.iter().any(meme) {
                garde.push(o);
            }
        }
        garde.reverse();
        garde
    }

    /// Le périphérique à suivre, et s'il s'agit d'un repli : le nom demandé
    /// est introuvable et c'est le défaut de Windows qu'on a pris.
    fn peripherique(nom: Option<&str>, entree: bool) -> anyhow::Result<(IMMDevice, bool)> {
        let enu = wasapi::enumerator()?;
        wasapi::pick(&enu, nom, entree)
    }

    fn volume_de(device: &IMMDevice) -> anyhow::Result<IAudioEndpointVolume> {
        Ok(unsafe { device.Activate(CLSCTX_ALL, None)? })
    }

    fn lire_volume(v: &IAudioEndpointVolume) -> anyhow::Result<Volume> {
        unsafe {
            let (mut min_db, mut max_db, mut pas) = (0f32, 0f32, 0f32);
            v.GetVolumeRange(&mut min_db, &mut max_db, &mut pas)?;
            let balance = if v.GetChannelCount()? == 2 {
                Some(balance_de(v.GetChannelVolumeLevelScalar(0)?, v.GetChannelVolumeLevelScalar(1)?))
            } else {
                None
            };
            Ok(Volume {
                scalaire: v.GetMasterVolumeLevelScalar()?,
                db: v.GetMasterVolumeLevel()?,
                min_db,
                max_db,
                balance,
            })
        }
    }

    fn texte(p: PWSTR) -> String {
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.as_ptr() as *const _));
            s
        }
    }

    /// Les prises micro déjà trouvées, par point de terminaison (`None` : sa
    /// carte n'en montre pas). Les chercher traverse vers la topologie de la
    /// carte (`GetConnectedTo`), ce qui prend huit secondes la première fois
    /// sur certaines cartes USB (NICEHCK NK1 MAX) : une fois par carte.
    /// La prise micro de chaque carte (par identifiant), ou l'échec à la
    /// trouver, daté : une carte USB qui ne répond pas encore juste après son
    /// branchement n'est pas condamnée pour la session.
    type Prises = HashMap<String, (Option<IPart>, Instant)>;

    /// Au-delà, un échec à trouver la prise est retenté.
    const NOUVEL_ESSAI_PRISE: Duration = Duration::from_secs(10);

    fn identifiant(device: &IMMDevice) -> String {
        unsafe { device.GetId().map(texte).unwrap_or_default() }
    }

    /// La prise micro de ce périphérique, cherchée une fois — et de nouveau
    /// après un échec un peu ancien.
    fn prise_connue(device: &IMMDevice, prises: &mut Prises) -> Option<IPart> {
        let id = identifiant(device);
        match prises.get(&id) {
            // Une carte débranchée puis rebranchée garde son identifiant,
            // mais l'ancienne topologie ne répond plus : on la cherche de
            // nouveau.
            Some((Some(p), _)) if unsafe { p.GetGlobalId() }.map(texte).is_err() => {
                prises.remove(&id);
            }
            // Un échec transitoire (carte USB qui s'installe encore) n'est
            // pas définitif.
            Some((None, quand)) if quand.elapsed() > NOUVEL_ESSAI_PRISE => {
                prises.remove(&id);
            }
            _ => {}
        }
        prises.entry(id).or_insert_with(|| (prise_micro(device).ok(), Instant::now())).0.clone()
    }

    /// Le premier élément du chemin du micro dans la topologie de la carte :
    /// la prise micro, d'où l'on descend vers le logiciel.
    fn prise_micro(device: &IMMDevice) -> anyhow::Result<IPart> {
        unsafe {
            let topo: IDeviceTopology = device.Activate(CLSCTX_ALL, None)?;
            let connecteur = topo.GetConnector(0)?;
            let en_face: IConnector = connecteur.GetConnectedTo()?;
            Ok(en_face.cast::<IPart>()?)
        }
    }

    /// La commande de volume d'un élément de la topologie, s'il en a une.
    /// `IPart::Activate` n'a pas de forme générique : on passe l'IID et l'on
    /// reprend le pointeur rendu.
    fn activer_volume(part: &IPart) -> anyhow::Result<IAudioVolumeLevel> {
        unsafe {
            let mut brut: *mut core::ffi::c_void = std::ptr::null_mut();
            part.Activate(CLSCTX_ALL.0, &IAudioVolumeLevel::IID, Some(&mut brut))?;
            anyhow::ensure!(!brut.is_null(), "pas de commande de volume");
            Ok(IAudioVolumeLevel::from_raw(brut))
        }
    }

    /// Tous les volumes du chemin du micro, de la prise vers le logiciel.
    fn volumes_du_chemin(depart: &IPart) -> Vec<(IPart, Gain)> {
        let mut vus = HashSet::new();
        let mut out = Vec::new();
        parcourir(depart, 0, &mut vus, &mut out);
        out
    }

    fn parcourir(part: &IPart, profondeur: usize, vus: &mut HashSet<String>, out: &mut Vec<(IPart, Gain)>) {
        if profondeur > 16 {
            return;
        }
        unsafe {
            let Ok(id) = part.GetGlobalId().map(texte) else { return };
            if !vus.insert(id.clone()) {
                return;
            }
            if let Ok(v) = activer_volume(part) {
                let (mut min_db, mut max_db, mut pas_db) = (0f32, 0f32, 0f32);
                if v.GetLevelRange(0, &mut min_db, &mut max_db, &mut pas_db).is_ok() {
                    if let Ok(db) = v.GetLevel(0) {
                        let nom = part.GetName().map(texte).unwrap_or_default();
                        out.push((part.clone(), Gain { nom, db, min_db, max_db, pas_db, id }));
                    }
                }
            }
            let Ok(suivants) = part.EnumPartsOutgoing() else { return };
            for i in 0..suivants.GetCount().unwrap_or(0) {
                if let Ok(p) = suivants.GetPart(i) {
                    parcourir(&p, profondeur + 1, vus, out);
                }
            }
        }
    }

    /// Les amplifications : les volumes du chemin, sauf celui que Windows
    /// présente comme niveau du micro — même plage que le volume du point de
    /// terminaison.
    fn amplis(prise: &IPart, niveau: &Volume) -> Vec<(IPart, Gain)> {
        let mut volumes = volumes_du_chemin(prise);
        let meme_plage =
            |g: &Gain| (g.min_db - niveau.min_db).abs() < 0.1 && (g.max_db - niveau.max_db).abs() < 0.1;
        if let Some(i) = volumes.iter().position(|(_, g)| meme_plage(g)) {
            volumes.remove(i);
        }
        volumes
    }

    /// Relève l'état. Quand la prise du micro reste à chercher (lent sur
    /// certaines cartes), les volumes sont publiés d'abord : la page ne reste
    /// pas vide le temps que les amplifications arrivent.
    fn lire(
        suivis: &(Option<String>, Option<String>),
        prises: &mut Prises,
        publier: &Mutex<EtatMateriel>,
    ) -> EtatMateriel {
        let mut e = EtatMateriel { disponible: true, ..Default::default() };
        if let Ok((d, repli)) = peripherique(suivis.1.as_deref(), false) {
            e.sortie_nom = wasapi::friendly_name(&d).ok();
            e.sortie = volume_de(&d).and_then(|v| lire_volume(&v)).ok();
            e.sortie_repli = repli;
        }
        if let Ok((d, repli)) = peripherique(suivis.0.as_deref(), true) {
            e.entree_nom = wasapi::friendly_name(&d).ok();
            e.entree = volume_de(&d).and_then(|v| lire_volume(&v)).ok();
            e.entree_repli = repli;
            if let Some(niveau) = &e.entree {
                if !prises.contains_key(&identifiant(&d)) {
                    *publier.lock().unwrap() = e.clone();
                }
                if let Some(prise) = prise_connue(&d, prises) {
                    e.amplis = amplis(&prise, niveau).into_iter().map(|(_, g)| g).collect();
                }
            }
        }
        e.topologie_lue = true;
        e
    }

    fn appliquer(ordre: &Ordre, suivis: &(Option<String>, Option<String>), prises: &mut Prises) -> anyhow::Result<()> {
        let aucun = std::ptr::null::<GUID>();
        unsafe {
            match ordre {
                Ordre::VolumeSortie(s) => {
                    let v = volume_de(&peripherique(suivis.1.as_deref(), false)?.0)?;
                    v.SetMasterVolumeLevelScalar(s.clamp(0.0, 1.0), aucun)?;
                }
                Ordre::Balance(b) => {
                    let v = volume_de(&peripherique(suivis.1.as_deref(), false)?.0)?;
                    if v.GetChannelCount()? == 2 {
                        let (g, d) = canaux_pour(*b, v.GetMasterVolumeLevelScalar()?);
                        v.SetChannelVolumeLevelScalar(0, g, aucun)?;
                        v.SetChannelVolumeLevelScalar(1, d, aucun)?;
                    }
                }
                Ordre::NiveauMicro(s) => {
                    let v = volume_de(&peripherique(suivis.0.as_deref(), true)?.0)?;
                    v.SetMasterVolumeLevelScalar(s.clamp(0.0, 1.0), aucun)?;
                }
                Ordre::NiveauMicroDb(db) => {
                    let v = volume_de(&peripherique(suivis.0.as_deref(), true)?.0)?;
                    let (mut min, mut max, mut pas) = (0f32, 0f32, 0f32);
                    v.GetVolumeRange(&mut min, &mut max, &mut pas)?;
                    v.SetMasterVolumeLevel(caler(*db, min, max, 0.0), aucun)?;
                }
                Ordre::Ampli { id, db } => {
                    let (d, _) = peripherique(suivis.0.as_deref(), true)?;
                    let Some(prise) = prise_connue(&d, prises) else {
                        anyhow::bail!("pas de prise micro dans la topologie de la carte");
                    };
                    let Some((part, g)) = volumes_du_chemin(&prise).into_iter().find(|(_, g)| &g.id == id) else {
                        anyhow::bail!("gain introuvable sur le chemin du micro");
                    };
                    let v = activer_volume(&part)?;
                    v.SetLevelUniform(caler(*db, g.min_db, g.max_db, g.pas_db), None)?;
                }
                Ordre::Suivre { .. } | Ordre::Eveil => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_balance_fait_l_aller_retour() {
        assert_eq!(balance_de(0.8, 0.8), 0.0);
        for b in [-1.0f32, -0.5, -0.1, 0.0, 0.3, 1.0] {
            let (g, d) = canaux_pour(b, 0.8);
            assert!((balance_de(g, d) - b).abs() < 1e-5, "{b} → ({g}, {d})");
            // Le côté fort garde le volume d'ensemble.
            assert!((g.max(d) - 0.8).abs() < 1e-6);
        }
        assert_eq!(balance_de(0.0, 0.0), 0.0);
    }

    #[test]
    fn un_gain_se_cale_sur_son_pas() {
        // L'« Ampli microphone » de drion : 0 à 30 dB par pas de 10.
        assert_eq!(caler(13.0, 0.0, 30.0, 10.0), 10.0);
        assert_eq!(caler(16.0, 0.0, 30.0, 10.0), 20.0);
        assert_eq!(caler(-5.0, 0.0, 30.0, 10.0), 0.0);
        assert_eq!(caler(99.0, 0.0, 30.0, 10.0), 30.0);
        // Continu : seulement borné.
        assert_eq!(caler(3.3, -17.25, 12.0, 0.0), 3.3);
        assert_eq!(caler(20.0, -17.25, 12.0, 0.0), 12.0);
    }

    #[test]
    fn le_nom_windows_se_scinde() {
        assert_eq!(scinder_nom("Casque (Realtek USB Audio)"), ("Casque", Some("Realtek USB Audio")));
        assert_eq!(
            scinder_nom("Haut-parleurs (JBL Quantum Stream Talk)"),
            ("Haut-parleurs", Some("JBL Quantum Stream Talk"))
        );
        assert_eq!(scinder_nom("Micro sans carte"), ("Micro sans carte", None));
        // Des parenthèses dans le nom de la carte : elle reste entière.
        assert_eq!(scinder_nom("Casque (2- Arctis 7 (Chat))"), ("Casque", Some("2- Arctis 7 (Chat)")));
        assert_eq!(scinder_nom("(rien devant)"), ("(rien devant)", None));
    }
}
