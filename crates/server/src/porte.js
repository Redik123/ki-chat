// La porte web de ki-chat — le script de porte.html, servi à côté par le
// serveur. Aucune dépendance, aucun HTML fabriqué : tout ce qui vient du
// réseau passe par textContent.
//
// Le fil de la page :
//   nom → attente → salon → fermé
//                 ↘ refus
// Une seule WebSocket, ouverte à la demande d'accès (`hello`), tenue tant
// que le salon vit, et rouverte toute seule si elle tombe. Le serveur parle
// le même JSON que le client ki-chat (ServerMsg, un objet par ligne, `type`
// en snake_case) ; la page n'envoie que `hello`, `chat`, `ping` et `vocal`.
//
// Le vocal passe par la même WebSocket, en trames binaires :
//   montant     [u8 version=1][u64 LE compteur][Opus 48 kHz mono 20 ms]
//   descendant  [u8 version=1][u64 LE id du locuteur][u64 LE compteur][Opus]
// Le micro est découpé en trames de 960 échantillons par un AudioWorklet,
// encodé par WebCodecs (AudioEncoder « opus »), et ce qui arrive est
// décodé par locuteur (AudioDecoder), mis en tampon contre la gigue et
// mixé dans un second AudioWorklet. Sans WebCodecs (Safari), le texte
// marche et le vocal se déclare indisponible.
//
// Ce fichier est chargé deux fois : par la page, et par
// audioWorklet.addModule() comme module du fil audio — la politique de
// sécurité de contenu (script-src 'self') n'admettrait pas un module en
// blob:. Dans le fil audio, il ne définit que les deux processeurs.
(function () {
  'use strict';

  if (typeof registerProcessor === 'function') {
    definirProcesseurs();
    return;
  }

  // ---- Constantes partagées avec le protocole (crates/protocol) ----
  var MAX_TEXTE = 4000;         // MAX_CHAT_TEXT
  var NOM_MIN = 3;
  var NOM_MAX = 32;             // MAX_USERNAME
  var SUFFIXE_WEB = ' (web)';   // INVITE_SUFFIXE
  var INVITE_ID_BASE = Math.pow(2, 62);
  var INVITE_ID_FIN = Math.pow(2, 63);
  var ATTENTE_MAX_MS = 5 * 60 * 1000;
  // Cadence d'envoi montrée à l'invité : trois messages d'un coup, puis un
  // toutes les trois secondes. Le serveur a le dernier mot (il répond
  // `error` si on va trop vite) ; ici on prévient avant.
  var DEBIT_RAFALE = 3;
  var DEBIT_RECHARGE_MS = 3000;
  var RECUL_MIN_MS = 1000;
  var RECUL_MAX_MS = 30000;

  // ---- Constantes du vocal ----
  var VOCAL_VERSION = 1;
  var VOCAL_FREQUENCE = 48000;
  var VOCAL_TRAME = 960;                    // 20 ms à 48 kHz
  var VOCAL_TRAME_US = 20000;
  var VOCAL_DEBIT = 32000;                  // bit/s
  var VOCAL_ENTETE_MONTANT = 1 + 8;         // version + compteur
  var VOCAL_ENTETE_DESCENDANT = 1 + 8 + 8;  // version + locuteur + compteur
  var VOCAL_MAINTIEN_MS = 400;              // la voix retombée, on émet encore un peu
  var VOCAL_SEUIL_DB = -40;                 // ~0.01 d'amplitude, comme le client
  var VOCAL_PARLE_MS = 250;                 // « X parle » tant qu'il arrive du son
  var VOCAL_OUBLI_MS = 60000;               // un locuteur muet est oublié (décodeur fermé)
  var VOCAL_LOCUTEURS_MAX = 32;
  var VOCAL_PING_MS = 5000;
  var VOCAL_AFFICHAGE_MS = 150;
  var CONFIG_ENCODEUR = {
    codec: 'opus', sampleRate: VOCAL_FREQUENCE, numberOfChannels: 1, bitrate: VOCAL_DEBIT,
    opus: { frameDuration: VOCAL_TRAME_US, application: 'voip' }
  };
  var CONFIG_DECODEUR = { codec: 'opus', sampleRate: VOCAL_FREQUENCE, numberOfChannels: 1 };
  // Le module du fil audio : ce fichier même (voir en tête).
  var URL_SCRIPT = (document.currentScript && document.currentScript.src) || '/s/porte.js';

  // ---- Éléments ----
  var $ = function (id) { return document.getElementById(id); };
  var el = {
    nomServeur: $('nom-serveur'), nomPorte: $('nom-porte'), statut: $('statut'),
    etats: {
      nom: $('etat-nom'), attente: $('etat-attente'), refus: $('etat-refus'),
      salon: $('etat-salon'), ferme: $('etat-ferme')
    },
    formNom: $('form-nom'), prenom: $('prenom'), apercuNom: $('apercu-nom'),
    erreurNom: $('erreur-nom'), boutonDemander: $('bouton-demander'),
    attenteInfo: $('attente-info'), attenteCompteur: $('attente-compteur'),
    boutonAnnuler: $('bouton-annuler'),
    refusMotif: $('refus-motif'), boutonReessayer: $('bouton-reessayer'),
    presenceListe: $('presence-liste'),
    bandeau: $('bandeau'), bandeauTexte: $('bandeau-texte'), bandeauFermer: $('bandeau-fermer'),
    fil: $('fil'),
    formSaisie: $('form-saisie'), texte: $('texte'), compteurTexte: $('compteur-texte'),
    debit: $('debit'), boutonEnvoyer: $('bouton-envoyer'),
    boutonVocal: $('bouton-vocal'), vocalNote: $('vocal-note'), vocalControles: $('vocal-controles'),
    vocalSalon: $('vocal-salon'), vocalIndicateur: $('vocal-indicateur'), vocalLatence: $('vocal-latence'),
    vocalOccupants: $('vocal-occupants'), vocalQui: $('vocal-qui'),
    vocalNiveauBarre: $('vocal-niveau-barre'), vocalNiveauSeuil: $('vocal-niveau-seuil'),
    vocalParler: $('vocal-parler'), vocalSourdine: $('vocal-sourdine'), vocalMode: $('vocal-mode'),
    vocalSeuilLigne: $('vocal-seuil-ligne'), vocalSeuil: $('vocal-seuil'),
    vocalVolume: $('vocal-volume'), vocalQuitter: $('vocal-quitter'),
    fermeMotif: $('ferme-motif'),
    carteInvitation: $('carte-invitation'), invitationServeur: $('invitation-serveur'),
    invitationCode: $('invitation-code'), invitationTelechargement: $('invitation-telechargement'),
    invitationReplier: $('invitation-replier')
  };

  // ---- Où sommes-nous ? ----
  // https://serveur/s/salon1 → slug « salon1 », WebSocket sur /s/salon1/ws.
  var chemin = location.pathname.replace(/\/+$/, '');
  var slug = chemin.slice(chemin.lastIndexOf('/') + 1) || '';
  var urlWs = (location.protocol === 'https:' ? 'wss://' : 'ws://') + location.host + chemin + '/ws';

  var meta = document.querySelector('meta[name="ki-serveur"]');
  var nomServeur = meta ? meta.getAttribute('content') || '' : '';
  if (!nomServeur || nomServeur.indexOf('{{') !== -1) nomServeur = location.hostname;
  el.nomServeur.textContent = nomServeur;
  el.nomPorte.textContent = slug || '?';
  document.title = 'ki-chat — ' + nomServeur;

  // ---- État ----
  var etat = 'nom';
  var nom = '';              // ce que l'invité a tapé
  var nomAffiche = '';       // ce que le serveur en fait : « Kevin (web) »
  var ws = null;
  var fermetureVoulue = false;   // on a fermé nous-mêmes, pas de reprise
  var terminal = false;          // refus, salon fermé : plus jamais de reprise
  // La connexion est tombée depuis le salon et l'on refrappe : le serveur
  // n'a pas de reprise de session, un `hello` est une nouvelle demande.
  var reprise = false;
  var motifReprise = '';         // pourquoi on réessaie, pour l'écran d'attente
  var recul = RECUL_MIN_MS;
  var minuteurReprise = null;
  var attenteDepuis = 0;
  var minuteurAttente = null;
  var messages = new Map();      // « user_id:ts » → { li, reactions: Map }
  var presents = new Map();      // user_id → { nom, web }
  var jetons = DEBIT_RAFALE;
  var derniereRecharge = Date.now();

  // ---- Petits outils ----
  function montrer(nouvelEtat) {
    etat = nouvelEtat;
    document.documentElement.setAttribute('data-etat', nouvelEtat);
    Object.keys(el.etats).forEach(function (k) {
      el.etats[k].hidden = k !== nouvelEtat;
    });
    if (nouvelEtat === 'nom') {
      el.prenom.focus();
    } else if (nouvelEtat === 'attente') {
      el.boutonAnnuler.focus();
    } else if (nouvelEtat === 'refus') {
      el.boutonReessayer.focus();
    } else if (nouvelEtat === 'salon') {
      el.texte.focus();
      defilerEnBas();
    }
  }

  function statut(texte, ton) {
    el.statut.textContent = texte || '';
    el.statut.className = 'statut' + (ton ? ' ' + ton : '');
  }

  function bandeau(texte, ton) {
    if (!texte) { el.bandeau.hidden = true; return; }
    el.bandeauTexte.textContent = texte;
    el.bandeau.className = 'bandeau' + (ton ? ' ' + ton : '');
    el.bandeau.hidden = false;
  }

  function heure(ts) {
    var d = new Date(Number(ts) || Date.now());
    var h = d.getHours(), m = d.getMinutes();
    return (h < 10 ? '0' : '') + h + ':' + (m < 10 ? '0' : '') + m;
  }

  function estInvite(id) {
    id = Number(id);
    return id >= INVITE_ID_BASE && id < INVITE_ID_FIN;
  }

  function estMoi(id, username) {
    return nomAffiche ? username === nomAffiche : (estInvite(id) && username === nom + SUFFIXE_WEB);
  }

  function sansSuffixe(username) {
    var s = String(username || '');
    return s.slice(-SUFFIXE_WEB.length) === SUFFIXE_WEB ? s.slice(0, -SUFFIXE_WEB.length) : s;
  }

  function cle(ref) { return String(ref.user_id) + ':' + String(ref.ts); }

  function borner(texte, max) {
    var s = String(texte == null ? '' : texte);
    // Le serveur retire déjà les caractères de contrôle et bidi ; on ne
    // fait ici que borner, au cas où.
    return s.length > max ? s.slice(0, max) + '…' : s;
  }

  function memoriser(clef, valeur) {
    try { sessionStorage.setItem(clef, valeur); } catch (e) { /* navigation privée */ }
  }
  function rappeler(clef) {
    try { return sessionStorage.getItem(clef) || ''; } catch (e) { return ''; }
  }

  // ---- Le nom (a) ----
  function nomValide(s) {
    var n = s.trim().replace(/\s+/g, ' ');
    if (n.length < NOM_MIN) return { erreur: 'Au moins ' + NOM_MIN + ' caractères.' };
    if (n.length > NOM_MAX) return { erreur: 'Au plus ' + NOM_MAX + ' caractères.' };
    if (/[\u0000-\u001f\u007f]/.test(n)) return { erreur: 'Pas de caractère de contrôle.' };
    if (n.slice(-SUFFIXE_WEB.length) === SUFFIXE_WEB) n = n.slice(0, -SUFFIXE_WEB.length).trim();
    if (n.length < NOM_MIN) return { erreur: 'Au moins ' + NOM_MIN + ' caractères.' };
    return { nom: n };
  }

  el.prenom.addEventListener('input', function () {
    var v = el.prenom.value.trim().replace(/\s+/g, ' ');
    el.apercuNom.textContent = v || 'Kevin';
    el.erreurNom.hidden = true;
    el.prenom.removeAttribute('aria-invalid');
  });

  function erreurNom(texte) {
    el.erreurNom.textContent = texte;
    el.erreurNom.hidden = false;
    el.prenom.setAttribute('aria-invalid', 'true');
    el.prenom.focus();
  }

  el.formNom.addEventListener('submit', function (ev) {
    ev.preventDefault();
    var r = nomValide(el.prenom.value);
    if (r.erreur) { erreurNom(r.erreur); return; }
    nom = r.nom;
    nomAffiche = '';
    memoriser('ki-porte-nom-' + slug, nom);
    terminal = false;
    fermetureVoulue = false;
    reprise = false;
    motifReprise = '';
    recul = RECUL_MIN_MS;
    commencerAttente();
    connecter();
  });

  // ---- L'attente (b) ----
  function commencerAttente(texte) {
    attenteDepuis = Date.now();
    el.attenteInfo.textContent = texte || 'Quelqu’un va t’ouvrir. Reste sur cette page.';
    majCompteurAttente();
    clearInterval(minuteurAttente);
    minuteurAttente = setInterval(majCompteurAttente, 1000);
    montrer('attente');
    statut('en attente');
  }

  function majCompteurAttente() {
    var s = Math.floor((Date.now() - attenteDepuis) / 1000);
    var m = Math.floor(s / 60);
    s = s % 60;
    el.attenteCompteur.textContent = m + ':' + (s < 10 ? '0' : '') + s;
    if (Date.now() - attenteDepuis > ATTENTE_MAX_MS && etat === 'attente' && (!ws || ws.readyState !== 1)) {
      // Le serveur a dû lâcher la demande et on n'arrive plus à le joindre.
      finirAttente('Personne n’a répondu à temps. Tu peux refrapper.');
    }
  }

  function finirAttente(motif) {
    clearInterval(minuteurAttente);
    terminal = true;
    fermerWs(1000);
    el.refusMotif.textContent = motif;
    montrer('refus');
    statut('');
  }

  el.boutonAnnuler.addEventListener('click', function () {
    clearInterval(minuteurAttente);
    fermetureVoulue = true;
    reprise = false;
    motifReprise = '';
    fermerWs(1000);
    montrer('nom');
    statut('');
  });

  el.boutonReessayer.addEventListener('click', function () {
    terminal = false;
    montrer('nom');
  });

  // ---- La WebSocket ----
  function connecter() {
    clearTimeout(minuteurReprise);
    if (ws && (ws.readyState === 0 || ws.readyState === 1)) return;
    var socket;
    try {
      socket = new WebSocket(urlWs);
    } catch (e) {
      planifierReprise();
      return;
    }
    socket.binaryType = 'arraybuffer';   // les trames du vocal
    ws = socket;
    statut(etat === 'salon' ? 'reconnexion…' : 'connexion…');

    socket.onopen = function () {
      if (socket !== ws) return;
      recul = RECUL_MIN_MS;
      motifReprise = '';
      // Depuis le salon : le serveur ne reprend pas une session, ce `hello`
      // est une nouvelle demande que les membres doivent accepter. On le
      // montre — l'attente, pas « en ligne ». Le fil reste tel quel : le
      // `history` qui suivra l'acceptation nous y ramène, dédoublonné.
      if (etat === 'salon') {
        reprise = true;
        commencerAttente('Connexion rétablie : on redemande l’accès aux membres.');
      }
      envoyer({ type: 'hello', nom: nom });
      // À la reprise, si on était en vocal, on le redit tout de suite
      // après le `hello`, et ce qui attendait dans les files d'écoute est
      // périmé. Le serveur, lui, repart d'une nouvelle demande : il répond
      // `porte_vocal_fin`, et on sort proprement — jusqu'à ce qu'un membre
      // nous y ramène.
      if (vocal.actif) { direVocal(true); viderEcoute(); }
      statut(etat === 'salon' ? 'en ligne' : 'en attente', etat === 'salon' ? 'ok' : '');
      if (etat === 'salon') bandeau('');
    };

    socket.onmessage = function (ev) {
      if (socket !== ws) return;
      if (ev.data instanceof ArrayBuffer) { trameVocaleRecue(ev.data); return; }
      if (typeof ev.data !== 'string') return;
      ev.data.split('\n').forEach(function (ligne) {
        if (!ligne.trim()) return;
        var msg;
        try { msg = JSON.parse(ligne); } catch (e) { return; }
        if (msg && typeof msg === 'object') recevoir(msg);
      });
    };

    socket.onclose = function (ev) {
      if (socket !== ws) return;
      ws = null;
      if (fermetureVoulue || terminal) return;
      // 4xxx : le serveur nous dit que c'est fini pour de bon.
      if (ev.code >= 4000 && ev.code < 5000) {
        terminer(ev.reason || 'Le serveur a fermé la porte.');
        return;
      }
      planifierReprise();
    };

    socket.onerror = function () { /* onclose suit toujours */ };
  }

  // Fermeture voulue : plus de reprise, et le vocal s'arrête avec.
  function fermerWs(code) {
    clearTimeout(minuteurReprise);
    quitterVocal(true);
    var s = ws;
    ws = null;
    if (s) { try { s.close(code || 1000); } catch (e) { /* déjà fermée */ } }
  }

  function envoyer(objet) {
    if (!ws || ws.readyState !== 1) return false;
    try { ws.send(JSON.stringify(objet)); return true; } catch (e) { return false; }
  }

  function planifierReprise() {
    clearTimeout(minuteurReprise);
    var gigue = 1 + (Math.random() * 0.4 - 0.2);
    var delai = Math.round(recul * gigue);
    recul = Math.min(recul * 2, RECUL_MAX_MS);
    var secondes = Math.max(1, Math.round(delai / 1000));
    statut('hors ligne', 'danger');
    if (etat === 'salon') {
      bandeau('Connexion perdue — nouvel essai dans ' + secondes + ' s.', 'alerte');
    } else if (etat === 'attente') {
      el.attenteInfo.textContent = (motifReprise || 'Le serveur ne répond pas') + ' — nouvel essai dans ' + secondes + ' s.';
    }
    minuteurReprise = setTimeout(connecter, delai);
  }

  // Refus, expulsion, salon fermé : le message qui convient à l'état.
  function terminer(motif) {
    terminal = true;
    clearInterval(minuteurAttente);
    fermerWs(1000);
    if (etat === 'salon' || etat === 'ferme') {
      el.fermeMotif.textContent = motif || 'Le salon a fermé.';
      montrer('ferme');
    } else {
      el.refusMotif.textContent = motif || 'La demande n’a pas abouti.';
      montrer('refus');
    }
    statut('');
  }

  // ---- Ce que le serveur envoie ----
  function recevoir(msg) {
    switch (msg.type) {
      case 'history':
        entrerDansLeSalon();
        (Array.isArray(msg.messages) ? msg.messages : []).forEach(function (m) {
          ajouterMessage(m, true);
        });
        defilerEnBas();
        break;
      case 'chat':
        if (etat !== 'salon') entrerDansLeSalon();
        ajouterMessage(msg, false);
        break;
      case 'message_edited':
        modifierMessage(msg);
        break;
      case 'message_deleted':
        supprimerMessage(msg);
        break;
      case 'reaction':
        reagir(msg);
        break;
      case 'info':
        if (etat === 'salon') {
          ajouterLigne('info', borner(msg.message, 500));
          reconnaitrePresence(msg.message);
        } else if (etat === 'attente') {
          el.attenteInfo.textContent = (reprise ? 'Connexion rétablie — ' : '') + borner(msg.message, 300);
        }
        break;
      case 'error':
        if (etat === 'salon') {
          if (erreurDeControleVocal(msg.message)) break;
          bandeau(borner(msg.message, 300), 'danger');
        } else if (etat === 'attente' && reprise && /déjà pris/.test(String(msg.message || ''))) {
          // Notre ancienne session est encore là — le serveur la garde
          // jusqu'à une minute de silence — et notre nom avec elle. On ne
          // retombe pas sur l'écran du nom : le serveur ferme derrière, et
          // `onclose` replanifie l'essai, en le disant.
          motifReprise = 'Ton ancienne session est encore là';
        } else {
          clearInterval(minuteurAttente);
          fermetureVoulue = true;
          fermerWs(1000);
          montrer('nom');
          statut('');
          erreurNom(borner(msg.message, 300) || 'Le serveur a refusé la demande.');
        }
        break;
      case 'kicked':
        terminer(etat === 'salon'
          ? (msg.reason ? 'Tu as été retiré du salon : ' + borner(msg.reason, 300) : 'Tu as été retiré du salon.')
          : (msg.reason ? borner(msg.reason, 300) : 'Un membre a refusé ta demande.'));
        break;
      case 'porte_fermee':
        terminer(msg.motif ? 'Le salon a fermé : ' + borner(msg.motif, 300) : 'Le salon a fermé.');
        break;
      case 'porte_invitation':
        afficherInvitation(msg);
        break;
      case 'members':
        (Array.isArray(msg.members) ? msg.members : []).forEach(function (m) {
          presents.set(String(m.user_id), { nom: String(m.username || ''), web: !!m.invite || estInvite(m.user_id) });
        });
        rendrePresence();
        break;
      case 'user_joined':
        presents.set(String(msg.user_id), { nom: String(msg.username || ''), web: estInvite(msg.user_id) });
        rendrePresence();
        break;
      case 'user_left':
        presents.delete(String(msg.user_id));
        rendrePresence();
        break;
      case 'porte_vocal':
        // Un membre nous a mis dans un salon vocal : on peut le rejoindre.
        // Le même message avec `channel` à null nous en sort.
        if (Object.prototype.hasOwnProperty.call(msg, 'channel') && msg.channel === null) vocalFini();
        else vocalOuvert(msg);
        break;
      case 'porte_vocal_fin':
        vocalFini();
        break;
      case 'porte_vocal_occupants':
        vocalOccupants(msg.occupants);
        break;
      case 'pong':
        if (vocal.pingEnvoye) {
          vocal.rtt = performance.now() - vocal.pingEnvoye;
          vocal.pingEnvoye = 0;
        }
        break;
      default:
        break;
    }
  }

  function entrerDansLeSalon() {
    if (etat === 'salon') return;
    clearInterval(minuteurAttente);
    reprise = false;
    motifReprise = '';
    presents.set('moi', { nom: nom + SUFFIXE_WEB, web: true, moi: true });
    montrer('salon');
    statut('en ligne', 'ok');
    bandeau('');
    majBoutonVocal();
  }

  // ---- Le fil (d) ----
  function ajouterMessage(m, historique) {
    if (m == null || typeof m !== 'object') return;
    var k = cle(m);
    if (messages.has(k)) return; // rejoué à la reprise

    var li = document.createElement('li');
    li.className = 'msg';
    li.dataset.cle = k;
    var systeme = Number(m.user_id) === 0;
    if (systeme) li.classList.add('systeme');
    if (estInvite(m.user_id)) li.classList.add('web');
    if (estMoi(m.user_id, m.username)) {
      li.classList.add('moi');
      if (!nomAffiche) nomAffiche = String(m.username);
    }

    var tete = document.createElement('div');
    tete.className = 'msg-tete';
    var auteur = document.createElement('span');
    auteur.className = 'auteur';
    auteur.textContent = borner(sansSuffixe(m.username), NOM_MAX + 8);
    tete.appendChild(auteur);
    var h = document.createElement('span');
    h.className = 'heure';
    h.textContent = heure(m.ts);
    tete.appendChild(h);
    var modifie = document.createElement('span');
    modifie.className = 'modifie';
    modifie.textContent = m.edited ? '(modifié)' : '';
    tete.appendChild(modifie);
    li.appendChild(tete);

    if (m.reply_to && typeof m.reply_to === 'object') {
      var rep = document.createElement('p');
      rep.className = 'reponse';
      var ra = document.createElement('span');
      ra.className = 'auteur';
      ra.textContent = borner(sansSuffixe(m.reply_to.username), NOM_MAX + 8) + ' : ';
      rep.appendChild(ra);
      rep.appendChild(document.createTextNode(borner(m.reply_to.excerpt, 120)));
      li.appendChild(rep);
    }

    var texte = document.createElement('p');
    texte.className = 'texte';
    texte.textContent = borner(m.text, MAX_TEXTE);
    li.appendChild(texte);

    var reactions = document.createElement('div');
    reactions.className = 'reactions';
    li.appendChild(reactions);

    var entree = { li: li, reactions: new Map() };
    (Array.isArray(m.reactions) ? m.reactions : []).forEach(function (r) {
      if (r && typeof r.emoji === 'string') {
        entree.reactions.set(r.emoji, new Set((r.users || []).map(String)));
      }
    });
    rendreReactions(entree);
    messages.set(k, entree);

    if (systeme) {
      // « Kevin (web) a rejoint » : le serveur le poste dans le fil.
      reconnaitrePresence(m.text);
    } else if (!li.classList.contains('moi')) {
      presents.set(String(m.user_id), { nom: String(m.username || ''), web: estInvite(m.user_id) });
      if (!historique) rendrePresence();
    }

    var enBas = presDuBas();
    el.fil.appendChild(li);
    if (!historique && (enBas || li.classList.contains('moi'))) defilerEnBas();
  }

  function ajouterLigne(classe, texte) {
    var li = document.createElement('li');
    li.className = 'msg ' + classe;
    var p = document.createElement('p');
    p.className = 'texte';
    p.textContent = texte;
    li.appendChild(p);
    var enBas = presDuBas();
    el.fil.appendChild(li);
    if (enBas) defilerEnBas();
  }

  function modifierMessage(msg) {
    var e = msg.message && messages.get(cle(msg.message));
    if (!e) return;
    e.li.querySelector('.texte').textContent = borner(msg.text, MAX_TEXTE);
    e.li.querySelector('.modifie').textContent = '(modifié)';
  }

  function supprimerMessage(msg) {
    var k = msg.message && cle(msg.message);
    var e = k && messages.get(k);
    if (!e) return;
    e.li.remove();
    messages.delete(k);
  }

  function reagir(msg) {
    var e = msg.message && messages.get(cle(msg.message));
    if (!e || typeof msg.emoji !== 'string') return;
    var qui = String(msg.by);
    var ensemble = e.reactions.get(msg.emoji);
    if (msg.on) {
      if (!ensemble) { ensemble = new Set(); e.reactions.set(msg.emoji, ensemble); }
      ensemble.add(qui);
    } else if (ensemble) {
      ensemble.delete(qui);
      if (ensemble.size === 0) e.reactions.delete(msg.emoji);
    }
    rendreReactions(e);
  }

  function rendreReactions(e) {
    var zone = e.li.querySelector('.reactions');
    while (zone.firstChild) zone.removeChild(zone.firstChild);
    e.reactions.forEach(function (users, emoji) {
      if (users.size === 0) return;
      var span = document.createElement('span');
      span.className = 'reaction';
      span.textContent = borner(emoji, 8) + ' ' + users.size;
      zone.appendChild(span);
    });
  }

  function presDuBas() {
    return el.fil.scrollHeight - el.fil.scrollTop - el.fil.clientHeight < 80;
  }

  function defilerEnBas() {
    el.fil.scrollTop = el.fil.scrollHeight;
  }

  // ---- Qui est là ----
  // Le serveur ne donne pas la liste des membres à un invité : on retient
  // ceux qui ont écrit, et on lit les entrées et sorties que le serveur
  // annonce dans le fil (« Kevin (web) a rejoint »).
  function reconnaitrePresence(message) {
    var s = String(message || '');
    var m = /^(.{1,40}?) (?:a rejoint|est arrivé|est là)/.exec(s);
    if (m) {
      var n = m[1];
      var deja = n === nom + SUFFIXE_WEB;
      presents.forEach(function (p) { if (p.nom === n) deja = true; });
      if (!deja) presents.set('nom:' + n, { nom: n, web: n.slice(-SUFFIXE_WEB.length) === SUFFIXE_WEB });
      rendrePresence();
      return;
    }
    m = /^(.{1,40}?) (?:a quitté|est parti|est partie|s’en va)/.exec(s);
    if (m) {
      var partant = m[1];
      var acles = [];
      presents.forEach(function (p, k) { if (p.nom === partant && !p.moi) acles.push(k); });
      acles.forEach(function (k) { presents.delete(k); });
      rendrePresence();
    }
  }

  function rendrePresence() {
    var zone = el.presenceListe;
    while (zone.firstChild) zone.removeChild(zone.firstChild);
    if (presents.size === 0) { zone.textContent = 'personne pour l’instant'; return; }
    var premier = true;
    presents.forEach(function (p) {
      if (!premier) zone.appendChild(document.createTextNode(', '));
      premier = false;
      var span = document.createElement('span');
      if (p.web) span.className = 'web';
      span.textContent = borner(sansSuffixe(p.nom), NOM_MAX + 8) + (p.moi ? ' (toi)' : '');
      zone.appendChild(span);
    });
  }

  // ---- La saisie ----
  function majCompteur() {
    var n = el.texte.value.length;
    el.compteurTexte.textContent = n + ' / ' + MAX_TEXTE;
    el.compteurTexte.className = 'compteur-texte' + (n > MAX_TEXTE ? ' trop' : n > MAX_TEXTE - 200 ? ' limite' : '');
    el.texte.style.height = 'auto';
    el.texte.style.height = Math.min(el.texte.scrollHeight, 160) + 'px';
  }

  el.texte.addEventListener('input', majCompteur);

  el.texte.addEventListener('keydown', function (ev) {
    if (ev.key === 'Enter' && !ev.shiftKey && !ev.isComposing) {
      ev.preventDefault();
      envoyerMessage();
    }
  });

  el.formSaisie.addEventListener('submit', function (ev) {
    ev.preventDefault();
    envoyerMessage();
  });

  function envoyerMessage() {
    var t = el.texte.value.replace(/\r\n?/g, '\n').trim();
    if (!t) return;
    if (t.length > MAX_TEXTE) {
      bandeau('Message trop long : ' + MAX_TEXTE + ' caractères au plus.', 'danger');
      return;
    }
    rechargerDebit();
    if (jetons < 1) {
      bandeau('Doucement — un message toutes les ' + (DEBIT_RECHARGE_MS / 1000) + ' secondes.', 'alerte');
      return;
    }
    if (!envoyer({ type: 'chat', text: t })) {
      bandeau('Pas de connexion pour l’instant, ton message n’est pas parti.', 'danger');
      return;
    }
    jetons -= 1;
    rendreDebit();
    el.texte.value = '';
    majCompteur();
    el.texte.focus();
  }

  function rechargerDebit() {
    var maintenant = Date.now();
    var gagne = Math.floor((maintenant - derniereRecharge) / DEBIT_RECHARGE_MS);
    if (gagne > 0) {
      jetons = Math.min(DEBIT_RAFALE, jetons + gagne);
      derniereRecharge += gagne * DEBIT_RECHARGE_MS;
      if (jetons === DEBIT_RAFALE) derniereRecharge = maintenant;
    }
  }

  function rendreDebit() {
    var points = el.debit.querySelectorAll('i');
    for (var i = 0; i < points.length; i++) {
      points[i].className = i < jetons ? '' : 'vide';
    }
    el.debit.className = 'debit' + (jetons <= 1 ? ' serre' : '');
    el.boutonEnvoyer.disabled = jetons < 1;
  }

  setInterval(function () {
    if (etat !== 'salon') return;
    var avant = jetons;
    rechargerDebit();
    if (jetons !== avant) rendreDebit();
  }, 500);

  el.bandeauFermer.addEventListener('click', function () { bandeau(''); });

  // Le clavier du téléphone pousse la page : on garde le fil en bas.
  if (window.visualViewport) {
    window.visualViewport.addEventListener('resize', function () {
      if (etat === 'salon' && presDuBas()) defilerEnBas();
    });
  }

  // =====================================================================
  // Le vocal
  // =====================================================================
  //
  // Côté micro : getUserMedia → AudioContext 48 kHz → AudioWorklet
  // « ki-capture » (trames de 960 échantillons + niveau RMS) → ici, la
  // décision d'émettre (voix ou bouton maintenu) → AudioEncoder Opus →
  // trame binaire sur la WebSocket.
  //
  // Côté écoute : trame binaire → AudioDecoder du locuteur → PCM → AudioWorklet
  // « ki-sortie », qui garde une petite file par locuteur (tampon de gigue :
  // trois trames avant de jouer, dix au plus) et mixe tout le monde → GainNode
  // (le volume) → haut-parleur.
  //
  // Le serveur ne dit rien à l'invité des membres du salon : pour nommer qui
  // parle, il envoie les occupants du vocal avec `porte_vocal` puis à chaque
  // changement avec `porte_vocal_occupants` ({ id, nom }). Sans eux, on dit
  // « quelqu'un parle ».

  var vocal = {
    supporte: null,        // null : on ne sait pas encore ; false : motif dans `note`
    note: '',
    autorise: false,       // le serveur a envoyé `porte_vocal`
    salon: '',             // le nom du salon vocal
    channel: null,
    actif: false,          // on y est (ou on s'y branche)
    ctx: null, flux: null, source: null, capture: null, sortie: null, gain: null,
    encodeur: null,
    trames: 0,             // trames capturées : l'horodatage donné à l'encodeur
    compteur: 0,           // trames encodées : le compteur du protocole
    sourdine: false,
    maintien: false,       // « maintenir pour parler » plutôt que la voix
    maintenu: false,       // le bouton est enfoncé
    ouvert: false,         // la voix a passé le seuil
    derniereVoix: 0,
    niveauDb: -100,
    seuilDb: VOCAL_SEUIL_DB,
    emet: false,           // on envoie en ce moment
    volume: 100,
    profondeur: 0,         // trames en attente dans le tampon de sortie
    rtt: null,             // aller-retour mesuré au ping, en ms
    pingEnvoye: 0,
    controleEnvoye: 0,     // quand on a dit `vocal` au serveur pour la dernière fois
    controleIgnore: false, // ce serveur ne connaît pas `vocal` : on n'insiste pas
    locuteurs: new Map(),  // id → { id, decodeur, dernier, seq, recu, pertes }
    occupants: new Map(),  // id → nom (ce que le serveur en dit)
    minuteurs: []
  };

  // ---- Est-ce que ce navigateur sait faire ? ----
  function detecterVocal() {
    var manque = [];
    if (!window.isSecureContext) manque.push('une page sécurisée (https)');
    if (!(navigator.mediaDevices && navigator.mediaDevices.getUserMedia)) manque.push('le micro');
    if (typeof AudioContext === 'undefined' || typeof AudioWorkletNode === 'undefined') manque.push('AudioWorklet');
    if (typeof AudioEncoder === 'undefined' || typeof AudioDecoder === 'undefined' ||
        typeof AudioData === 'undefined' || typeof EncodedAudioChunk === 'undefined') manque.push('WebCodecs');
    if (typeof Reflect === 'undefined' || !Reflect.construct) manque.push('Reflect');
    if (manque.length) {
      vocalIndisponible('il manque ' + manque.join(', '));
      return;
    }
    Promise.all([
      AudioEncoder.isConfigSupported(CONFIG_ENCODEUR),
      AudioDecoder.isConfigSupported(CONFIG_DECODEUR)
    ]).then(function (r) {
      if (r[0] && r[0].supported && r[1] && r[1].supported) {
        vocal.supporte = true;
        majBoutonVocal();
      } else {
        vocalIndisponible('pas d’Opus en WebCodecs');
      }
    }, function () {
      vocalIndisponible('pas d’Opus en WebCodecs');
    });
  }

  function vocalIndisponible(detail) {
    vocal.supporte = false;
    vocal.note = 'Le vocal n’est pas disponible sur ce navigateur (Safari, par exemple) : le texte marche.' +
      (detail ? ' (' + detail + ')' : '');
    majBoutonVocal();
  }

  // Le bouton « Rejoindre le vocal » et sa note, selon ce qu'on sait.
  function majBoutonVocal() {
    var b = el.boutonVocal;
    if (vocal.actif) {
      b.hidden = true;
      el.vocalNote.hidden = true;
      return;
    }
    b.hidden = false;
    b.textContent = 'Rejoindre le vocal' + (vocal.salon ? ' — ' + borner(vocal.salon, 40) : '');
    if (vocal.supporte === false) {
      b.disabled = true;
      b.title = '';
      el.vocalNote.textContent = vocal.note;
      el.vocalNote.hidden = !vocal.autorise;   // inutile de le dire tant que rien n'est ouvert
    } else if (!vocal.autorise) {
      b.disabled = true;
      b.title = 'Un membre doit d’abord te mettre dans un salon vocal';
      el.vocalNote.hidden = true;
    } else if (vocal.supporte === null) {
      b.disabled = true;
      b.title = 'Un instant…';
      el.vocalNote.hidden = true;
    } else {
      b.disabled = false;
      b.title = '';
      el.vocalNote.hidden = true;
    }
  }

  // ---- Ce que le serveur dit du vocal ----
  function vocalOuvert(msg) {
    var nouveau = !vocal.autorise;
    vocal.autorise = true;
    vocal.salon = typeof msg.nom_salon === 'string' ? msg.nom_salon : (vocal.salon || '');
    var ancien = vocal.channel;
    if (msg.channel != null) vocal.channel = msg.channel;
    // Déplacé d'un salon vocal à un autre pendant qu'on écoute : ce qui
    // reste en file vient de l'ancien.
    if (vocal.actif && ancien != null && vocal.channel !== ancien) viderEcoute();
    if (Array.isArray(msg.occupants)) vocalOccupants(msg.occupants);
    majBoutonVocal();
    if (vocal.actif) rendreVocalSalon();
    // Ouvert pour la première fois, ou rouvert alors qu'on n'y est pas
    // entré : un membre qui nous invite une seconde fois veut qu'on clique.
    if ((nouveau || !vocal.actif) && etat === 'salon') {
      ajouterLigne('info', 'Un membre t’ouvre le vocal' + (vocal.salon ? ' « ' + borner(vocal.salon, 40) + ' »' : '') +
        ' — le bouton est en bas.');
    }
  }

  function vocalFini() {
    // Déjà dehors : rien à dire — le serveur peut nous le redire, quand
    // notre `vocal` et sa sortie se sont croisés.
    if (!vocal.autorise && !vocal.actif) return;
    var etaitDedans = vocal.actif;
    vocal.autorise = false;
    vocal.salon = '';
    vocal.channel = null;
    vocal.occupants.clear();
    quitterVocal(true);
    majBoutonVocal();
    if (etat === 'salon') {
      ajouterLigne('info', etaitDedans ? 'Le vocal est terminé pour toi.' : 'Le vocal n’est plus ouvert.');
    }
  }

  function vocalOccupants(liste) {
    if (!Array.isArray(liste)) return;
    vocal.occupants.clear();
    liste.forEach(function (o) {
      if (o && o.id != null && typeof o.nom === 'string') {
        vocal.occupants.set(Number(o.id), borner(o.nom, NOM_MAX + 8));
      }
    });
    rendreVocalOccupants();
  }

  function nomLocuteur(id) {
    return vocal.occupants.get(Number(id)) || '';
  }

  function estMonNomVocal(n) {
    return n === nom + SUFFIXE_WEB || (nomAffiche && n === nomAffiche);
  }

  // ---- Rejoindre ----
  el.boutonVocal.addEventListener('click', rejoindreVocal);

  function rejoindreVocal() {
    if (vocal.actif || !vocal.autorise || !vocal.supporte) return;
    // Le contexte se crée dans le clic : c'est la règle des navigateurs
    // pour avoir le droit de sortir du son.
    var ctx;
    try {
      ctx = new AudioContext({ sampleRate: VOCAL_FREQUENCE, latencyHint: 'interactive' });
    } catch (e) {
      vocalErreur('Le son n’a pas pu démarrer : ' + (e && e.message ? e.message : e));
      return;
    }
    if (ctx.sampleRate !== VOCAL_FREQUENCE) {
      ctx.close();
      vocalErreur('Ce navigateur ne sait pas travailler à 48 kHz, ce que demande Opus.');
      return;
    }
    vocal.ctx = ctx;
    vocal.actif = true;
    vocal.trames = 0;
    // Le compteur ne doit jamais reculer aux yeux du serveur — c'est lui
    // qui en fait le nonce et il jette tout ce qui recule — ni d'un vocal
    // au suivant, ni d'un rechargement de la page qui garde sa session.
    // On repart donc de l'heure comptée en trames de 20 ms : toujours
    // devant ce qu'on a pu envoyer, et loin de 2^53.
    vocal.compteur = Math.max(vocal.compteur, Date.now() * 50);
    vocal.rtt = null;
    vocal.pingEnvoye = 0;
    majBoutonVocal();
    el.vocalControles.hidden = false;
    rendreVocalSalon();
    rendreVocalOccupants();
    rendreVocalQui([]);
    el.vocalIndicateur.textContent = 'Connexion…';
    el.vocalIndicateur.className = 'vocal-indicateur';

    ctx.audioWorklet.addModule(URL_SCRIPT).then(function () {
      if (!vocal.actif || vocal.ctx !== ctx) return null;
      return navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 }
      });
    }).then(function (flux) {
      if (!flux) return;
      if (!vocal.actif || vocal.ctx !== ctx) { arreterFlux(flux); return; }
      brancherVocal(flux);
    }).catch(function (e) {
      quitterVocal(true);
      vocalErreur(motifMicro(e));
    });
  }

  function motifMicro(e) {
    var n = e && e.name;
    if (n === 'NotAllowedError' || n === 'SecurityError') {
      return 'Le micro est refusé : autorise-le dans le navigateur, puis réessaie.';
    }
    if (n === 'NotFoundError' || n === 'OverconstrainedError') return 'Aucun micro trouvé.';
    if (n === 'NotReadableError') return 'Le micro est pris par une autre application.';
    if (n === 'AbortError') return 'Le chargement du module audio a été interrompu (politique de sécurité ?).';
    return 'Le vocal n’a pas pu démarrer : ' + (e && (e.message || e.name) ? (e.message || e.name) : 'erreur inconnue');
  }

  function vocalErreur(texte) {
    bandeau(texte, 'danger');
  }

  function arreterFlux(flux) {
    try { flux.getTracks().forEach(function (t) { t.stop(); }); } catch (e) { /* rien */ }
  }

  // Le micro est là : on monte la chaîne complète et on prévient le serveur.
  function brancherVocal(flux) {
    var ctx = vocal.ctx;
    vocal.flux = flux;
    var piste = flux.getAudioTracks()[0];
    if (piste) {
      // Micro débranché, autorisation retirée : on sort proprement.
      piste.onended = function () {
        if (vocal.actif && vocal.flux === flux) {
          quitterVocal(false);
          vocalErreur('Le micro s’est arrêté : le vocal est quitté.');
        }
      };
    }

    vocal.source = ctx.createMediaStreamSource(flux);
    vocal.capture = new AudioWorkletNode(ctx, 'ki-capture', {
      numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1],
      channelCount: 1, channelCountMode: 'explicit', channelInterpretation: 'speakers'
    });
    vocal.capture.port.onmessage = function (ev) { trameCapturee(ev.data); };
    vocal.source.connect(vocal.capture);
    // Le processeur de capture ne sort que du silence ; le brancher garde
    // le nœud vivant.
    vocal.capture.connect(ctx.destination);

    vocal.sortie = new AudioWorkletNode(ctx, 'ki-sortie', {
      numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [1]
    });
    vocal.sortie.port.onmessage = function (ev) {
      if (ev.data && ev.data.type === 'etat') vocal.profondeur = ev.data.profondeur;
    };
    vocal.gain = ctx.createGain();
    vocal.gain.gain.value = vocal.volume / 100;
    vocal.sortie.connect(vocal.gain);
    vocal.gain.connect(ctx.destination);

    vocal.encodeur = new AudioEncoder({
      output: trameEncodee,
      error: function (e) {
        if (!vocal.actif) return;
        quitterVocal(false);
        vocalErreur('L’encodeur audio a lâché : ' + (e && e.message ? e.message : e));
      }
    });
    vocal.encodeur.configure(CONFIG_ENCODEUR);

    if (ctx.state !== 'running') ctx.resume().catch(function () { /* on réessaie au geste suivant */ });

    direVocal(true);

    vocal.minuteurs.push(setInterval(rafraichirVocal, VOCAL_AFFICHAGE_MS));
    vocal.minuteurs.push(setInterval(pingVocal, VOCAL_PING_MS));
    pingVocal();
    rendreModeVocal();
    rendreIndicateur();
  }

  // ---- Quitter ----
  // `silencieux` : pas de bandeau ; la WebSocket peut déjà être fermée.
  function quitterVocal(silencieux) {
    if (!vocal.actif && !vocal.ctx) return;
    var etaitActif = vocal.actif;
    vocal.actif = false;
    if (etaitActif) direVocal(false);

    vocal.minuteurs.forEach(clearInterval);
    vocal.minuteurs = [];

    if (vocal.encodeur) { try { vocal.encodeur.close(); } catch (e) { /* déjà fermé */ } }
    vocal.encodeur = null;
    vocal.locuteurs.forEach(function (l) { fermerDecodeur(l); });
    vocal.locuteurs.clear();

    if (vocal.flux) arreterFlux(vocal.flux);
    vocal.flux = null;
    [vocal.source, vocal.capture, vocal.sortie, vocal.gain].forEach(function (n) {
      if (n) { try { n.disconnect(); } catch (e) { /* rien */ } }
    });
    if (vocal.capture) vocal.capture.port.onmessage = null;
    if (vocal.sortie) vocal.sortie.port.onmessage = null;
    vocal.source = vocal.capture = vocal.sortie = vocal.gain = null;
    if (vocal.ctx) { try { vocal.ctx.close().catch(function () {}); } catch (e) { /* rien */ } }
    vocal.ctx = null;

    vocal.emet = false;
    vocal.maintenu = false;
    vocal.ouvert = false;
    vocal.niveauDb = -100;
    vocal.profondeur = 0;
    vocal.rtt = null;
    vocal.pingEnvoye = 0;

    el.vocalControles.hidden = true;
    el.vocalParler.setAttribute('aria-pressed', 'false');
    majBoutonVocal();
    if (!silencieux && etat === 'salon') ajouterLigne('info', 'Tu as quitté le vocal.');
  }

  el.vocalQuitter.addEventListener('click', function () {
    quitterVocal(false);
    el.texte.focus();
  });

  // « J'émets et j'écoute » / « je ne suis plus là » : un mot au serveur,
  // pour qu'il sache quand pousser le son. Un serveur qui ne connaît pas
  // encore ce message répond « message invalide » : on le note une fois,
  // sans bandeau, et on n'insiste plus — la voix passe quand même, le
  // serveur la route d'après le salon vocal où un membre nous a mis.
  function direVocal(actif) {
    if (vocal.controleIgnore) return;
    if (envoyer({ type: 'vocal', actif: !!actif })) vocal.controleEnvoye = Date.now();
  }

  function erreurDeControleVocal(message) {
    if (!vocal.controleEnvoye || Date.now() - vocal.controleEnvoye > 3000) return false;
    if (!/message invalide|inconnu/i.test(String(message || ''))) return false;
    vocal.controleIgnore = true;
    vocal.controleEnvoye = 0;
    return true;
  }

  // Tout ce qui attend d'être joué est jeté : décodeurs et files de sortie.
  // À la reprise d'une coupure, ou quand on change de salon vocal.
  function viderEcoute() {
    vocal.locuteurs.forEach(function (l) {
      if (l.decodeur) { try { l.decodeur.close(); } catch (e) { /* déjà fermé */ } }
      l.decodeur = null;
    });
    vocal.locuteurs.clear();
    if (vocal.sortie) vocal.sortie.port.postMessage({ type: 'vider' });
    vocal.profondeur = 0;
  }

  // Onglet fermé ou mis en veille : le micro est rendu.
  window.addEventListener('pagehide', function () { quitterVocal(true); });

  // ---- Le micro : une trame toutes les 20 ms ----
  function trameCapturee(m) {
    if (!vocal.actif || !m || !m.pcm) return;
    var pcm = m.pcm;
    vocal.trames += 1;
    var db = m.rms > 0 ? 20 * Math.log(m.rms) / Math.LN10 : -100;
    vocal.niveauDb = db;

    // Activation à la voix : on ouvre dès que le niveau passe le seuil, on
    // referme après un temps de maintien sous le seuil — pour ne pas
    // hacher les fins de phrase.
    var maintenant = Date.now();
    if (db >= vocal.seuilDb) {
      vocal.ouvert = true;
      vocal.derniereVoix = maintenant;
    } else if (vocal.ouvert && maintenant - vocal.derniereVoix > VOCAL_MAINTIEN_MS) {
      vocal.ouvert = false;
    }

    var emet = !vocal.sourdine && (vocal.maintien ? vocal.maintenu : vocal.ouvert);
    if (emet !== vocal.emet) {
      vocal.emet = emet;
      rendreIndicateur();
    }
    if (!emet || !vocal.encodeur || vocal.encodeur.state !== 'configured') return;

    var donnees;
    try {
      donnees = new AudioData({
        format: 'f32', sampleRate: VOCAL_FREQUENCE, numberOfFrames: pcm.length, numberOfChannels: 1,
        timestamp: vocal.trames * VOCAL_TRAME_US, data: pcm
      });
    } catch (e) { return; }
    try { vocal.encodeur.encode(donnees); } catch (e) { /* l'erreur arrive par le rappel */ }
    donnees.close();
  }

  // Une trame Opus sortie de l'encodeur : en-tête, puis sur le fil.
  function trameEncodee(chunk) {
    if (!vocal.actif) return;
    var n = chunk.byteLength;
    var tampon = new ArrayBuffer(VOCAL_ENTETE_MONTANT + n);
    var vue = new DataView(tampon);
    vue.setUint8(0, VOCAL_VERSION);
    ecrireU64(vue, 1, vocal.compteur);
    vocal.compteur += 1;
    chunk.copyTo(new Uint8Array(tampon, VOCAL_ENTETE_MONTANT));
    if (!ws || ws.readyState !== 1) return;   // coupure : la trame est perdue, le compteur avance
    try { ws.send(tampon); } catch (e) { /* la fermeture suit */ }
  }

  // Un u64 petit-boutiste en deux u32 : les compteurs tiennent dans les
  // 2^53 d'un nombre JavaScript, et les identifiants d'invités (≥ 2^62), que
  // le serveur espace de 2^10 (INVITE_ID_PAS) pour cela, s'y écrivent
  // exactement — ici comme dans le JSON des occupants, les deux se
  // retrouvent.
  function ecrireU64(vue, position, n) {
    vue.setUint32(position, n % 4294967296, true);
    vue.setUint32(position + 4, Math.floor(n / 4294967296), true);
  }
  function lireU64(vue, position) {
    return vue.getUint32(position, true) + vue.getUint32(position + 4, true) * 4294967296;
  }

  // ---- L'écoute : une trame d'un locuteur ----
  function trameVocaleRecue(tampon) {
    if (!vocal.actif || !vocal.sortie || tampon.byteLength <= VOCAL_ENTETE_DESCENDANT) return;
    var vue = new DataView(tampon);
    if (vue.getUint8(0) !== VOCAL_VERSION) return;
    var id = lireU64(vue, 1);
    var seq = lireU64(vue, 9);
    var l = vocal.locuteurs.get(id);
    if (!l) {
      if (vocal.locuteurs.size >= VOCAL_LOCUTEURS_MAX) return;
      l = { id: id, decodeur: null, dernier: 0, seq: -1, recu: 0, pertes: 0 };
      vocal.locuteurs.set(id, l);
    }
    if (seq === l.seq) return;                       // rejouée
    if (seq > l.seq + 1 && l.seq >= 0) l.pertes += seq - l.seq - 1;
    l.seq = seq;
    l.dernier = Date.now();
    if (!l.decodeur || l.decodeur.state !== 'configured') ouvrirDecodeur(l);
    var chunk;
    try {
      chunk = new EncodedAudioChunk({
        type: 'key', timestamp: l.recu * VOCAL_TRAME_US, data: new Uint8Array(tampon, VOCAL_ENTETE_DESCENDANT)
      });
    } catch (e) { return; }
    l.recu += 1;
    try { l.decodeur.decode(chunk); } catch (e) { fermerDecodeur(l); }
  }

  function ouvrirDecodeur(l) {
    fermerDecodeur(l);
    var d = new AudioDecoder({
      output: function (donnees) { livrerPcm(l, donnees); },
      error: function () { l.decodeur = null; }   // refait à la prochaine trame
    });
    try { d.configure(CONFIG_DECODEUR); } catch (e) { return; }
    l.decodeur = d;
  }

  function fermerDecodeur(l) {
    if (l.decodeur) { try { l.decodeur.close(); } catch (e) { /* déjà fermé */ } }
    l.decodeur = null;
    if (vocal.sortie) vocal.sortie.port.postMessage({ type: 'retirer', id: l.id });
  }

  // Le PCM décodé part vers le fil audio, qui le met en file pour ce locuteur.
  function livrerPcm(l, donnees) {
    var pcm = null;
    try {
      var format = String(donnees.format || '');
      var octets = donnees.allocationSize({ planeIndex: 0 });
      if (format.indexOf('f32') === 0) {
        pcm = new Float32Array(octets / 4);
        donnees.copyTo(pcm, { planeIndex: 0 });
      } else if (format.indexOf('s16') === 0) {
        var court = new Int16Array(octets / 2);
        donnees.copyTo(court, { planeIndex: 0 });
        pcm = new Float32Array(court.length);
        for (var i = 0; i < court.length; i++) pcm[i] = court[i] / 32768;
      }
    } catch (e) { pcm = null; }
    donnees.close();
    if (!pcm || !vocal.sortie || !vocal.actif) return;
    vocal.sortie.port.postMessage({ type: 'trame', id: l.id, pcm: pcm }, [pcm.buffer]);
  }

  // ---- Ce qui s'affiche, six fois par seconde ----
  function rafraichirVocal() {
    if (!vocal.actif) return;
    var maintenant = Date.now();
    var parlent = [];
    var oublies = [];
    vocal.locuteurs.forEach(function (l) {
      if (maintenant - l.dernier < VOCAL_PARLE_MS) parlent.push(l.id);
      else if (maintenant - l.dernier > VOCAL_OUBLI_MS) oublies.push(l);
    });
    oublies.forEach(function (l) { fermerDecodeur(l); vocal.locuteurs.delete(l.id); });
    rendreVocalQui(parlent);
    rendreNiveau();
    rendreLatence();
  }

  function rendreVocalSalon() {
    el.vocalSalon.textContent = vocal.salon ? borner(vocal.salon, 40) : 'vocal';
  }

  function rendreVocalOccupants() {
    var noms = [];
    vocal.occupants.forEach(function (n) { noms.push(estMonNomVocal(n) ? 'toi' : sansSuffixe(n)); });
    el.vocalOccupants.textContent = noms.length
      ? 'Dans le vocal : ' + noms.join(', ')
      : 'Dans le vocal : le serveur ne l’a pas dit.';
  }

  function rendreVocalQui(ids) {
    var noms = [];
    var anonymes = 0;
    ids.forEach(function (id) {
      var n = nomLocuteur(id);
      if (n) noms.push(sansSuffixe(n)); else anonymes += 1;
    });
    if (anonymes) noms.push(anonymes === 1 ? 'quelqu’un' : anonymes + ' personnes');
    var texte;
    if (noms.length === 0) texte = 'Personne ne parle.';
    else if (noms.length === 1) texte = noms[0] + ' parle';
    else texte = noms.slice(0, -1).join(', ') + ' et ' + noms[noms.length - 1] + ' parlent';
    if (el.vocalQui.textContent !== texte) el.vocalQui.textContent = texte;
    el.vocalQui.className = 'vocal-qui' + (ids.length ? ' parle' : '');
  }

  function rendreIndicateur() {
    var i = el.vocalIndicateur;
    if (vocal.sourdine) { i.textContent = 'Micro coupé'; i.className = 'vocal-indicateur coupe'; }
    else if (vocal.emet) { i.textContent = 'Tu parles'; i.className = 'vocal-indicateur parle'; }
    else { i.textContent = vocal.maintien ? 'Maintiens le bouton pour parler' : 'Silence'; i.className = 'vocal-indicateur'; }
    el.vocalParler.setAttribute('aria-pressed', vocal.maintenu ? 'true' : 'false');
  }

  // Le niveau du micro entre −70 et −10 dB, et le seuil posé dessus.
  function pourcentDb(db) {
    return Math.max(0, Math.min(100, (db + 70) / 60 * 100));
  }
  function rendreNiveau() {
    // Une transformation plutôt qu'une largeur : dix fois par seconde,
    // sans recalcul de la mise en page (voir porte.css).
    el.vocalNiveauBarre.style.transform = 'scaleX(' + (pourcentDb(vocal.niveauDb) / 100).toFixed(3) + ')';
    el.vocalNiveauBarre.className = vocal.emet ? 'ouvert' : '';
    el.vocalNiveauSeuil.style.left = pourcentDb(vocal.seuilDb) + '%';
    el.vocalNiveauSeuil.hidden = vocal.maintien;
  }

  // La latence qu'on peut estimer : la trame elle-même, ce qui attend dans
  // le tampon de gigue, la sortie audio du navigateur et la moitié de
  // l'aller-retour mesuré au ping. Sans le décodage ni ce que fait le
  // serveur — d'où le « ≈ ».
  function rendreLatence() {
    var ctx = vocal.ctx;
    var ms = VOCAL_TRAME_US / 1000 + vocal.profondeur * (VOCAL_TRAME_US / 1000);
    if (ctx) ms += ((ctx.outputLatency || 0) + (ctx.baseLatency || 0)) * 1000;
    if (vocal.rtt != null) ms += vocal.rtt / 2;
    el.vocalLatence.textContent = '≈ ' + Math.round(ms) + ' ms' + (vocal.rtt == null ? ' + réseau' : '');
  }

  function pingVocal() {
    if (!vocal.actif) return;
    if (envoyer({ type: 'ping' })) vocal.pingEnvoye = performance.now();
  }

  // ---- Les commandes ----
  el.vocalSourdine.addEventListener('click', function () {
    vocal.sourdine = !vocal.sourdine;
    el.vocalSourdine.setAttribute('aria-pressed', vocal.sourdine ? 'true' : 'false');
    el.vocalSourdine.textContent = vocal.sourdine ? 'Rendre le micro' : 'Sourdine';
    rendreIndicateur();
  });

  el.vocalVolume.addEventListener('input', function () {
    vocal.volume = Number(el.vocalVolume.value) || 0;
    if (vocal.gain) vocal.gain.gain.value = vocal.volume / 100;
    memoriser('ki-porte-vocal-volume', String(vocal.volume));
  });

  el.vocalSeuil.addEventListener('input', function () {
    vocal.seuilDb = Number(el.vocalSeuil.value) || VOCAL_SEUIL_DB;
    memoriser('ki-porte-vocal-seuil', String(vocal.seuilDb));
    rendreNiveau();
  });

  // Voix ou bouton : sur un écran tactile, le bouton d'office (le bruit
  // ambiant d'un téléphone se prête mal à la détection) ; on peut changer.
  el.vocalMode.addEventListener('click', function () {
    vocal.maintien = !vocal.maintien;
    vocal.maintenu = false;
    memoriser('ki-porte-vocal-mode', vocal.maintien ? 'maintien' : 'voix');
    rendreModeVocal();
    rendreIndicateur();
  });

  function rendreModeVocal() {
    el.vocalMode.textContent = vocal.maintien ? 'Mode : bouton' : 'Mode : voix';
    el.vocalMode.title = vocal.maintien
      ? 'Tu parles en maintenant le bouton — clique pour passer à la voix'
      : 'Tu parles dès que ta voix passe le seuil — clique pour passer au bouton';
    el.vocalParler.hidden = !vocal.maintien;
    el.vocalSeuilLigne.hidden = vocal.maintien;
    rendreNiveau();
  }

  // « Maintenir pour parler » : doigt, souris ou clavier (Espace, Entrée).
  function maintenir(oui) {
    if (vocal.maintenu === oui) return;
    vocal.maintenu = oui;
    el.vocalParler.setAttribute('aria-pressed', oui ? 'true' : 'false');
  }
  el.vocalParler.addEventListener('pointerdown', function (ev) {
    ev.preventDefault();
    try { el.vocalParler.setPointerCapture(ev.pointerId); } catch (e) { /* rien */ }
    maintenir(true);
  });
  ['pointerup', 'pointercancel', 'lostpointercapture'].forEach(function (nomEv) {
    el.vocalParler.addEventListener(nomEv, function () { maintenir(false); });
  });
  el.vocalParler.addEventListener('contextmenu', function (ev) { ev.preventDefault(); });
  el.vocalParler.addEventListener('keydown', function (ev) {
    if ((ev.key === ' ' || ev.key === 'Enter') && !ev.repeat) { ev.preventDefault(); maintenir(true); }
  });
  el.vocalParler.addEventListener('keyup', function (ev) {
    if (ev.key === ' ' || ev.key === 'Enter') { ev.preventDefault(); maintenir(false); }
  });
  el.vocalParler.addEventListener('blur', function () { maintenir(false); });
  window.addEventListener('blur', function () { maintenir(false); });

  // ---- Les deux processeurs du fil audio ----
  // Exécuté seulement quand ce fichier est chargé par audioWorklet.addModule.
  // Syntaxe ES5 : les processeurs héritent d'AudioWorkletProcessor par
  // Reflect.construct, comme le ferait une classe.
  function definirProcesseurs() {
    var TRAME = 960;        // 20 ms à 48 kHz
    var CIBLE = 3;          // trames en réserve avant de jouer un locuteur
    var MAXI = 10;          // au-delà, on rattrape en sautant les plus vieilles

    // « ki-capture » : regroupe l'entrée en trames de 960 échantillons et
    // les envoie à la page avec leur niveau RMS. Sortie : silence.
    function Capture() {
      var moi = Reflect.construct(AudioWorkletProcessor, [], Capture);
      moi.tampon = new Float32Array(TRAME);
      moi.rempli = 0;
      return moi;
    }
    Capture.prototype = Object.create(AudioWorkletProcessor.prototype);
    Capture.prototype.constructor = Capture;
    Capture.prototype.process = function (entrees) {
      var canal = entrees[0] && entrees[0][0];
      if (!canal || !canal.length) return true;
      var t = this.tampon;
      for (var i = 0; i < canal.length; i++) {
        t[this.rempli++] = canal[i];
        if (this.rempli === TRAME) {
          var somme = 0;
          for (var j = 0; j < TRAME; j++) somme += t[j] * t[j];
          this.port.postMessage({ pcm: t, rms: Math.sqrt(somme / TRAME) }, [t.buffer]);
          this.tampon = t = new Float32Array(TRAME);
          this.rempli = 0;
        }
      }
      return true;
    };
    registerProcessor('ki-capture', Capture);

    // « ki-sortie » : une file de trames PCM par locuteur, un tampon de
    // gigue (on attend CIBLE trames avant de commencer à lire, on lâche
    // les plus vieilles au-delà de MAXI), et la somme de tous, bornée.
    function Sortie() {
      var moi = Reflect.construct(AudioWorkletProcessor, [], Sortie);
      moi.locuteurs = new Map();   // id → { file: [Float32Array], lu, amorce }
      moi.dernierEtat = 0;
      moi.port.onmessage = function (ev) { moi.recevoir(ev.data); };
      return moi;
    }
    Sortie.prototype = Object.create(AudioWorkletProcessor.prototype);
    Sortie.prototype.constructor = Sortie;
    Sortie.prototype.recevoir = function (m) {
      if (!m) return;
      if (m.type === 'trame') {
        var l = this.locuteurs.get(m.id);
        if (!l) { l = { file: [], lu: 0, amorce: true }; this.locuteurs.set(m.id, l); }
        l.file.push(m.pcm);
        while (l.file.length > MAXI) { l.file.shift(); l.lu = 0; }
      } else if (m.type === 'retirer') {
        this.locuteurs.delete(m.id);
      } else if (m.type === 'vider') {
        this.locuteurs.clear();
      }
    };
    Sortie.prototype.process = function (entrees, sorties) {
      var sortie = sorties[0] && sorties[0][0];
      if (!sortie) return true;
      sortie.fill(0);
      var profondeur = 0;
      this.locuteurs.forEach(function (l) {
        if (l.amorce) {
          if (l.file.length < CIBLE) return;
          l.amorce = false;
        }
        if (l.file.length > profondeur) profondeur = l.file.length;
        for (var i = 0; i < sortie.length; i++) {
          if (l.file.length === 0) { l.amorce = true; break; }   // à sec : on réamorce
          var bloc = l.file[0];
          sortie[i] += bloc[l.lu++];
          if (l.lu >= bloc.length) { l.file.shift(); l.lu = 0; }
        }
      });
      for (var k = 0; k < sortie.length; k++) {
        if (sortie[k] > 1) sortie[k] = 1; else if (sortie[k] < -1) sortie[k] = -1;
      }
      if (currentTime - this.dernierEtat > 0.5) {
        this.dernierEtat = currentTime;
        this.port.postMessage({ type: 'etat', profondeur: profondeur });
      }
      return true;
    };
    registerProcessor('ki-sortie', Sortie);
  }

  // ---- La carte « Installe ki-chat » (e) ----
  function afficherInvitation(msg) {
    el.invitationCode.textContent = borner(msg.code, 64);
    el.invitationServeur.textContent = borner(msg.serveur, 128);
    var lien = String(msg.telechargement || '');
    // Seul un lien https vers GitHub est suivi ; sinon la page des releases.
    if (!/^https:\/\/github\.com\//.test(lien)) lien = 'https://github.com/Redik123/ki-chat/releases/latest';
    el.invitationTelechargement.href = lien;
    el.carteInvitation.hidden = false;
    if (etat === 'salon') {
      ajouterLigne('info', 'Un membre t’invite à installer ki-chat — voir la carte.');
    }
    el.invitationReplier.focus();
  }

  el.invitationReplier.addEventListener('click', function () {
    el.carteInvitation.hidden = true;
    if (etat === 'salon') el.texte.focus();
  });

  Array.prototype.forEach.call(document.querySelectorAll('button[data-copie]'), function (b) {
    b.addEventListener('click', function () {
      var source = $(b.dataset.copie);
      var texte = source ? source.textContent : '';
      var fini = function (ok) {
        var avant = b.textContent;
        b.textContent = ok ? 'Copié' : 'Sélectionne et copie';
        setTimeout(function () { b.textContent = avant; }, 1500);
      };
      if (navigator.clipboard && navigator.clipboard.writeText) {
        navigator.clipboard.writeText(texte).then(function () { fini(true); }, function () { fini(false); });
      } else {
        fini(false);
      }
    });
  });

  // ---- Départ ----
  var rappel = rappeler('ki-porte-nom-' + slug);
  if (rappel) {
    el.prenom.value = rappel;
    el.apercuNom.textContent = rappel;
  }
  // Les réglages du vocal, gardés d'une page à l'autre dans l'onglet.
  var modeRappele = rappeler('ki-porte-vocal-mode');
  vocal.maintien = modeRappele ? modeRappele === 'maintien'
    : !!(window.matchMedia && window.matchMedia('(pointer: coarse)').matches);
  var seuilRappele = Number(rappeler('ki-porte-vocal-seuil'));
  if (seuilRappele && seuilRappele >= -70 && seuilRappele <= -15) vocal.seuilDb = seuilRappele;
  el.vocalSeuil.value = String(vocal.seuilDb);
  var volumeRappele = rappeler('ki-porte-vocal-volume');
  if (volumeRappele !== '' && Number(volumeRappele) >= 0 && Number(volumeRappele) <= 100) vocal.volume = Number(volumeRappele);
  el.vocalVolume.value = String(vocal.volume);
  rendreModeVocal();
  majCompteur();
  rendreDebit();
  majBoutonVocal();
  detecterVocal();
  montrer('nom');
})();
