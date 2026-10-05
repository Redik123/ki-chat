package `fun`.baws.kichat

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.util.Log

/**
 * Le retour de l'installeur d'Android pendant une mise à jour.
 *
 * Quand il lui faut l'accord de l'utilisateur, il le dit ici
 * (`STATUS_PENDING_USER_ACTION`) en joignant l'écran de confirmation, qu'on
 * ouvre. Le reste (succès, refus, échec) ne fait que se noter dans le
 * journal : en cas de succès, Android relance l'appli à jour.
 */
class InstallRecepteur : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    when (val statut = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
      PackageInstaller.STATUS_PENDING_USER_ACTION -> {
        val confirmation: Intent? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
          intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
        } else {
          @Suppress("DEPRECATION")
          intent.getParcelableExtra(Intent.EXTRA_INTENT)
        }
        confirmation?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)?.let { context.startActivity(it) }
      }
      PackageInstaller.STATUS_SUCCESS -> Log.i("ki-chat", "mise à jour installée")
      else -> Log.w(
        "ki-chat",
        "mise à jour non installée ($statut) : ${intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)}"
      )
    }
  }
}
