//! Persistance de l'historique texte : un fichier JSONL par salon.
//!
//! Volontairement simple et 100 % Rust (pas de dépendance C) : append-only,
//! les N derniers messages par salon sont gardés en mémoire pour les requêtes
//! History. Migration possible vers redb/SQLite si le besoin grandit.
//!
//! L'écriture sur disque ne se fait PAS dans le fil appelant : `append` est
//! appelé depuis la boucle asynchrone, sur le chemin le plus chaud du serveur
//! (chaque message de chat). Un `writeln!` y bloque un ouvrier Tokio le temps
//! d'un appel système, et c'est le relais des datagrammes vocaux qui hoquette.
//! Les lignes partent donc vers un fil d'écriture dédié, qui les traite dans
//! l'ordre où elles ont été produites.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use ki_protocol::{ChannelId, ChannelInfo, ChatRecord};

/// Nombre max de messages gardés en mémoire par salon.
const MEM_CAP: usize = 1000;

/// Un message dans l'index : son horodatage et sa position dans le fichier.
///
/// Seize octets par message — cent mille messages tiennent dans 1,6 Mo, à
/// comparer aux quinze mégaoctets du journal lui-même.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Entry {
    ts: u64,
    offset: u64,
}

/// Index d'un journal, **trié par (horodatage, position)**.
///
/// Le tri est ce qui rend la recherche binaire possible, et il ne va pas de
/// soi : l'horloge murale peut reculer — un ajustement NTP, une machine
/// virtuelle qui se réveille — si bien que le fichier n'est pas
/// nécessairement dans l'ordre du temps. La position départage deux messages
/// de même horodatage, ce qui rend l'ordre total et stable.
type Index = Vec<Entry>;

/// Ce que l'index remplace.
///
/// `before()` relisait **l'intégralité** du fichier, désérialisait chaque
/// ligne, filtrait, triait, et jetait tout sauf une page de cinquante
/// messages. Remonter une conversation enchaîne les requêtes au rythme des
/// réponses : dix pages, c'étaient dix relectures de quinze mégaoctets et un
/// million de désérialisations JSON, sur le pool bloquant d'un VPS à deux
/// cœurs. Désormais : une recherche binaire, cinquante positionnements, et
/// cinquante lignes lues.
///
/// C'est aussi ce qui rendra la recherche dans l'historique possible.
const _: () = ();

/// Budget d'octets d'une réponse d'historique, marge sous [`ki_protocol::MAX_LINE`].
///
/// Une réponse `History`/`HistoryPage` part sur le flux de contrôle en **une
/// seule ligne**. Or le lecteur d'en face refuse toute ligne au-delà de
/// `MAX_LINE` et **ferme la connexion** : sans borne à l'émission, quelques
/// dizaines de longs messages suffisaient à rendre un salon impossible à
/// ouvrir (déconnexion en boucle). On ne renvoie donc jamais plus que ce qui
/// tient dans une ligne ; le reste se récupère en remontant le fil.
const MAX_HISTORY_BYTES: usize = ki_protocol::MAX_LINE - 8 * 1024;

/// Ne garde que les messages les plus **récents** dont la taille sérialisée
/// cumulée tient dans [`MAX_HISTORY_BYTES`], l'ordre chronologique préservé.
///
/// Renvoie aussi `true` si des messages plus anciens ont dû être retirés —
/// l'appelant en a besoin pour dire au client qu'il en reste à charger.
fn fit_within(messages: Vec<ChatRecord>) -> (Vec<ChatRecord>, bool) {
    let mut total = 0usize;
    let mut start = messages.len();
    for (i, rec) in messages.iter().enumerate().rev() {
        let size = serde_json::to_string(rec).map(|s| s.len() + 1).unwrap_or(usize::MAX);
        if total + size > MAX_HISTORY_BYTES {
            break;
        }
        total += size;
        start = i;
    }
    // Toujours rendre au moins le dernier message : un message seul ne peut de
    // toute façon pas dépasser le budget (texte borné à MAX_CHAT_TEXT), mais on
    // ne veut en aucun cas renvoyer une page vide en prétendant qu'il reste à
    // charger — le client bouclerait.
    if start == messages.len() && !messages.is_empty() {
        start = messages.len() - 1;
    }
    let truncated = start > 0;
    (messages[start..].to_vec(), truncated)
}

pub struct History {
    /// Les N derniers messages de chaque salon. Le fichier correspondant est
    /// tenu par le fil d'écriture, lui seul y touche.
    logs: Mutex<HashMap<ChannelId, VecDeque<ChatRecord>>>,
    /// Position de chaque message dans son fichier, par salon.
    ///
    /// Partagé avec le fil d'écriture : lui seul connaît la position d'une
    /// ligne qu'il vient d'écrire, c'est donc lui qui complète l'index.
    index: Arc<Mutex<HashMap<ChannelId, Index>>>,
    /// Vers le fil d'écriture. `Option` seulement pour pouvoir lâcher
    /// l'émetteur dans `Drop` avant d'attendre le fil : tant qu'un émetteur
    /// vit, le `recv` d'en face ne rend jamais la main.
    writes: Option<Sender<WriteCmd>>,
    writer: Option<std::thread::JoinHandle<()>>,
}

