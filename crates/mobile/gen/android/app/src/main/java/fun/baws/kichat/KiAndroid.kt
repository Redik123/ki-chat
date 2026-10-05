package `fun`.baws.kichat

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.app.PendingIntent
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.webkit.JavascriptInterface
import java.io.File
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

  /**
   * Installe l'APK d'une mise à jour, déjà téléchargé et vérifié par le Rust
   * (manifeste signé). Android vérifie de son côté qu'il porte la même
   * signature que l'appli installée, et demande l'accord de l'utilisateur
   * quand il le faut.
   *
   * Rend « ok », « autorisation » (il faut d'abord permettre à ki-chat
   * d'installer des applis : l'écran des réglages vient de s'ouvrir) ou le
   * message d'erreur.
   */
  @JavascriptInterface
  fun installerApk(chemin: String): String {
    return try {
      val pm = contexte.packageManager
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !pm.canRequestPackageInstalls()) {
        activite.startActivity(
          Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${contexte.packageName}"))
        )
        return "autorisation"
      }
      val fichier = File(chemin)
      val installeur = pm.packageInstaller
      val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL).apply {
        setAppPackageName(contexte.packageName)
        // Android 12+ : une appli qui s'installe elle-même peut se mettre à
        // jour sans confirmation, une fois qu'elle a fait la première.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
          setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
        }
      }
      val id = installeur.createSession(params)
      installeur.openSession(id).use { session ->
        fichier.inputStream().use { entree ->
          session.openWrite("ki-chat.apk", 0, fichier.length()).use { sortie ->
            entree.copyTo(sortie)
            session.fsync(sortie)
          }
        }
        val retour = PendingIntent.getBroadcast(
          contexte, id, Intent(contexte, InstallRecepteur::class.java),
          PendingIntent.FLAG_UPDATE_CURRENT or
            (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0)
        )
        session.commit(retour.intentSender)
      }
      "ok"
    } catch (e: Exception) {
      e.message ?: e.toString()
    }
  }

  companion object {
    const val CANAL_MESSAGES = "messages"
  }
}
