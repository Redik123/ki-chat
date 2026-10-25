package `fun`.baws.kichat

import android.app.ActivityOptions
import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.graphics.Rect
import android.graphics.Typeface
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.text.style.StyleSpan
import android.util.Log
import android.view.View
import android.webkit.WebView
import android.widget.RemoteViews
import androidx.core.os.BundleCompat
import org.json.JSONObject
import java.lang.ref.WeakReference

/**
 * L'écran externe des Galaxy Z Flip (« Flex Window ») et la notification du
 * vocal : ce qu'ils montrent, et ce que font leurs boutons.
 *
 * La page pousse l'état à chaque changement (`KiAndroid.ecranExterne`) : le
 * salon vocal, qui y est et qui parle, le micro, les non-lus. On le dessine
 * dans le widget et dans la notification, au plus une fois toutes les
 * [CADENCE_MS] : qui parle change plusieurs fois par seconde, et un widget
 * n'est pas fait pour suivre ce rythme (chaque mise à jour repasse par le
 * lanceur de l'écran externe).
 *
 * Les boutons repassent par la page, comme un geste dans l'appli : sans elle
 * (activité détruite), ils ne peuvent rien, et le widget le dit.
 *
 * Tout ce qui touche à l'état se fait sur le fil principal.
 */
object EcranExterne {
  /** L'écran externe, pour `ActivityOptions.setLaunchDisplayId` (0 : l'écran intérieur). */
  const val ECRAN_EXTERNE = 1
  const val ACTION_MICRO = "fun.baws.kichat.action.MICRO"
  const val ACTION_QUITTER = "fun.baws.kichat.action.QUITTER"
  private const val CADENCE_MS = 700L
  /** Les occupants listés ; au-delà, « et N autres ». */
  private const val OCCUPANTS_MAX = 4

  private const val VERT = 0xFF00D26A.toInt()
  private const val PARLE = 0xFF00E676.toInt()
  private const val ROUGE = 0xFFFF6B6B.toInt()
  private const val TEXTE = 0xFFE7EDF4.toInt()
  private const val DOUX = 0xFF94A2B2.toInt()
  private const val PALE = 0xFF3A4452.toInt()

  class Occupant(val nom: String, val couleur: Int, val parle: Boolean, val muet: Boolean, val moi: Boolean)

  class Etat(
    val connecte: Boolean,
    val serveur: String,
    /** En vocal, ou en train d'y revenir après une coupure. */
    val vocal: Boolean,
    val salon: String,
    val muet: Boolean,
    val occupants: List<Occupant>,
    val nonLus: Int,
  ) {
    /** « Bob parle », « Bob et Alice parlent », ou rien. */
    fun quiParle(): String? {
      val noms = occupants.filter { it.parle }.map { if (it.moi) "Toi" else it.nom }
      return when (noms.size) {
        0 -> null
        1 -> noms[0] + if (noms[0] == "Toi") " parles" else " parle"
        2 -> "${noms[0]} et ${noms[1]} parlent"
        else -> "${noms[0]}, ${noms[1]} et ${noms.size - 2} autres parlent"
      }
    }

    fun presents(): String = when (occupants.size) {
      0, 1 -> "Seul dans le salon"
      else -> "${occupants.size} présents"
    }

    fun texteNonLus(): String = when (nonLus) {
      0 -> "Aucun message non lu"
      1 -> "1 message non lu"
      else -> "$nonLus messages non lus"
    }
  }

  /** Le dernier état reçu de la page ; `null` sans page (appli pas ouverte). */
  var etat: Etat? = null
    private set

  private var page: WeakReference<WebView>? = null
  private val principal = Handler(Looper.getMainLooper())
  private var dernierDessin = 0L
  private var dessinPrevu = false

  /** La page est là : les boutons pourront lui parler. */
  fun pagePrete(webView: WebView) {
    page = WeakReference(webView)
  }

  /** La page s'en va (activité détruite, appli balayée) : plus rien à montrer. */
  fun pagePartie(context: Context) {
    page = null
    etat = null
    dessiner(context.applicationContext)
  }

  /** L'état poussé par la page (sur un fil du WebView). */
  fun recevoir(context: Context, json: String) {
    val e = try {
      lire(JSONObject(json))
    } catch (err: Exception) {
      Log.w("ki-chat", "état de l'écran externe illisible : $err")
      return
    }
    val app = context.applicationContext
    principal.post {
      etat = e
      planifier(app)
    }
  }

  /** Un bouton du widget ou de la notification, transmis à la page. */
  fun agir(action: String): Boolean {
    val webView = page?.get() ?: return false
    webView.post { webView.evaluateJavascript("window.kiAction && window.kiAction(\"$action\")", null) }
    return true
  }