/// Parcourt un journal en notant la position de chaque message lisible.
///
/// Une seule lecture, au démarrage — celle que l'on faisait déjà pour remplir
/// le cache mémoire. `read_line` plutôt que `lines()` : il faut compter les
/// octets consommés, ce que l'itérateur ne dit pas.
///
/// # La règle de lecture d'un journal
///
/// Elle vivait dans un `keep_or_stop` partagé par trois lecteurs ; il n'en
/// reste qu'un, celui-ci, mais la règle n'a pas changé et mérite d'être dite.
///
/// Une **ligne illisible** ne fait pas échouer le démarrage. Un fichier tronqué
/// par un disque plein ou une coupure de courant laisse souvent sa dernière
/// ligne coupée au milieu d'un caractère accentué : refuser de démarrer pour
/// ça ferait perdre tout l'historique, par ailleurs parfaitement lisible.
///
/// Une **vraie erreur d'entrée-sortie**, elle, arrête la lecture. La distinction
/// n'est pas cosmétique : une erreur matérielle ne fait pas avancer le curseur,
/// et l'ignorer ferait tourner la boucle indéfiniment — un serveur figé, plus
/// difficile à diagnostiquer qu'un serveur qui refuse de démarrer.
fn scan(path: &std::path::Path, mem_cap: usize) -> (VecDeque<ChatRecord>, Index, u64) {
    let mut recent = VecDeque::with_capacity(mem_cap);
    let mut index = Index::new();
    let mut offset = 0u64;

    let Ok(file) = File::open(path) else { return (recent, index, 0) };
    let mut reader = BufReader::new(file);
    // Octets bruts, et non `read_line` : celui-ci échoue sur de l'UTF-8
    // invalide **sans dire combien d'octets il a consommés**, ce qui ne
    // laisse d'autre choix que d'abandonner le reste du fichier. `read_until`
    // ne peut pas échouer là-dessus, et `from_slice` valide l'UTF-8 pour son
    // propre compte : une ligne abîmée est sautée, et on sait de combien
    // avancer. C'est ce qui préserve la règle énoncée plus haut.
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let read = match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                tracing::error!("lecture d'un journal interrompue : {e}");
                break;
            }
        };
        let debut = offset;
        offset += read as u64;
        if let Ok(rec) = serde_json::from_slice::<ChatRecord>(trim_eol(&buf)) {
            index.push(Entry { ts: rec.ts, offset: debut });
            if recent.len() == mem_cap {
                recent.pop_front();
            }
            recent.push_back(rec);
        }
    }
    // Triés une fois : l'horloge murale peut avoir reculé, le fichier n'est
    // donc pas forcément dans l'ordre du temps. L'index ET le cache mémoire
    // en dépendent — le premier pour sa recherche binaire, le second pour
    // rendre les pages dans l'ordre où on les lit.
    index.sort_unstable();
    let mut recent: Vec<ChatRecord> = recent.into_iter().collect();
    recent.sort_by_key(|r| r.ts);
    let recent: VecDeque<ChatRecord> = recent.into();
    (recent, index, offset)
}

impl History {
    pub fn open(data_dir: &str, channels: &[ChannelInfo]) -> anyhow::Result<Self> {
        let dir = PathBuf::from(data_dir);
        std::fs::create_dir_all(&dir)?;
        let mut logs = HashMap::new();
        let mut files = HashMap::new();
        let mut index = HashMap::new();
        for ch in channels {
            let path = dir.join(format!("channel-{}.jsonl", ch.id));
            // Une seule lecture remplit le cache mémoire ET l'index : c'est
            // la lecture qu'on faisait déjà, à laquelle on note au passage la
            // position de chaque message.
            let (recent, idx, taille) = scan(&path, MEM_CAP);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            files.insert(ch.id, Log { file, len: taille });
            index.insert(ch.id, idx);
            logs.insert(ch.id, recent);
        }
        // Canal non borné : le chat est déjà limité en amont par le seau à
        // jetons de chaque client, et faire attendre l'appelant ici serait
        // exactement ce qu'on cherche à éviter.
        let index = Arc::new(Mutex::new(index));
        let (writes, rx) = std::sync::mpsc::channel();
        let writer = std::thread::Builder::new()
            .name("ki-history".into())
            .spawn({
                let index = index.clone();
                move || writer_loop(files, rx, index)
            })?;
        Ok(Self {
            logs: Mutex::new(logs),
            index,
            writes: Some(writes),
            writer: Some(writer),
        })
    }

    pub fn append(&self, channel: ChannelId, rec: &ChatRecord) {
        // Le cache mémoire est mis à jour tout de suite, sous le verrou : un
        // History demandé dans la foulée doit déjà voir ce message, même si
        // le disque, lui, n'a pas encore été touché.
        {
            let mut logs = self.logs.lock().unwrap();
            let Some(recent) = logs.get_mut(&channel) else { return };
            if recent.len() == MEM_CAP {
                recent.pop_front();
            }
            // Presque toujours en fin : l'horloge avance. Quand elle recule —
            // ajustement NTP, machine virtuelle qui se réveille — on insère au
            // bon endroit, sinon la pagination rendrait les messages dans
            // l'ordre du fichier et non dans celui du temps.
            match recent.back() {
                Some(dernier) if dernier.ts <= rec.ts => recent.push_back(rec.clone()),
                None => recent.push_back(rec.clone()),
                Some(_) => {
                    let pos = recent.partition_point(|r| r.ts <= rec.ts);
                    recent.insert(pos, rec.clone());
                }
            }
        }
        // Verrou relâché avant l'envoi : le fil d'écriture ne doit jamais
        // faire attendre un `recent()`.
        if let Ok(line) = serde_json::to_string(rec) {
            if let Some(writes) = &self.writes {
                let _ = writes.send(WriteCmd::Line(channel, rec.ts, line));
            }
        }
    }

