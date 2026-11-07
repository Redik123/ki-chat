package `fun`.baws.kichat

import android.Manifest
import android.app.Activity
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.ViewGroup
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  private var page: WebView? = null
  private var densite = 0
  private var qualiteVoulue = ""

  /** La fenêtre système du partage d'écran : à enregistrer dès la création. */
  private val demandeEcran = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
    val donnees = r.data
    if (r.resultCode == Activity.RESULT_OK && donnees != null) {
      DiffusionService.demarrer(this, r.resultCode, donnees, qualiteVoulue)
    }
  }

  private val regard: RegardVue by lazy {
    RegardVue(
      this,
      quandFerme = {
        retourDuRegard.isEnabled = false
        EcranExterne.agir("fermer_regard")
      },
      quandTele = if (TeleCast.disponible(this)) { nom -> regarderSurTele(nom) } else null,
    )
  }

  /** Le stream sur la télé : le téléphone n'affiche plus que ki-chat. */
  private val tele: TeleCast by lazy {
    TeleCast(
      this,
      quandPret = { nom ->
        regard.fermer(prevenir = false)
        retourDuRegard.isEnabled = false
        EcranExterne.agirAvec("tele_pret", nom)
      },
      quandFini = { raison -> EcranExterne.agirAvec("tele_fin", raison) },
    )
  }

  /** Ce qui attend la réponse à la demande d'accès au réseau local. */
  private var apresReseauLocal: (() -> Unit)? = null
  private val demandeReseauLocal = registerForActivityResult(ActivityResultContracts.RequestPermission()) { oui ->
    val suite = apresReseauLocal
    apresReseauLocal = null
    if (oui) {
      suite?.invoke()
    } else {
      EcranExterne.direALaPage("Sans l'accès au réseau local, ki-chat ne peut pas envoyer le stream à la télé")
    }
  }

  /** « Retour » referme le stream, sans quitter ki-chat. */
  private val retourDuRegard: OnBackPressedCallback = object : OnBackPressedCallback(false) {
    override fun handleOnBackPressed() {
      regard.fermer(prevenir = true)
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    // Pas de « bord à bord » : les WebView anciennes ne donnent pas la place
    // des barres système (safe-area), et la page passait dessous.
    super.onCreate(savedInstanceState)
    densite = resources.configuration.densityDpi
    onBackPressedDispatcher.addCallback(this, retourDuRegard)
    VocalService.canaux(this)
    // Le micro (pour le vocal) et, depuis Android 13, le droit de notifier :
    // demandés ensemble au premier lancement.
    val manquantes = buildList {
      add(Manifest.permission.RECORD_AUDIO)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) add(Manifest.permission.POST_NOTIFICATIONS)
    }.filter { ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED }
    if (manquantes.isNotEmpty()) {
      ActivityCompat.requestPermissions(this, manquantes.toTypedArray(), 1)
    }
  }

  /**
   * La page reçoit `window.KiAndroid` : service vocal, secrets, notifications.
   * Les boutons de l'écran externe et de la notification lui parlent.
   */
  override fun onWebViewCreate(webView: WebView) {
    webView.addJavascriptInterface(KiAndroid(this), "KiAndroid")
    EcranExterne.pagePrete(webView)
    page = webView
  }

  /** La page demande à diffuser : la fenêtre système, puis `demandeEcran`. */
  fun diffuser(qualite: String) = runOnUiThread {
    qualiteVoulue = qualite
    val gestion = getSystemService(MediaProjectionManager::class.java)
    demandeEcran.launch(gestion.createScreenCaptureIntent())
  }

  /**
   * Envoie le stream sur la télé. Depuis Android 17, la télé qui vient chercher le
   * flux sur le téléphone passe par une permission : on la demande à ce moment-là.
   */
  fun regarderSurTele(nom: String) = runOnUiThread {
    val permission = "android.permission.ACCESS_LOCAL_NETWORK"
    val manque = Build.VERSION.SDK_INT >= 37 &&
      ContextCompat.checkSelfPermission(this, permission) != PackageManager.PERMISSION_GRANTED
    if (manque) {
      apresReseauLocal = { tele.ouvrir(nom) }
      demandeReseauLocal.launch(permission)
    } else {
      tele.ouvrir(nom)
    }
  }

  fun ouvrirRegard(nom: String) = runOnUiThread {
    tele.fermer()
    regard.ouvrir(nom)
    retourDuRegard.isEnabled = true
  }

  /** Le stream s'arrête de lui-même : la page le sait déjà. */
  fun fermerRegard() = runOnUiThread {
    tele.fermer()
    regard.fermer(prevenir = false)
    retourDuRegard.isEnabled = false
  }

  /**
   * L'activité passe sur l'écran externe d'un Flip, ou en revient : la
   * densité change sans recréer l'activité (voir « density » dans le
   * manifeste), et la page garde sa session et son vocal. Mais le WebView
   * dessinait encore à l'échelle de l'écran d'avant : page agrandie, coupée
   * à droite. Le détacher puis le rattacher lui fait relire la sienne.
   */
  override fun onConfigurationChanged(newConfig: Configuration) {
    super.onConfigurationChanged(newConfig)
    if (newConfig.densityDpi == densite) return
    Log.i("ki-chat", "densité $densite → ${newConfig.densityDpi} : page rattachée")
    densite = newConfig.densityDpi
    val webView = page ?: return
    val parent = webView.parent as? ViewGroup ?: return
    val place = parent.indexOfChild(webView)
    val mise = webView.layoutParams
    parent.removeViewAt(place)
    parent.addView(webView, place, mise)
  }

  override fun onDestroy() {
    EcranExterne.pagePartie(this)
    super.onDestroy()
  }
}