  /** Au plus un dessin toutes les CADENCE_MS ; le premier part tout de suite. */
  private fun planifier(context: Context) {
    if (dessinPrevu) return
    dessinPrevu = true
    val attente = (dernierDessin + CADENCE_MS - SystemClock.uptimeMillis()).coerceAtLeast(0)
    principal.postDelayed({
      dessinPrevu = false
      dernierDessin = SystemClock.uptimeMillis()
      dessiner(context)
    }, attente)
  }

  private fun dessiner(context: Context) {
    dessinerWidgets(context)
    VocalService.rafraichir(context)
  }

  fun dessinerWidgets(
    context: Context,
    manager: AppWidgetManager = AppWidgetManager.getInstance(context),
    ids: IntArray = manager.getAppWidgetIds(ComponentName(context, EcranExterneWidget::class.java)),
  ) {
    for (id in ids) manager.updateAppWidget(id, vues(context, manager.getAppWidgetOptions(id)))
  }

  private fun lire(o: JSONObject): Etat? {
    if (!o.optBoolean("session")) return null
    val liste = o.optJSONArray("occupants")
    val occupants = (0 until (liste?.length() ?: 0)).map { i ->
      val p = liste!!.getJSONObject(i)
      Occupant(
        nom = p.optString("nom"),
        couleur = try {
          Color.parseColor(p.optString("couleur"))
        } catch (_: IllegalArgumentException) {
          TEXTE
        },
        parle = p.optBoolean("parle"),
        muet = p.optBoolean("muet"),
        moi = p.optBoolean("moi"),
      )
    }
    return Etat(
      connecte = o.optBoolean("connecte"),
      serveur = o.optString("serveur"),
      vocal = o.optBoolean("vocal"),
      salon = o.optString("salon"),
      muet = o.optBoolean("muet"),
      occupants = occupants,
      nonLus = o.optInt("non_lus"),
    )
  }

  // ---------------------------------------------------------------------------
  // Les gestes
  // ---------------------------------------------------------------------------

  /** Couper le micro, quitter le vocal : un récepteur de l'appli, qui passe à la page. */
  fun actionDiffusee(context: Context, action: String): PendingIntent =
    PendingIntent.getBroadcast(
      context,
      action.hashCode(),
      Intent(context, ActionVocalRecepteur::class.java).setAction(action),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )

  /**
   * Ouvrir ki-chat sur l'écran externe (recette Samsung : l'activité y est
   * lancée par `launchDisplayId`). Le widget n'est visible que téléphone
   * fermé : c'est là qu'on veut l'appli.
   */
  fun ouvrirSurEcranExterne(context: Context): PendingIntent {
    val options = ActivityOptions.makeBasic().setLaunchDisplayId(ECRAN_EXTERNE).toBundle()
    return PendingIntent.getActivity(
      context, 2, VocalService.lancement(context), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT, options
    )
  }

  // ---------------------------------------------------------------------------
  // Le widget
  // ---------------------------------------------------------------------------