    /// Ouvre le journal d'un salon créé en cours d'exécution.
    ///
    /// À appeler **avant** d'annoncer le salon : `append` ignore en silence
    /// un salon qu'il ne connaît pas, et le premier message partirait dans
    /// le vide sans que rien ne le signale.
    pub fn open_channel(&self, data_dir: &str, channel: ChannelId) -> anyhow::Result<()> {
        let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        self.logs
            .lock()
            .unwrap()
            .entry(channel)
            .or_insert_with(|| VecDeque::with_capacity(MEM_CAP));
        if let Some(writes) = &self.writes {
            let _ = writes.send(WriteCmd::Open(channel, file));
        }
        Ok(())
    }

    /// Referme le journal d'un salon supprimé. Le fichier reste sur le
    /// disque — c'est l'archivage, assuré par le magasin de salons.
    pub fn close_channel(&self, channel: ChannelId) {
        self.logs.lock().unwrap().remove(&channel);
        if let Some(writes) = &self.writes {
            let _ = writes.send(WriteCmd::Close(channel));
        }
    }

    pub fn recent(&self, channel: ChannelId, limit: usize) -> Vec<ChatRecord> {
        let logs = self.logs.lock().unwrap();
        match logs.get(&channel) {
            Some(recent) => {
                let skip = recent.len().saturating_sub(limit);
                let msgs: Vec<ChatRecord> = recent.iter().skip(skip).cloned().collect();
                // Borné à une ligne de contrôle. Le client complète en
                // remontant le fil (`HistoryBefore`) si tout ne tient pas.
                fit_within(msgs).0
            }
            None => Vec::new(),
        }
    }

    /// Les `limit` messages qui précèdent `before_ts`, du plus ancien au plus
    /// récent, plus un drapeau disant s'il en reste encore avant.
    ///
    /// Sert à remonter le fil. Le cache mémoire ne couvre que les 1000
    /// derniers messages : au-delà, il faut relire le fichier, ce qui est
    /// justement ce qui rendait le passé inatteignable jusqu'ici. La lecture
    /// disque est donc assumée — c'est une action rare, déclenchée par un
    /// défilement, et l'appelant la déporte hors de la boucle asynchrone.
    pub fn before(
        &self,
        data_dir: &str,
        channel: ChannelId,
        before_ts: u64,
        limit: usize,
    ) -> (Vec<ChatRecord>, bool) {
        // Le cache d'abord : le cas courant est de remonter de quelques
        // pages, ce qui ne touche pas le disque.
        let from_memory = {
            let logs = self.logs.lock().unwrap();
            let Some(recent) = logs.get(&channel) else { return (Vec::new(), false) };
            let older: Vec<ChatRecord> =
                recent.iter().filter(|r| r.ts < before_ts).cloned().collect();
            // Le cache est plein ET on en a épuisé le début : le reste est
            // sur le disque, il faut aller le chercher.
            let exhausted = older.len() < limit && recent.len() == MEM_CAP;
            if !exhausted {
                let skip = older.len().saturating_sub(limit);
                let more = skip > 0 || recent.len() == MEM_CAP;
                // Tronqué à une ligne de contrôle si besoin : dans ce cas il
                // reste forcément des messages à charger avant.
                let (page, truncated) = fit_within(older[skip..].to_vec());
                return (page, more || truncated);
            }
            older
        };

        let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
        let Ok(mut file) = File::open(&path) else {
            // Pas de fichier : le cache est tout ce qui existe.
            let (page, truncated) = fit_within(from_memory);
            return (page, truncated);
        };

        // L'index dit où sont les messages ; on ne lit que ceux de la page.
        //
        // Auparavant : relecture intégrale du fichier, désérialisation de
        // chaque ligne, filtre, tri, et l'on jetait tout sauf cinquante
        // messages. Un salon de cent mille messages fait quinze mégaoctets,
        // et remonter une conversation enchaîne les requêtes au rythme des
        // réponses.
        let fenetre: Vec<Entry> = {
            let index = self.index.lock().unwrap();
            let Some(entries) = index.get(&channel) else {
                let (page, truncated) = fit_within(from_memory);
                return (page, truncated);
            };
            // L'index étant trié par (horodatage, position), la borne se
            // trouve par recherche binaire.
            let fin = entries.partition_point(|e| e.ts < before_ts);
            let debut = fin.saturating_sub(limit);
            entries[debut..fin].to_vec()
        };
        // `debut > 0` se relit sur la fenêtre : si elle fait exactement
        // `limit`, c'est qu'on a pu en couper avant.
        let reste_avant = fenetre.len() == limit;

        let mut older = Vec::with_capacity(fenetre.len());
        for entry in &fenetre {
            match read_record_at(&mut file, entry.offset) {
                Some(rec) => older.push(rec),
                // Une position devenue fausse — fichier remplacé sous nos
                // pieds, ligne illisible — ne doit pas faire perdre la page :
                // on saute ce message.
                None => continue,
            }
        }
        let (page, truncated) = fit_within(older);
        (page, reste_avant || truncated)
    }

