package `fun`.baws.kichat

import android.Manifest
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.os.Build
import android.os.Bundle
import android.util.Log
import android.view.ViewGroup
import android.webkit.WebView
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  private var page: WebView? = null
  private var densite = 0

  override fun onCreate(savedInstanceState: Bundle?) {
    // Pas de « bord à bord » : les WebView anciennes ne donnent pas la place
    // des barres système (safe-area), et la page passait dessous.
    super.onCreate(savedInstanceState)
    densite = resources.configuration.densityDpi
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
