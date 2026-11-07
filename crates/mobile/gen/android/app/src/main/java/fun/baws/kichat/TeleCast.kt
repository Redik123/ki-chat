package `fun`.baws.kichat

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.View
import android.view.ViewGroup
import androidx.appcompat.app.AppCompatActivity
import androidx.mediarouter.app.MediaRouteButton
import com.google.android.gms.cast.CastMediaControlIntent
import com.google.android.gms.cast.MediaInfo
import com.google.android.gms.cast.MediaLoadRequestData
import com.google.android.gms.cast.MediaMetadata
import com.google.android.gms.cast.MediaStatus
import com.google.android.gms.cast.framework.CastButtonFactory
import com.google.android.gms.cast.framework.CastContext
import com.google.android.gms.cast.framework.CastOptions
import com.google.android.gms.cast.framework.CastSession
import com.google.android.gms.cast.framework.OptionsProvider
import com.google.android.gms.cast.framework.SessionManagerListener
import com.google.android.gms.cast.framework.SessionProvider
import com.google.android.gms.cast.framework.media.RemoteMediaClient
import com.google.android.gms.common.ConnectionResult
import com.google.android.gms.common.GoogleApiAvailability
import java.net.Inet4Address

/**
 * Ce que le SDK Cast lit au démarrage (voir le manifeste) : le lecteur standard de
 * Google, sans rien à enregistrer ni à héberger.
 */
class ReglagesCast : OptionsProvider {
  override fun getCastOptions(context: Context): CastOptions =
    CastOptions.Builder()
      .setReceiverApplicationId(CastMediaControlIntent.DEFAULT_MEDIA_RECEIVER_APPLICATION_ID)
      .build()

  override fun getAdditionalSessionProviders(context: Context): List<SessionProvider>? = null
}

/**
 * Le stream sur un Chromecast, le téléphone restant libre pour écrire.
 *
 * Le téléphone ne décode rien : crates/mobile/src/tele.rs sert le stream en MP4 sur le
 * réseau local, et ce qui est ici se contente de choisir la télé et de lui donner
 * l'adresse. Si le stream change de résolution ou que le lecteur tombe, on la lui redonne.
 */