    /// Cherche `query` dans les salons donnés, casse ignorée.
    ///
    /// Rend au plus `limit` résultats, les **plus récents**, du plus ancien
    /// au plus récent ; le booléen dit s'il en a été laissé de côté.
    ///
    /// # Pourquoi une lecture séquentielle, et pas l'index
    ///
    /// L'index de `before` ne sert à rien ici, et il faut le dire : il situe
    /// un message par son horodatage, or une recherche ne sait pas d'avance
    /// dans quelle tranche de temps regarder. Elle doit voir chaque message,
    /// donc lire tout le fichier — et une lecture d'un bout à l'autre bat
    /// largement cent mille repositionnements.
    ///
    /// Ce qui coûte cher n'est pas la lecture mais la **désérialisation** :
    /// reconstruire cent mille `ChatRecord` pour en garder trois. D'où le
    /// tamis : on cherche d'abord la chaîne dans la ligne JSON brute, en
    /// octets, et l'on ne déserialise que ce qui a survécu.
    pub fn search(
        &self,
        data_dir: &str,
        channels: &[ChannelId],
        query: &str,
        limit: usize,
    ) -> (Vec<(ChannelId, ChatRecord)>, bool) {
        let besoin = query.trim().to_lowercase();
        if besoin.is_empty() || limit == 0 {
            return (Vec::new(), false);
        }
        // Le tamis ne s'utilise que là où il ne peut **rien manquer**. Deux
        // conditions, et les deux ont été apprises en les manquant :
        //
        // 1. Requête en ASCII pur. La comparaison se fait en octets ramenés
        //    aux minuscules ASCII : c'est sans faille tant que l'aiguille est
        //    ASCII, aucun octet ASCII n'apparaissant jamais à l'intérieur
        //    d'une séquence UTF-8 multi-octets. Avec un accent, en revanche,
        //    « É » et « é » ne se ramènent pas l'un à l'autre à ce niveau.
        //
        // 2. Aucun caractère que JSON échappe. La ligne du journal n'est pas
        //    le texte : `serde_json` y écrit `\"`, `\\`, `\n`. Chercher un
        //    guillemet dans une ligne où il s'écrit en deux octets ne trouve
        //    rien — un faux négatif, c'est-à-dire exactement ce qu'un tamis
        //    n'a pas le droit de produire.
        //
        // Hors de ces deux cas, on désérialise tout : plus lent, mais juste.
        let tamisable = besoin.is_ascii()
            && !besoin.chars().any(|c| c == '"' || c == '\\' || c.is_control());
        let tamis = tamisable.then(|| besoin.as_bytes().to_vec());

        let mut trouves: Vec<(ChannelId, ChatRecord)> = Vec::new();
        let mut deborde = false;
        for &channel in channels {
            // Fenêtre glissante sur les `limit` derniers résultats du salon :
            // c'est ce qui borne la mémoire quel que soit le nombre de
            // correspondances. Chercher « le » dans dix ans d'archives ne doit
            // pas rapatrier dix ans d'archives.
            let mut par_salon: VecDeque<ChatRecord> = VecDeque::with_capacity(limit);

            // Le cache mémoire est la **queue du fichier** : les mêmes
            // messages s'y trouvent deux fois. On relève donc d'abord ce
            // qu'il contient, pour écarter du fichier ce qu'il redira.
            //
            // Le sens de la comparaison est tout l'enjeu. Dédoublonner
            // contre ce qui a *survécu* dans la fenêtre était faux : ce qui
            // en était tombé n'était plus reconnu comme déjà vu, se faisait
            // réinsérer, et chassait les résultats récents — la recherche
            // rendait alors les plus **anciens**. Le défaut ne se voyait que
            // si le fil d'écriture avait eu le temps de vider sa file, ce qui
            // dépend de la machine : passant ici, échouant en intégration.
            //
            // Le cache est borné à `MEM_CAP`, cet ensemble aussi : seize
            // kilooctets au pire, quel que soit le nombre de correspondances.
            let (frais, en_memoire) = {
                let logs = self.logs.lock().unwrap();
                match logs.get(&channel) {
                    Some(recent) => (
                        recent
                            .iter()
                            .filter(|r| r.text.to_lowercase().contains(&besoin))
                            .cloned()
                            .collect::<Vec<ChatRecord>>(),
                        recent
                            .iter()
                            .map(|r| (r.user_id, r.ts))
                            .collect::<std::collections::HashSet<_>>(),
                    ),
                    None => (Vec::new(), std::collections::HashSet::new()),
                }
            };

            let path = PathBuf::from(data_dir).join(format!("channel-{channel}.jsonl"));
            if let Ok(file) = File::open(&path) {
                let mut reader = BufReader::new(file);
                let mut buf: Vec<u8> = Vec::new();
                let mut minuscules: Vec<u8> = Vec::new();
                loop {
                    buf.clear();
                    // Même règle que `scan` : une ligne illisible se saute,
                    // une erreur matérielle arrête la lecture.
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("recherche interrompue : {e}");
                            break;
                        }
                    }
                    if let Some(tamis) = &tamis {
                        minuscules.clear();
                        minuscules.extend(buf.iter().map(u8::to_ascii_lowercase));
                        if !contient(&minuscules, tamis) {
                            continue;
                        }
                    }
                    // Le tamis peut se tromper — le mot cherché peut être dans
                    // le pseudo, ou dans un nom de fichier. Seul le texte
                    // compte, et c'est ici qu'on le vérifie pour de bon.
                    if let Ok(rec) = serde_json::from_slice::<ChatRecord>(trim_eol(&buf)) {
                        // Déjà en mémoire : le cache le redira, et en meilleur
                        // ordre. On le laisse passer une seule fois.
                        if en_memoire.contains(&(rec.user_id, rec.ts)) {
                            continue;
                        }
                        if rec.text.to_lowercase().contains(&besoin) {
                            garder(&mut par_salon, limit, rec, &mut deborde);
                        }
                    }
                }
            }

            // Le cache par-dessus, donc en dernier : il porte les messages les
            // plus récents — y compris ceux que le fil d'écriture n'a pas
            // encore posés sur le disque. Ne pas trouver le message envoyé
            // trois secondes plus tôt ferait douter de toute la recherche.
            for rec in frais {
                garder(&mut par_salon, limit, rec, &mut deborde);
            }

