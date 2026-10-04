package `fun`.baws.kichat

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Le micro se demande dès le lancement : le moteur voix (cpal → AAudio)
    // ouvre la capture à la connexion, sans passer par la page.
    if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO)
      != PackageManager.PERMISSION_GRANTED) {
      ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.RECORD_AUDIO), 1)
    }
  }
}
