package `fun`.baws.kichat

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.webkit.WebView
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Pas de « bord à bord » : les WebView anciennes ne donnent pas la place
    // des barres système (safe-area), et la page passait dessous.
    super.onCreate(savedInstanceState)
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

  /** La page reçoit `window.KiAndroid` : service vocal, secrets, notifications. */
  override fun onWebViewCreate(webView: WebView) {
    webView.addJavascriptInterface(KiAndroid(this), "KiAndroid")
  }
}