  private fun vues(context: Context, options: Bundle): RemoteViews {
    val v = RemoteViews(context.packageName, R.layout.widget_ecran_externe)
    val e = etat
    // Les caméras mordent sur l'écran externe : en bas à droite sur le Flip 6
    // (Rect(379, 654 - 748, 720), en pixels du widget). Samsung donne leurs
    // zones au widget ; le contenu s'en écarte d'autant.
    fun camera(cle: String) = BundleCompat.getParcelable(options, cle, Rect::class.java)
      ?.takeUnless { it.isEmpty }?.height() ?: 0
    v.setViewPadding(R.id.racine, 0, camera("cutOutTopRect"), 0, camera("cutOutBottomRect"))
    v.setOnClickPendingIntent(R.id.racine, ouvrirSurEcranExterne(context))
    v.setOnClickPendingIntent(R.id.bouton_ouvrir, ouvrirSurEcranExterne(context))
    v.setOnClickPendingIntent(R.id.bouton_micro, actionDiffusee(context, ACTION_MICRO))
    v.setOnClickPendingIntent(R.id.bouton_quitter, actionDiffusee(context, ACTION_QUITTER))

    val enVocal = e != null && e.vocal
    v.setViewVisibility(R.id.bouton_micro, if (enVocal) View.VISIBLE else View.GONE)
    v.setViewVisibility(R.id.bouton_quitter, if (enVocal) View.VISIBLE else View.GONE)
    // Pendant une reconnexion, la liste du serveur est vide : on ne la montre pas.
    v.setViewVisibility(R.id.occupants, if (enVocal && e?.connecte == true) View.VISIBLE else View.GONE)
    v.setViewVisibility(R.id.non_lus, if (e != null) View.VISIBLE else View.GONE)

    when {
      e == null -> {
        v.setTextViewText(R.id.titre, "KI-CHAT")
        v.setTextColor(R.id.titre, DOUX)
        v.setTextViewText(R.id.salon, "Pas ouvert")
        v.setTextViewText(R.id.detail, "Touche pour ouvrir ki-chat")
        v.setInt(R.id.icone_vocal, "setColorFilter", DOUX)
      }
      !e.vocal -> {
        v.setTextViewText(R.id.titre, if (e.connecte) "CONNECTÉ" else "RECONNEXION…")
        v.setTextColor(R.id.titre, if (e.connecte) VERT else DOUX)
        v.setTextViewText(R.id.salon, "Pas en vocal")
        v.setTextViewText(R.id.detail, e.serveur)
        v.setInt(R.id.icone_vocal, "setColorFilter", DOUX)
      }
      else -> {
        v.setTextViewText(R.id.titre, if (!e.connecte) "RECONNEXION…" else if (e.muet) "EN VOCAL · MICRO COUPÉ" else "EN VOCAL")
        v.setTextColor(R.id.titre, if (!e.connecte) DOUX else if (e.muet) ROUGE else VERT)
        v.setTextViewText(R.id.salon, e.salon.ifEmpty { "Vocal" })
        val etatVocal = if (e.connecte) e.quiParle() ?: e.presents() else "Connexion perdue, le vocal va reprendre"
        v.setTextViewText(R.id.detail, listOf(etatVocal, e.serveur).filter { it.isNotEmpty() }.joinToString(" · "))
        v.setInt(R.id.icone_vocal, "setColorFilter", if (e.occupants.any { it.parle }) PARLE else VERT)
        v.setTextViewText(R.id.occupants, occupants(e))
        // Le micro : rouge quand il est coupé, comme dans l'appli.
        v.setImageViewResource(R.id.icone_micro, if (e.muet) R.drawable.ic_micro_coupe else R.drawable.ic_micro)
        v.setInt(R.id.icone_micro, "setColorFilter", if (e.muet) ROUGE else TEXTE)
        v.setTextViewText(R.id.texte_micro, if (e.muet) "Rouvrir" else "Couper")
        v.setTextColor(R.id.texte_micro, if (e.muet) ROUGE else TEXTE)
        v.setInt(R.id.bouton_micro, "setBackgroundResource", if (e.muet) R.drawable.widget_bouton_coupe else R.drawable.widget_bouton)
      }
    }
    if (e != null) {
      v.setTextViewText(R.id.non_lus, e.texteNonLus())
      v.setTextColor(R.id.non_lus, if (e.nonLus > 0) VERT else DOUX)
    }
    return v
  }

  /** Une ligne par occupant : pastille verte s'il parle, son nom dans sa couleur. */
  private fun occupants(e: Etat): CharSequence {
    val t = SpannableStringBuilder()
    for ((i, p) in e.occupants.take(OCCUPANTS_MAX).withIndex()) {
      if (i > 0) t.append('\n')
      ajouter(t, "● ", ForegroundColorSpan(if (p.parle) PARLE else PALE))
      ajouter(t, p.nom, ForegroundColorSpan(p.couleur), StyleSpan(if (p.parle) Typeface.BOLD else Typeface.NORMAL))
      if (p.moi) ajouter(t, " (toi)", ForegroundColorSpan(DOUX), RelativeSizeSpan(0.85f))
      if (p.muet) ajouter(t, "  muet", ForegroundColorSpan(ROUGE), RelativeSizeSpan(0.85f))
    }
    val reste = e.occupants.size - OCCUPANTS_MAX
    if (reste > 0) {
      t.append('\n')
      ajouter(t, "et $reste autre" + if (reste > 1) "s" else "", ForegroundColorSpan(DOUX), RelativeSizeSpan(0.85f))
    }
    if (e.occupants.isEmpty()) ajouter(t, "Personne pour l'instant", ForegroundColorSpan(DOUX))
    return t
  }

  private fun ajouter(t: SpannableStringBuilder, texte: String, vararg styles: Any) {
    val debut = t.length
    t.append(texte)
    for (s in styles) t.setSpan(s, debut, t.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
  }
}

/**
 * Le widget de l'écran externe. Déclaré pour Samsung par deux `<meta-data>`
 * (voir le manifeste), il s'ajoute dans Paramètres > Écran externe >
 * Widgets. Android ne le redessine jamais de lui-même : c'est la page qui
 * pousse l'état, et un widget fraîchement posé reprend le dernier.
 */
class EcranExterneWidget : AppWidgetProvider() {
  override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
    EcranExterne.dessinerWidgets(context, manager, ids)
  }

  /** Taille ou zones des caméras changées : on redessine avec les nouvelles. */
  override fun onAppWidgetOptionsChanged(context: Context, manager: AppWidgetManager, id: Int, options: Bundle) {
    EcranExterne.dessinerWidgets(context, manager, intArrayOf(id))
  }
}