            trouves.extend(par_salon.into_iter().map(|r| (channel, r)));
        }

        // Le classement final est chronologique, tous salons confondus : on
        // cherche « quand ai-je vu ça », pas « dans quel fichier ».
        trouves.sort_by_key(|(_, r)| r.ts);
        if trouves.len() > limit {
            trouves.drain(..trouves.len() - limit);
            deborde = true;
        }

        // La réponse part en **une seule ligne** sur le flux de contrôle,
        // exactement comme une page d'historique : même borne, même raison.
        // Cent résultats de quatre mille caractères ne tiendraient pas, et une
        // ligne trop longue fait fermer la connexion d'en face — la recherche
        // deviendrait un moyen de se déconnecter soi-même.
        let mut total = 0usize;
        let mut debut = trouves.len();
        for (i, (_, rec)) in trouves.iter().enumerate().rev() {
            // Une quarantaine d'octets pour l'enveloppe `SearchHit` autour du
            // message : le numéro de salon et les accolades.
            let taille = serde_json::to_string(rec).map(|s| s.len() + 48).unwrap_or(usize::MAX);
            if total + taille > MAX_HISTORY_BYTES {
                break;
            }
            total += taille;
            debut = i;
        }
        if debut > 0 {
            trouves.drain(..debut);
            deborde = true;
        }
        (trouves, deborde)
    }
}

/// Ajoute un résultat à la fenêtre, en poussant dehors le plus ancien si elle
/// est pleine — et en notant qu'il a été poussé dehors.
fn garder(
    fenetre: &mut VecDeque<ChatRecord>,
    limit: usize,
    rec: ChatRecord,
    deborde: &mut bool,
) {
    if fenetre.len() == limit {
        fenetre.pop_front();
        *deborde = true;
    }
    fenetre.push_back(rec);
}

/// `foin.contains(aiguille)`, en octets.
///
/// `slice::windows` plutôt qu'une conversion en `str` : la ligne brute n'est
/// pas forcément de l'UTF-8 valide, et l'on refuse de payer une validation
/// pour un test qui n'en a pas besoin.
fn contient(foin: &[u8], aiguille: &[u8]) -> bool {
    if aiguille.len() > foin.len() {
        return false;
    }
    foin.windows(aiguille.len()).any(|f| f == aiguille)
}

/// Lit le message qui commence à `offset`.
///
/// `None` = position invalide ou ligne illisible. Le fichier est ouvert une
/// fois par page et repositionné pour chaque message : dans le cas courant —
/// horloge qui avance — les positions se suivent, et la lecture anticipée du
/// système fait le reste.
fn read_record_at(file: &mut File, offset: u64) -> Option<ChatRecord> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf: Vec<u8> = Vec::new();
    let mut reader = BufReader::new(file);
    match reader.read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => serde_json::from_slice::<ChatRecord>(trim_eol(&buf)).ok(),
    }
}

/// Retire la fin de ligne, quelle que soit sa convention.
///
/// Un journal écrit sous Windows peut porter des `\r\n` : les deux octets
/// s'enlèvent, dans cet ordre.
fn trim_eol(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && line[end - 1] == b'\r' {
        end -= 1;
    }
    &line[..end]
}

