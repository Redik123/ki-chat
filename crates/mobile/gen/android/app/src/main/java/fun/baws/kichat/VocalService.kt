package `fun`.baws.kichat

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat

/**
 * Garde ki-chat en vie quand l'appli passe en arrière-plan.
 *
 * Sans service au premier plan, Android coupe le micro d'une appli qui passe
 * en arrière-plan, puis gèle son processus : la voix (moteur Rust, dans ce
 * même processus) s'arrêtait à l'extinction de l'écran, et la connexion au
 * serveur avec elle — plus de messages, plus de notifications.
 *
 * Deux modes, une seule notification permanente :
 * - « connecté » : la connexion tient, les messages arrivent et notifient ;
 * - « vocal » : en plus, le micro reste ouvert (type « microphone »).
 *
 * Le verrou de veille partiel garde le processeur pour les paquets QUIC ; en
 * vocal, celui du Wi-Fi garde la radio pour les cinquante datagrammes voix
 * par seconde.
 */
class VocalService : Service() {
  private var veille: PowerManager.WakeLock? = null
  private var wifi: WifiManager.WifiLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val vocal = intent?.getStringExtra(EXTRA_MODE) == MODE_VOCAL
    val texte = intent?.getStringExtra(EXTRA_TEXTE) ?: ""
    val type = when {
      vocal && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R -> ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
      !vocal && Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE -> ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
      else -> 0
    }
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(this, vocal, texte), type)
    if (veille == null) {
      val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
      veille = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ki-chat:connexion").apply {
        setReferenceCounted(false)
        acquire()
      }
    }
    if (vocal && wifi == null) {
      val wm = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
      @Suppress("DEPRECATION")
      val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        WifiManager.WIFI_MODE_FULL_LOW_LATENCY
      } else {
        WifiManager.WIFI_MODE_FULL_HIGH_PERF
      }
      wifi = wm.createWifiLock(mode, "ki-chat:vocal").apply {
        setReferenceCounted(false)
        acquire()
      }
    } else if (!vocal) {
      wifi?.release()
      wifi = null
    }
    return START_NOT_STICKY
  }

  override fun onDestroy() {
    veille?.release()
    veille = null
    wifi?.release()
    wifi = null
    super.onDestroy()
  }

  /** L'appli balayée hors des récentes : le processus part, le service avec. */
  override fun onTaskRemoved(rootIntent: Intent?) {
    stopSelf()
  }

  companion object {
    const val NOTIFICATION_ID = 1
    const val CANAL_VOCAL = "vocal"
    const val EXTRA_MODE = "mode"
    const val EXTRA_TEXTE = "texte"
    const val MODE_VOCAL = "vocal"
    const val MODE_CONNECTE = "connecte"

    fun canaux(context: Context) {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
      val nm = context.getSystemService(NotificationManager::class.java)
      nm.createNotificationChannel(
        NotificationChannel(CANAL_VOCAL, "Connexion et vocal", NotificationManager.IMPORTANCE_LOW).apply {
          description = "Pendant que ki-chat reste connecté, ou que tu es en vocal"
          setShowBadge(false)
        }
      )
      nm.createNotificationChannel(
        NotificationChannel(KiAndroid.CANAL_MESSAGES, "Messages", NotificationManager.IMPORTANCE_HIGH).apply {
          description = "Les messages et les mentions, quand l'appli est en arrière-plan"
        }
      )
    }

    fun retourAppli(context: Context): PendingIntent {
      val intent = Intent(context, MainActivity::class.java)
        .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
      return PendingIntent.getActivity(
        context, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
      )
    }

    private fun notification(context: Context, vocal: Boolean, texte: String): Notification =
      NotificationCompat.Builder(context, CANAL_VOCAL)
        .setSmallIcon(R.mipmap.ic_launcher)
        .setContentTitle(if (vocal) "En vocal" else "Connecté")
        .setContentText(texte)
        .setOngoing(true)
        .setSilent(true)
        .setCategory(if (vocal) NotificationCompat.CATEGORY_CALL else NotificationCompat.CATEGORY_SERVICE)
        .setContentIntent(retourAppli(context))
        .build()
  }
}