class TeleCast(
  private val activite: AppCompatActivity,
  private val quandPret: (String) -> Unit,
  private val quandFini: (String) -> Unit,
) {
  private val boucle = Handler(Looper.getMainLooper())
  private var contexte: CastContext? = null
  private var bouton: MediaRouteButton? = null
  private var client: RemoteMediaClient? = null

  /** Le stream voulu : posé dès la demande, avant même d'avoir choisi la télé. */
  private var nom: String? = null
  private var enCours = false
  private var adresse: String? = null
  private var generation = 0
  private var essais = 0

  private val ecouteur = object : SessionManagerListener<CastSession> {
    override fun onSessionStarting(session: CastSession) {}
    override fun onSessionStarted(session: CastSession, id: String) = demarrer(session)
    override fun onSessionStartFailed(session: CastSession, erreur: Int) {
      Log.w(TAG, "connexion à la télé refusée : $erreur")
      if (nom != null) {
        nom = null
        quandFini("Connexion à la télé impossible")
      }
    }
    override fun onSessionEnding(session: CastSession) {}
    override fun onSessionEnded(session: CastSession, erreur: Int) = finie("La diffusion sur la télé est terminée")
    override fun onSessionResuming(session: CastSession, id: String) {}
    override fun onSessionResumed(session: CastSession, suspendue: Boolean) = demarrer(session)
    override fun onSessionResumeFailed(session: CastSession, erreur: Int) {}
    override fun onSessionSuspended(session: CastSession, raison: Int) {}
  }

  /** Le lecteur s'arrête seul : le stream est coupé, ou il n'a pas su le lire. */
  private val suiviDuLecteur = object : RemoteMediaClient.Callback() {
    override fun onStatusUpdated() {
      val c = client ?: return
      when (c.playerState) {
        MediaStatus.PLAYER_STATE_PLAYING -> essais = 0
        MediaStatus.PLAYER_STATE_IDLE -> {
          val raison = c.idleReason
          if (enCours && (raison == MediaStatus.IDLE_REASON_ERROR || raison == MediaStatus.IDLE_REASON_FINISHED)) {
            Log.w(TAG, "le lecteur s'est arrêté (raison $raison), essai $essais")
            if (++essais > ESSAIS_MAX) {
              finie("La télé n'arrive pas à lire ce stream")
              contexte?.sessionManager?.endCurrentSession(true)
            } else {
              boucle.postDelayed({ charger() }, 1000)
            }
          }
        }
      }
    }
  }

  /** Le stream change de résolution : un MP4 ne le permet pas sans recharger. */
  private val surveillance = object : Runnable {
    override fun run() {
      if (!enCours) return
      val g = KiVideo.teleGeneration()
      if (g != generation) {
        generation = g
        charger()
      }
      boucle.postDelayed(this, 1000)
    }
  }

  /** Le bouton officiel, invisible : c'est lui qui ouvre le choix de la télé. */
  private fun preparer(): Boolean {
    if (contexte != null) return true
    val c = try {
      CastContext.getSharedInstance(activite)
    } catch (e: RuntimeException) {
      Log.w(TAG, "Cast indisponible", e)
      return false
    }
    contexte = c
    c.sessionManager.addSessionManagerListener(ecouteur, CastSession::class.java)
    val b = MediaRouteButton(activite).apply {
      alpha = 0f
      setAlwaysVisible(true)
    }
    CastButtonFactory.setUpMediaRouteButton(activite.applicationContext, b)
    activite.findViewById<ViewGroup>(android.R.id.content).addView(b, ViewGroup.LayoutParams(1, 1))
    bouton = b
    return true
  }

  /** La télé déjà choisie, ou le choix du système ; `demarrer` vient ensuite. */
  fun ouvrir(nom: String) {
    if (!preparer()) {
      quandFini("Le Cast n'est pas disponible sur ce téléphone")
      return
    }
    this.nom = nom
    val session = contexte?.sessionManager?.currentCastSession
    if (session?.isConnected == true) {
      demarrer(session)
    } else {
      bouton?.post { bouton?.performClick() }
    }
  }

  private fun demarrer(session: CastSession) {
    val voulu = nom ?: return
    val fin = KiVideo.teleDemarrer()
    val ip = adresseLocale()
    if (fin == null || ip == null) {
      KiVideo.teleArreter()
      nom = null
      quandFini(if (ip == null) "Il faut être en Wi-Fi pour envoyer le stream à la télé" else "Impossible de servir le stream à la télé")
      return
    }
    // Le côté Rust est prévenu d'abord : quand la vue du téléphone se ferme, il ne
    // doit pas prendre ça pour une appli en arrière-plan et arrêter de regarder.
    adresse = "http://$ip:$fin"
    enCours = true
    essais = 0
    generation = KiVideo.teleGeneration()
    client?.unregisterCallback(suiviDuLecteur)
    client = session.remoteMediaClient
    client?.registerCallback(suiviDuLecteur)
    quandPret(voulu)
    charger()
    boucle.removeCallbacks(surveillance)
    boucle.post(surveillance)
  }

  private fun charger() {
    if (!enCours) return
    val c = client
    val url = adresse
    if (c == null || url == null) return
    val titre = MediaMetadata(MediaMetadata.MEDIA_TYPE_MOVIE).apply { putString(MediaMetadata.KEY_TITLE, nom.orEmpty()) }
    val media = MediaInfo.Builder(url)
      .setStreamType(MediaInfo.STREAM_TYPE_LIVE)
      .setContentType("video/mp4")
      .setMetadata(titre)
      .build()
    c.load(MediaLoadRequestData.Builder().setMediaInfo(media).setAutoplay(true).build())
      .setResultCallback { r ->
        if (!r.status.isSuccess) Log.w(TAG, "la télé refuse le stream : ${r.status}")
      }
  }

  /** La session s'est terminée sans qu'on le demande : la télé, ou la notification. */
  private fun finie(raison: String) {
    if (!enCours) return
    arreter()
    quandFini(raison)
  }

  private fun arreter() {
    enCours = false
    nom = null
    adresse = null
    boucle.removeCallbacks(surveillance)
    client?.unregisterCallback(suiviDuLecteur)
    client = null
    KiVideo.teleArreter()
  }

  /** On arrête nous-mêmes : la télé revient à son écran d'accueil. */
  fun fermer() {
    nom = null
    if (!enCours) return
    arreter()
    contexte?.sessionManager?.endCurrentSession(true)
  }

  /** L'adresse du téléphone sur le Wi-Fi : c'est là que la télé viendra lire. */
  private fun adresseLocale(): String? {
    val reseaux = activite.getSystemService(ConnectivityManager::class.java) ?: return null
    @Suppress("DEPRECATION")
    val wifi = reseaux.allNetworks.firstOrNull {
      reseaux.getNetworkCapabilities(it)?.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) == true
    } ?: return null
    return reseaux.getLinkProperties(wifi)?.linkAddresses
      ?.map { it.address }
      ?.firstOrNull { it is Inet4Address && !it.isLoopbackAddress }
      ?.hostAddress
  }

  companion object {
    private const val TAG = "ki-chat"
    private const val ESSAIS_MAX = 3

    /** Sans les services Google (certains téléphones), pas de Cast : pas de bouton. */
    fun disponible(contexte: Context) =
      GoogleApiAvailability.getInstance().isGooglePlayServicesAvailable(contexte) == ConnectionResult.SUCCESS
  }
}