impl Drop for History {
    /// Lâcher l'émetteur ferme le canal ; le fil écrit ce qui reste en file
    /// avant de s'arrêter. Sans cette attente, les derniers messages reçus
    /// juste avant un arrêt propre n'atteindraient jamais le fichier.
    fn drop(&mut self) {
        self.writes.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

/// Sérialise les écritures : un seul fil, une seule file, donc les lignes
/// d'un salon arrivent sur le disque dans l'ordre où elles ont été acceptées.
fn writer_loop(
    mut files: HashMap<ChannelId, Log>,
    rx: Receiver<WriteCmd>,
    index: Arc<Mutex<HashMap<ChannelId, Index>>>,
) {
    // `recv` continue de rendre ce qui est déjà en file après la fermeture du
    // canal : rien de ce qui a été accepté n'est perdu à l'arrêt.
    while let Ok(cmd) = rx.recv() {
        match cmd {
            WriteCmd::Line(channel, ts, line) => {
                let Some(log) = files.get_mut(&channel) else { continue };
                let debut = log.len;
                if let Err(e) = writeln!(log.file, "{line}") {
                    tracing::error!(
                        "écriture de l'historique du salon {channel} impossible : {e}"
                    );
                    // Position inconnue après un échec partiel : on cesse
                    // d'indexer ce salon plutôt que de mentir sur des
                    // positions. L'index reste juste pour ce qui précède.
                    log.len = match log.file.metadata().map(|m| m.len()) {
                        Ok(n) => n,
                        Err(_) => continue,
                    };
                    continue;
                }
                log.len = debut + line.len() as u64 + 1;

                // L'index est complété ici, et seulement ici : c'est le seul
                // endroit qui connaisse la position d'une ligne.
                let mut index = index.lock().unwrap();
                let entries = index.entry(channel).or_default();
                let entry = Entry { ts, offset: debut };
                // Presque toujours en fin : l'horloge avance. On insère au
                // bon endroit quand elle a reculé, plutôt que de retrier tout
                // l'index à chaque message.
                match entries.last() {
                    Some(dernier) if *dernier <= entry => entries.push(entry),
                    None => entries.push(entry),
                    Some(_) => {
                        let pos = entries.partition_point(|e| *e <= entry);
                        entries.insert(pos, entry);
                    }
                }
            }
            // L'ouverture passe par la même file que les écritures : c'est
            // ce qui garantit qu'aucune ligne ne peut la précéder.
            WriteCmd::Open(channel, file) => {
                let len = file.metadata().map(|m| m.len()).unwrap_or(0);
                files.insert(channel, Log { file, len });
            }
            WriteCmd::Close(channel) => {
                files.remove(&channel);
                index.lock().unwrap().remove(&channel);
            }
        }
    }
}

/// Ordres envoyés au fil d'écriture.
enum WriteCmd {
    /// L'horodatage voyage avec la ligne : c'est la clé de l'index, et le fil
    /// d'écriture ne va pas redésérialiser ce qu'on vient de sérialiser.
    Line(ChannelId, u64, String),
    Open(ChannelId, File),
    Close(ChannelId),
}

/// Un journal ouvert en écriture, et sa taille courante.
///
/// La taille est tenue à jour plutôt qu'interrogée : c'est elle qui donne la
/// position de la prochaine ligne, et demander au système la taille d'un
/// fichier à chaque message serait un appel système de plus par message.
struct Log {
    file: File,
    len: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("ki-chat-history-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn text_channel(id: ChannelId) -> ChannelInfo {
        ChannelInfo {
            id,
            name: format!("salon-{id}"),
            kind: ki_protocol::ChannelKind::Text,
            position: 0,
            locked: false,
            allowed_roles: None,
        }
    }

    fn record(text: &str) -> ChatRecord {
        ChatRecord { user_id: 1, username: "kevin".into(), text: text.into(), ts: 42 }
    }

    fn stamped(n: u64) -> ChatRecord {
        ChatRecord { user_id: 1, username: "kevin".into(), text: format!("m{n}"), ts: n }
    }

    fn dit(user_id: ki_protocol::UserId, ts: u64, text: &str) -> ChatRecord {
        ChatRecord { user_id, username: format!("u{user_id}"), text: text.into(), ts }
    }

    /// La recherche ignore la casse, traverse les salons, et rend les
    /// résultats du plus ancien au plus récent.
    #[test]
    fn la_recherche_ignore_la_casse_et_traverse_les_salons() {
        let dir = scratch("recherche");
        let history = History::open(&dir, &[text_channel(1), text_channel(2)]).unwrap();
        history.append(1, &dit(1, 10, "on se fait un Valorant ?"));
        history.append(2, &dit(2, 20, "VALORANT à 21h"));
        history.append(1, &dit(3, 30, "moi je suis sur Deadlock"));

        let (hits, more) = history.search(&dir, &[1, 2], "valorant", 10);
        assert_eq!(hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(), vec![10, 20]);
        assert_eq!(hits.iter().map(|(c, _)| *c).collect::<Vec<_>>(), vec![1, 2]);
        assert!(!more, "les deux résultats tiennent dans la limite");

        // Restreindre à un salon ne rend que celui-là : c'est ce qui rend la
        // garde de permission efficace côté serveur, qui ne fait que réduire
        // cette liste.
        let (hits, _) = history.search(&dir, &[2], "valorant", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 2);
    }

    /// Le tamis en octets ne doit **jamais** faire manquer un résultat que la
    /// lecture complète aurait trouvé — c'est tout l'enjeu de l'optimisation,
    /// et le seul endroit où elle pourrait mentir.
    ///
    /// Le journal est écrit à la main, au-delà de `MEM_CAP`, et les messages
    /// cherchés sont **au début** : le cache mémoire ne les a pas. Sans cette
    /// précaution le test passait quoi qu'il arrive — c'est le cache qui
    /// rattrapait le tamis, et le trou restait invisible.
    #[test]
    fn le_tamis_ne_manque_aucun_resultat() {
        let dir = scratch("tamis");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let mut bytes = Vec::new();
        // Trois pièges, chacun visant une façon différente de manquer un
        // résultat : l'accent majuscule que les octets ne ramènent pas au
        // minuscule, et les deux caractères que JSON réécrit dans le fichier.
        let pieges = [
            (1u64, "ÉNORME partie hier"),
            (2, "un guillemet \" au milieu"),
            (3, "un antislash \\ au milieu"),
            (4, "sur deux\nlignes"),
        ];
        for (ts, texte) in pieges {
            bytes.extend_from_slice(serde_json::to_string(&dit(1, ts, texte)).unwrap().as_bytes());
            bytes.push(b'\n');
        }
        for n in 100..(100 + MEM_CAP as u64) {
            bytes.extend_from_slice(serde_json::to_string(&stamped(n)).unwrap().as_bytes());
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();

        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        for (besoin, attendu) in [
            ("énorme", 1),
            ("guillemet \"", 2),
            ("antislash \\", 3),
            ("deux\nlignes", 4),
            // Le cas courant, celui qui passe par le tamis : il doit trouver.
            ("partie", 1),
        ] {
            let (hits, _) = history.search(&dir, &[1], besoin, 10);
            assert_eq!(
                hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(),
                vec![attendu],
                "« {besoin} » aurait dû être trouvé"
            );
        }
    }

    /// Le pseudo n'est pas le texte : chercher un nom ne doit pas rendre tout
    /// ce que cette personne a écrit. Le tamis, lui, voit la ligne entière —
    /// c'est la vérification après désérialisation qui tranche.
    #[test]
    fn la_recherche_ne_porte_que_sur_le_texte() {
        let dir = scratch("texte-seul");
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        history.append(1, &dit(7, 10, "salut"));
        history.append(1, &dit(1, 20, "u7 tu viens ?"));

        let (hits, _) = history.search(&dir, &[1], "u7", 10);
        assert_eq!(hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(), vec![20]);
    }

    /// Un message présent **à la fois** dans le fichier et dans le cache ne
    /// doit apparaître qu'une fois.
    ///
    /// C'est exactement la situation ordinaire : le cache est la queue du
    /// fichier, les mêmes messages y sont donc deux fois. Le journal est
    /// écrit à la main puis relu à l'ouverture pour que ce recouvrement soit
    /// certain — l'écrire par `append` le ferait dépendre de l'avance du fil
    /// d'écriture, et le test passerait ou non selon la machine.
    #[test]
    fn un_message_present_des_deux_cotes_ne_sort_qu_une_fois() {
        let dir = scratch("doublon");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let mut bytes = Vec::new();
        for n in 1..=3u64 {
            bytes.extend_from_slice(
                serde_json::to_string(&dit(1, n, "encore un test")).unwrap().as_bytes(),
            );
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();
        let history = History::open(&dir, &[text_channel(1)]).unwrap();

        // Limite large : rien n'est écarté faute de place, donc tout doublon
        // se voit.
        let (hits, more) = history.search(&dir, &[1], "test", 50);
        assert_eq!(
            hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "chaque message une seule fois"
        );
        assert!(!more);
    }

    /// Trop de résultats : on garde les plus **récents**, et on le dit.
    ///
    /// Le journal est écrit à la main puis relu à l'ouverture : le fichier
    /// **et** le cache mémoire portent alors les dix messages. C'est le cas
    /// du doublon, et il doit être déterministe — l'écrire par `append`
    /// faisait dépendre le résultat de l'avance du fil d'écriture, si bien
    /// que ce test passait sur une machine et échouait sur une autre. Il a
    /// d'ailleurs fallu l'intégration continue pour le découvrir.
    #[test]
    fn la_recherche_garde_les_plus_recents_et_l_annonce() {
        let dir = scratch("trop");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let mut bytes = Vec::new();
        for n in 1..=10u64 {
            bytes.extend_from_slice(
                serde_json::to_string(&dit(1, n, "encore un test")).unwrap().as_bytes(),
            );
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();
        let history = History::open(&dir, &[text_channel(1)]).unwrap();

        let (hits, more) = history.search(&dir, &[1], "test", 3);
        assert_eq!(hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(), vec![8, 9, 10]);
        assert!(more, "sept résultats ont été laissés de côté");

        // Et un message qui n'a PAS encore atteint le disque compte quand
        // même : c'est l'autre moitié du contrat, et elle ne doit pas se
        // faire écraser par la correction du doublon.
        history.append(1, &dit(1, 11, "encore un test"));
        let (hits, _) = history.search(&dir, &[1], "test", 3);
        assert_eq!(hits.iter().map(|(_, r)| r.ts).collect::<Vec<_>>(), vec![9, 10, 11]);
    }

    /// Remonter le fil doit rendre les messages **antérieurs**, du plus
    /// ancien au plus récent, et dire honnêtement s'il en reste avant : un
    /// `more` toujours vrai ferait boucler le client indéfiniment.
    #[test]
    fn paging_walks_backwards_until_the_start() {
        let dir = scratch("before");
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        for n in 1..=10 {
            history.append(1, &stamped(n));
        }

        // Les 3 messages précédant le 8e : 5, 6, 7 — dans cet ordre.
        let (page, more) = history.before(&dir, 1, 8, 3);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![5, 6, 7]);
        assert!(more, "il reste 1 à 4 avant");

        // Au ras du début : on ne rend que ce qui existe, et plus rien après.
        let (page, more) = history.before(&dir, 1, 3, 10);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![1, 2]);
        assert!(!more, "1 et 2 épuisent le salon");

        // Avant le tout premier message : page vide, et surtout pas de
        // `more` — sinon le client redemanderait sans fin.
        let (page, more) = history.before(&dir, 1, 1, 10);
        assert!(page.is_empty());
        assert!(!more);

        // Un salon inconnu ne doit pas paniquer.
        assert_eq!(history.before(&dir, 999, 100, 10).0.len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Le cas que l'index existe pour servir : plus de messages que le cache
    /// mémoire n'en garde, donc une pagination qui doit **vraiment** aller
    /// chercher sur le disque.
    ///
    /// Le fichier est écrit à la main pour dépasser `MEM_CAP` sans attendre
    /// que le fil d'écriture ait fini : c'est `scan` qui construit l'index à
    /// l'ouverture, et c'est lui qu'on veut éprouver.
    #[test]
    fn la_pagination_va_chercher_au_dela_du_cache_memoire() {
        let dir = scratch("index-disque");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let total = MEM_CAP + 500;
        let mut bytes = Vec::new();
        for n in 1..=total {
            bytes.extend_from_slice(
                serde_json::to_string(&stamped(n as u64)).unwrap().as_bytes(),
            );
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();

        let history = History::open(&dir, &[text_channel(1)]).unwrap();

        // Bien au-delà de ce que la mémoire garde : seul l'index peut
        // répondre.
        let (page, more) = history.before(&dir, 1, 100, 5);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![95, 96, 97, 98, 99]);
        assert!(more, "il reste 1 à 94 avant");

        // Au tout début du salon : on rend ce qui existe, et l'on annonce
        // qu'il n'y a plus rien avant — sans quoi le client boucle.
        let (page, more) = history.before(&dir, 1, 4, 10);
        assert_eq!(page.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert!(!more);

        // Et la remontée complète page par page retrouve tout, dans l'ordre.
        let mut vus = Vec::new();
        let mut curseur = 200u64;
        loop {
            let (page, more) = history.before(&dir, 1, curseur, 25);
            if page.is_empty() {
                break;
            }
            curseur = page[0].ts;
            let mut ts: Vec<u64> = page.iter().map(|r| r.ts).collect();
            ts.extend(vus);
            vus = ts;
            if !more {
                break;
            }
        }
        assert_eq!(vus, (1..200).collect::<Vec<u64>>());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// L'horloge murale peut reculer — ajustement NTP, machine virtuelle qui
    /// se réveille — et le fichier n'est alors pas dans l'ordre du temps.
    /// L'index étant trié, la pagination doit quand même rendre les messages
    /// du plus ancien au plus récent.
    #[test]
    fn une_horloge_qui_recule_ne_desordonne_pas_la_pagination() {
        let dir = scratch("index-horloge");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        // Écrits dans le désordre, exprès.
        let mut bytes = Vec::new();
        for n in [50u64, 10, 40, 20, 30] {
            bytes.extend_from_slice(serde_json::to_string(&stamped(n)).unwrap().as_bytes());
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();

        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        let (page, _) = history.before(&dir, 1, 45, 10);
        assert_eq!(
            page.iter().map(|r| r.ts).collect::<Vec<_>>(),
            vec![10, 20, 30, 40],
            "l'index trié rend le passé dans l'ordre du temps, pas du fichier"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Le disque est écrit ailleurs, mais la mémoire, elle, est à jour tout de
    /// suite : sans ça un client qui poste puis demande son historique ne
    /// verrait pas son propre message.
    #[test]
    fn recent_sees_a_message_right_after_append() {
        let history = History::open(&scratch("immediate"), &[text_channel(1)]).unwrap();
        history.append(1, &record("coucou"));
        let seen = history.recent(1, 10);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].text, "coucou");
        // Un salon inconnu ne fabrique pas d'historique au passage.
        history.append(9, &record("nulle part"));
        assert!(history.recent(9, 10).is_empty());
    }

    /// Rien ne se perd ni ne se réordonne entre l'appel et le fichier.
    #[test]
    fn every_line_reaches_the_file_in_order() {
        let dir = scratch("ordered");
        {
            let history = History::open(&dir, &[text_channel(1), text_channel(2)]).unwrap();
            for i in 0..200 {
                history.append(1, &record(&format!("m{i}")));
            }
            history.append(2, &record("autre salon"));
            // La sortie de bloc attend le fil d'écriture.
        }
        // Nouveau processus : ce qu'on relit est ce qui a atteint le disque.
        let history = History::open(&dir, &[text_channel(1), text_channel(2)]).unwrap();
        let seen = history.recent(1, 1000);
        assert_eq!(seen.len(), 200);
        assert_eq!(seen[0].text, "m0");
        assert_eq!(seen[199].text, "m199");
        // Les salons ne se mélangent pas.
        let autre = history.recent(2, 10);
        assert_eq!(autre.len(), 1);
        assert_eq!(autre[0].text, "autre salon");
    }

    /// Une ligne abîmée (octets non-UTF-8 d'un fichier tronqué) ne doit pas
    /// empêcher le serveur de démarrer : elle est sautée, le reste est lu.
    #[test]
    fn a_corrupt_line_does_not_stop_the_server_from_starting() {
        let dir = scratch("corrupt-line");
        let path = PathBuf::from(&dir).join("channel-1.jsonl");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(serde_json::to_string(&stamped(1)).unwrap().as_bytes());
        bytes.push(b'\n');
        // Ligne coupée au milieu d'un caractère : octets non-UTF-8.
        bytes.extend_from_slice(&[0xff, 0xfe, 0x00]);
        bytes.push(b'\n');
        bytes.extend_from_slice(serde_json::to_string(&stamped(3)).unwrap().as_bytes());
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();

        // Ne panique pas, et lit les deux lignes saines de part et d'autre.
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        let seen = history.recent(1, 10);
        assert_eq!(seen.iter().map(|r| r.ts).collect::<Vec<_>>(), vec![1, 3]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Une réponse d'historique ne doit jamais dépasser une ligne de contrôle :
    /// au-delà, le client la refuse et se déconnecte. On en rend donc moins que
    /// demandé plutôt que de produire un salon « piège ».
    #[test]
    fn a_history_response_stays_within_a_control_line() {
        let dir = scratch("budget");
        let history = History::open(&dir, &[text_channel(1)]).unwrap();
        let big = "x".repeat(ki_protocol::MAX_CHAT_TEXT);
        for n in 1..=100 {
            history.append(
                1,
                &ChatRecord { user_id: 1, username: "k".into(), text: big.clone(), ts: n },
            );
        }

        // Ce qui est mesuré est le **message réellement émis**, enveloppe
        // comprise, et non le tableau nu : c'est cette ligne-là que le client
        // refuse au-delà de MAX_LINE, saut de ligne inclus.
        let line_of = |msg: &ki_protocol::ServerMsg| serde_json::to_string(msg).unwrap().len() + 1;

        // 100 messages de 4000 caractères dépasseraient largement MAX_LINE.
        let page = history.recent(1, 100);
        assert!(page.len() < 100, "la réponse aurait dû être tronquée");
        assert!(!page.is_empty());
        // Ce sont les plus récents qui sont conservés.
        assert_eq!(page.last().unwrap().ts, 100);
        let sent = ki_protocol::ServerMsg::History { messages: page };
        assert!(line_of(&sent) <= ki_protocol::MAX_LINE, "ligne de {} octets", line_of(&sent));

        // Et en remontant le fil, la page reste elle aussi bornée, en signalant
        // honnêtement qu'il en reste avant.
        let (older, more) = history.before(&dir, 1, 101, 100);
        assert!(more, "il reste des messages plus anciens à charger");
        let sent = ki_protocol::ServerMsg::HistoryPage { messages: older, more, channel: 1 };
        assert!(line_of(&sent) <= ki_protocol::MAX_LINE, "ligne de {} octets", line_of(&sent));

        std::fs::remove_dir_all(&dir).ok();
    }
}
