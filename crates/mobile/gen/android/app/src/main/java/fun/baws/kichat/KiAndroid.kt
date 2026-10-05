package `fun`.baws.kichat

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.JavascriptInterface
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat

/**
 * Ce que la page demande à Android, sous `window.KiAndroid`.
 *
 * Trois choses que le Rust et la page ne savent pas faire seuls : garder le
 * vocal écran éteint (service au premier plan), ranger le mot de passe dans
 * le Keystore, et notifier quand l'appli est en arrière-plan.
 *
 * Les méthodes sont appelées sur un fil du WebView, jamais celui de
 * l'interface : rien ici ne touche aux vues.
 */
class KiAndroid(private val activite: Activity) {
  private val contexte = activite.applicationContext
  private var prochaineNotification = 100

  /**
   * Le service qui garde l'appli en vie : `mode` « vocal » (micro compris),
   * « connecte » (la connexion seulement), ou vide pour l'arrêter. `texte`
   * s'affiche dans sa notification (le salon, le serveur).
   */
  @JavascriptInterface
  fun service(mode: String, texte: String) {
    val intent = Intent(contexte, VocalService::class.java)
    if (mode.isEmpty()) {
      contexte.stopService(intent)
    } else {
      intent.putExtra(VocalService.EXTRA_MODE, mode)
      intent.putExtra(VocalService.EXTRA_TEXTE, texte)
      ContextCompat.startForegroundService(contexte, intent)
    }
  }

  @JavascriptInterface
  fun retenirSecret(nom: String, valeur: String) = Secrets.retenir(contexte, nom, valeur)

  @JavascriptInterface
  fun relireSecret(nom: String): String? = Secrets.relire(contexte, nom)

  @JavascriptInterface
  fun oublierSecret(nom: String) = Secrets.oublier(contexte, nom)

  /** Une notification de message, une par salon (la dernière remplace). */
  @JavascriptInterface
  fun notifier(salon: Int, titre: String, texte: String, mention: Boolean) {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
      ContextCompat.checkSelfPermission(contexte, Manifest.permission.POST_NOTIFICATIONS)
      != PackageManager.PERMISSION_GRANTED
    ) {
      return
    }
    val notification = NotificationCompat.Builder(contexte, CANAL_MESSAGES)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle(titre)
      .setContentText(texte)
      .setStyle(NotificationCompat.BigTextStyle().bigText(texte))
      .setAutoCancel(true)
      .setPriority(if (mention) NotificationCompat.PRIORITY_HIGH else NotificationCompat.PRIORITY_DEFAULT)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setContentIntent(VocalService.retourAppli(contexte))
      .build()
    NotificationManagerCompat.from(contexte).notify(prochaineNotification + salon, notification)
  }

  /** De retour dans l'appli : les notifications de messages n'ont plus lieu d'être. */
  @JavascriptInterface
  fun effacerNotifications() {
    val nm = NotificationManagerCompat.from(contexte)
    // Toutes sauf celle du vocal, qui appartient au service.
    nm.activeNotifications.filter { it.id != VocalService.NOTIFICATION_ID }.forEach { nm.cancel(it.id) }
  }

  companion object {
    const val CANAL_MESSAGES = "messages"
  }
}
