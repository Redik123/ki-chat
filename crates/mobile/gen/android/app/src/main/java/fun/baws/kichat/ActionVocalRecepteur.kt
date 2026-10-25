package `fun`.baws.kichat

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Les boutons « Couper le micro » et « Quitter le vocal », du widget de
 * l'écran externe comme de la notification du vocal. Ils passent par la page,
 * comme le même geste dans l'appli (voir `kiAction` dans index.html).
 */
class ActionVocalRecepteur : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val action = when (intent.action) {
      EcranExterne.ACTION_MICRO -> "micro"
      EcranExterne.ACTION_QUITTER -> "quitter"
      else -> return
    }
    if (!EcranExterne.agir(action)) {
      // Plus de page : l'appli n'est pas ouverte, le widget le montrera.
      Log.w("ki-chat", "bouton « $action » sans page pour le recevoir")
      EcranExterne.pagePartie(context)
    }
  }
}
