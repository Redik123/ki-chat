package `fun`.baws.kichat

import android.app.Activity
import android.content.Context
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.graphics.Typeface
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlin.math.min

/**
 * Le stream qu'on regarde, en plein écran par-dessus la page.
 *
 * Une vue native et non une balise de la page : le décodeur matériel peint
 * directement dans la `Surface` (flux.rs), sans copie ni passage par la WebView.
 * Au-dessus de la page dans la même activité : la page garde sa session et son
 * vocal, et « retour » referme la vue sans quitter ki-chat.
 */
class RegardVue(
  private val activite: Activity,
  private val quandFerme: () -> Unit,
  /** Envoyer ce stream sur la télé ; rien si ce téléphone ne sait pas le faire. */
  private val quandTele: ((String) -> Unit)?,
) {
  private val boucle = Handler(Looper.getMainLooper())
  private var racine: FrameLayout? = null
  private var ecran: SurfaceView? = null
  private var attente: ProgressBar? = null
  private var barre: View? = null
  private var chiffres: TextView? = null
  private val preferences = activite.getSharedPreferences("ki-regard", Context.MODE_PRIVATE)
  private var dimensions = 0L
  private var enLargeur = false

  val ouvert: Boolean
    get() = racine != null

  private val suivre = object : Runnable {
    override fun run() {
      ajuster()
      if (barre?.visibility == View.VISIBLE) {
        val texte = KiVideo.chiffres().orEmpty().ifEmpty { "En attente de la première image…" }
        chiffres?.text = texte + "\n" + "Écran : ${frequence()} Hz"
      }
      boucle.postDelayed(this, 300)
    }
  }
  private val masquerBarre = Runnable { barre?.visibility = View.GONE }

  fun ouvrir(nom: String) {
    if (ouvert) fermer(prevenir = false)
    KiVideo.fluide(fluide())
    val r = FrameLayout(activite).apply {
      setBackgroundColor(Color.BLACK)
      // Un toucher montre ou cache la barre, et ne passe pas à la page dessous.
      isClickable = true
      setOnClickListener { basculerBarre() }
    }
    val s = SurfaceView(activite).apply {
      keepScreenOn = true
      holder.addCallback(object : SurfaceHolder.Callback {
        override fun surfaceCreated(holder: SurfaceHolder) {
          // Le système règle la dalle sur ce qu'il croit voir : 30 images/s, et le Flip 6
          // descend à 24 Hz pour économiser, en jetant une image sur cinq. Le stream est
          // une source à son propre rythme, jusqu'à 60 images/s : il le lui dit.
          if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            holder.surface.setFrameRate(FREQUENCE_DE_L_ECRAN, Surface.FRAME_RATE_COMPATIBILITY_FIXED_SOURCE)
          }
          KiVideo.surface(holder.surface)
        }
        override fun surfaceChanged(holder: SurfaceHolder, format: Int, largeur: Int, hauteur: Int) {}
        override fun surfaceDestroyed(holder: SurfaceHolder) = KiVideo.surface(null)
      })
    }
    r.addView(s, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT, Gravity.CENTER))
    val p = ProgressBar(activite)
    r.addView(p, FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.CENTER))
    val b = barreDuHaut(nom)
    r.addView(b, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP))
    activite.findViewById<ViewGroup>(android.R.id.content).addView(
      r,
      ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT),
    )
    racine = r
    ecran = s
    attente = p
    barre = b
    dimensions = 0
    enLargeur = false
    barresSysteme(visibles = false)
    boucle.post(suivre)
    // À l'ouverture, le temps de voir qu'elle existe. Après, c'est un toucher qui
    // la montre et un autre qui la cache : les chiffres, ça se lit tranquillement.
    montrerBarre()
    boucle.postDelayed(masquerBarre, 4000)
  }

  /** `prevenir` : c'est l'utilisateur qui ferme, et la page doit l'apprendre. */
  fun fermer(prevenir: Boolean) {
    val r = racine ?: return
    boucle.removeCallbacks(suivre)
    boucle.removeCallbacks(masquerBarre)
    (r.parent as? ViewGroup)?.removeView(r)
    racine = null
    ecran = null
    attente = null
    barre = null
    chiffres = null
    barresSysteme(visibles = true)
    activite.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
    if (prevenir) quandFerme()
  }

  private fun barreDuHaut(nom: String): View {
    val marge = (12 * activite.resources.displayMetrics.density).toInt()
    val colonne = LinearLayout(activite).apply {
      orientation = LinearLayout.VERTICAL
      setBackgroundColor(Color.argb(170, 0, 0, 0))
      setPadding(marge, marge, marge, marge)
    }
    // L'appareil photo perce l'écran : ni bouton ni chiffre dessous.
    ViewCompat.setOnApplyWindowInsetsListener(colonne) { v, insets ->
      val e = insets.getInsets(WindowInsetsCompat.Type.displayCutout() or WindowInsetsCompat.Type.systemBars())
      v.setPadding(marge + e.left, marge + e.top, marge + e.right, marge)
      insets
    }
    val b = LinearLayout(activite).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
    }
    colonne.addView(b)
    colonne.addView(Button(activite).apply {
      text = libelleDuMode(fluide())
      setOnClickListener {
        val oui = !fluide()
        preferences.edit().putBoolean(CLE_FLUIDE, oui).apply()
        KiVideo.fluide(oui)
        text = libelleDuMode(oui)
      }
    })
    val c = TextView(activite).apply {
      setTextColor(Color.WHITE)
      textSize = 12f
      typeface = Typeface.MONOSPACE
      setPadding(0, marge, 0, 0)
    }
    colonne.addView(c)
    chiffres = c
    b.addView(Button(activite).apply {
      text = "Fermer"
      setOnClickListener { fermer(prevenir = true) }
    })
    b.addView(
      TextView(activite).apply {
        text = nom
        setTextColor(Color.WHITE)
        textSize = 16f
        maxLines = 1
        setPadding((12 * resources.displayMetrics.density).toInt(), 0, 0, 0)
      },
      LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f),
    )
    if (quandTele != null) {
      b.addView(Button(activite).apply {
        text = "TV"
        setOnClickListener { quandTele.invoke(nom) }
      })
    }
    b.addView(Button(activite).apply {
      text = "Tourner"
      setOnClickListener {
        enLargeur = !enLargeur
        activite.requestedOrientation =
          if (enLargeur) ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE else ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT
      }
    })
    return colonne
  }

  private fun frequence(): Int {
    val ecran = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      activite.display
    } else {
      @Suppress("DEPRECATION")
      activite.windowManager.defaultDisplay
    }
    return ecran?.refreshRate?.toInt() ?: 0
  }

  private fun fluide() = preferences.getBoolean(CLE_FLUIDE, true)

  private fun libelleDuMode(fluide: Boolean) = if (fluide) "Mode : fluide" else "Mode : faible latence"

  private fun basculerBarre() {
    if (barre?.visibility == View.VISIBLE) masquerBarre.run() else montrerBarre()
  }

  private fun montrerBarre() {
    barre?.visibility = View.VISIBLE
    boucle.removeCallbacks(masquerBarre)
  }

  private fun barresSysteme(visibles: Boolean) {
    val fenetre = activite.window
    val controle = WindowCompat.getInsetsController(fenetre, fenetre.decorView)
    if (visibles) {
      controle.show(WindowInsetsCompat.Type.systemBars())
    } else {
      controle.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
      controle.hide(WindowInsetsCompat.Type.systemBars())
    }
  }

  /** Garde les proportions du stream, et passe en largeur s'il l'est. */
  private fun ajuster() {
    val r = racine ?: return
    val s = ecran ?: return
    val d = KiVideo.dimensions()
    if (d == 0L || r.width == 0 || r.height == 0) return
    val largeur = (d ushr 32).toInt()
    val hauteur = (d and 0xffffffffL).toInt()
    if (largeur <= 0 || hauteur <= 0) return
    if (dimensions == 0L) {
      attente?.visibility = View.GONE
      // Une première image en largeur : on tourne, comme un lecteur vidéo.
      if (largeur > hauteur && !enLargeur) {
        enLargeur = true
        activite.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
      }
    }
    dimensions = d
    val echelle = min(r.width.toFloat() / largeur, r.height.toFloat() / hauteur)
    val l = (largeur * echelle).toInt()
    val h = (hauteur * echelle).toInt()
    val mise = s.layoutParams
    if (mise.width != l || mise.height != h) {
      mise.width = l
      mise.height = h
      s.layoutParams = mise
    }
  }

  private companion object {
    const val CLE_FLUIDE = "fluide"
    const val FREQUENCE_DE_L_ECRAN = 60f
  }
}
