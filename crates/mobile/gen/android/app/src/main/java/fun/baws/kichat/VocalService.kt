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
 * Le vocal tient écran éteint.
 *
 * Sans service au premier plan, Android coupe le micro d'une appli qui passe
 * en arrière-plan, et finit par geler son processus : la voix (moteur Rust,
 * dans ce même processus) s'arrêtait dès qu'on éteignait l'écran. Le service
 * de type « microphone » garde le droit au micro et le processus éveillé ;
 * une notification permanente dit qu'on est en vocal.
 *
 * Le verrou de veille partiel et celui du Wi-Fi gardent le processeur et la
 * radio pour les datagrammes voix, cinquante par seconde.
 */
class VocalService : Service() {
  private var veille: PowerManager.WakeLock? = null
  private var wifi: WifiManager.WifiLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val salon = intent?.getStringExtra(EXTRA_SALON) ?: "Vocal"
    val notification = notification(this, salon)
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
    } else {
      0
    }
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
    if (veille == null) {
      val pm = getSystemService(Context.POWER_SERVICE) as PowerManager
      veille = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ki-chat:vocal").apply {
        setReferenceCounted(false)
        acquire()
      }
    }
    if (wifi == null) {
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

  /** L'appli balayée hors des récentes : le processus part, le vocal avec. */
  override fun onTaskRemoved(rootIntent: Intent?) {
    stopSelf()
  }

  companion object {
    const val NOTIFICATION_ID = 1
    const val CANAL_VOCAL = "vocal"
    const val EXTRA_SALON = "salon"

    fun canaux(context: Context) {
      if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
      val nm = context.getSystemService(NotificationManager::class.java)
      nm.createNotificationChannel(
        NotificationChannel(CANAL_VOCAL, "En vocal", NotificationManager.IMPORTANCE_LOW).apply {
          description = "Pendant que tu es dans un salon vocal"
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

    private fun notification(context: Context, salon: String): Notification =
      NotificationCompat.Builder(context, CANAL_VOCAL)
        .setSmallIcon(R.mipmap.ic_launcher)
        .setContentTitle("En vocal")
        .setContentText(salon)
        .setOngoing(true)
        .setSilent(true)
        .setCategory(NotificationCompat.CATEGORY_CALL)
        .setContentIntent(retourAppli(context))
        .build()
  }
}
