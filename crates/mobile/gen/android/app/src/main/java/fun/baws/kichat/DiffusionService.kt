package `fun`.baws.kichat

import android.Manifest
import android.app.Activity
import android.app.Notification
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.Process
import android.os.SystemClock
import android.util.DisplayMetrics
import android.util.Log
import android.view.Display
import android.view.Surface
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.nio.ByteBuffer
import kotlin.math.max
import kotlin.math.min

/**
 * Diffuser l'écran du téléphone : le service exigé par Android pour la capture.
 *
 * L'utilisateur a accepté la fenêtre système (MainActivity) ; la capture écrit dans
 * la surface de l'encodeur matériel, et chaque trame encodée part vers Rust
 * (flux.rs), qui la chiffre et l'envoie au format du PC. Le serveur répond dans la
 * valeur rendue par [KiVideo.trame] : trame clé demandée, débit à tenir, ou fin.
 *
 * Si l'écran tourne, ou que le Flip se déplie, l'encodeur est refait aux nouvelles
 * dimensions : les spectateurs les lisent dans l'en-tête de la trame clé suivante.
 */
class DiffusionService : Service() {
  private val principal = Handler(Looper.getMainLooper())
  private var projection: MediaProjection? = null
  private var affichage: VirtualDisplay? = null
  private var encodeur: Encodeur? = null
  private var filSon: Thread? = null
  @Volatile private var arrete = false
  private var qualite = QUALITES.getValue(QUALITE_DEFAUT)

  private val suiviEcran = object : DisplayManager.DisplayListener {
    override fun onDisplayAdded(id: Int) {}
    override fun onDisplayRemoved(id: Int) {}
    override fun onDisplayChanged(id: Int) {
      if (id == Display.DEFAULT_DISPLAY) refaireSiBesoin()
    }
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    if (intent == null || intent.action == ACTION_ARRETER) {
      arreter()
      return START_NOT_STICKY
    }
    // D'abord au premier plan : Android 14 refuse la capture sinon.
    val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
      ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
    } else {
      0
    }
    ServiceCompat.startForeground(this, NOTIFICATION_ID, notification(), type)
    qualite = QUALITES[intent.getStringExtra(EXTRA_QUALITE)] ?: qualite
    val resultat = intent.getIntExtra(EXTRA_RESULTAT, Activity.RESULT_CANCELED)
    @Suppress("DEPRECATION")
    val donnees = intent.getParcelableExtra<Intent>(EXTRA_DONNEES)
    // Déjà en cours : la capture ne se demande qu'une fois.
    if (projection == null && (donnees == null || !demarrer(resultat, donnees))) arreter()
    return START_NOT_STICKY
  }

  private fun demarrer(resultat: Int, donnees: Intent): Boolean {
    val p = try {
      getSystemService(MediaProjectionManager::class.java).getMediaProjection(resultat, donnees)
    } catch (e: Exception) {
      Log.w(TAG, "capture refusée", e)
      null
    }
    if (p == null) {
      EcranExterne.direALaPage("Android refuse de partager l'écran.")
      return false
    }
    projection = p
    // Avant la surface : Android 14 l'exige, et c'est par là qu'arrive la fin de
    // la capture (l'utilisateur l'arrête depuis le système, l'écran se verrouille).
    p.registerCallback(object : MediaProjection.Callback() {
      override fun onStop() = arreter()
    }, principal)
    val (largeur, hauteur, densite) = tailleCible()
    if (!KiVideo.demarrerDiffusion(largeur, hauteur, qualite.fps, qualite.kbps)) {
      EcranExterne.direALaPage("Rejoins un salon vocal pour diffuser.")
      return false
    }
    val e = try {
      Encodeur(largeur, hauteur)
    } catch (ex: Exception) {
      Log.w(TAG, "encodeur indisponible", ex)
      EcranExterne.direALaPage("Ce téléphone n'a pas pu lancer l'encodeur vidéo.")
      return false
    }
    encodeur = e
    affichage = p.createVirtualDisplay(
      "ki-chat",
      largeur,
      hauteur,
      densite,
      DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
      e.surface,
      null,
      principal,
    )
    getSystemService(DisplayManager::class.java).registerDisplayListener(suiviEcran, principal)
    demarrerSon(p)
    Log.i(TAG, "diffusion : ${largeur}x$hauteur, ${qualite.fps} i/s, ${qualite.kbps} kbit/s")
    return true
  }

  /** L'écran a changé de forme : un encodeur neuf, aux nouvelles dimensions. */
  private fun refaireSiBesoin() {
    val ancien = encodeur ?: return
    val (largeur, hauteur, densite) = tailleCible()
    if (largeur == ancien.largeur && hauteur == ancien.hauteur) return
    val neuf = try {
      Encodeur(largeur, hauteur)
    } catch (e: Exception) {
      Log.w(TAG, "encodeur non refait", e)
      return
    }
    encodeur = neuf
    affichage?.surface = neuf.surface
    affichage?.resize(largeur, hauteur, densite)
    ancien.arreter()
    Log.i(TAG, "diffusion : ${largeur}x$hauteur")
  }

  /**
   * L'écran réel ramené à la qualité choisie : son petit côté à la hauteur voulue,
   * dans ce que l'encodeur sait faire et à ses multiples.
   */
  private fun tailleCible(): Triple<Int, Int, Int> {
    val mesure = DisplayMetrics()
    @Suppress("DEPRECATION")
    getSystemService(DisplayManager::class.java).getDisplay(Display.DEFAULT_DISPLAY).getRealMetrics(mesure)
    val (reelleL, reelleH) = mesure.widthPixels to mesure.heightPixels
    var echelle = min(1f, qualite.hauteur.toFloat() / min(reelleL, reelleH))
    val c = capacites()
    val maxLong = c?.supportedWidths?.upper ?: 3840
    echelle = min(echelle, maxLong.toFloat() / max(reelleL, reelleH))
    val (alignL, alignH) = (c?.widthAlignment ?: 2) to (c?.heightAlignment ?: 2)
    val largeur = max(((reelleL * echelle).toInt() / alignL) * alignL, alignL)
    val hauteur = max(((reelleH * echelle).toInt() / alignH) * alignH, alignH)
    return Triple(largeur, hauteur, mesure.densityDpi)
  }

  private fun capacites(): MediaCodecInfo.VideoCapabilities? = try {
    val codec = MediaCodec.createEncoderByType(MIME)
    val c = codec.codecInfo.getCapabilitiesForType(MIME).videoCapabilities
    codec.release()
    c
  } catch (e: Exception) {
    null
  }

  /**
   * Le son de ce qui joue sur le téléphone, sauf ki-chat (sinon le vocal reviendrait
   * chez ceux qui le disent). Android 10 ou plus, et seulement les applis qui le
   * permettent : sans ça, la diffusion part sans son, l'image n'en dépend pas.
   */
  private fun demarrerSon(p: MediaProjection) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return
    if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) return
    val enregistreur = try {
      val capture = AudioPlaybackCaptureConfiguration.Builder(p)
        .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
        .addMatchingUsage(AudioAttributes.USAGE_GAME)
        .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
        .excludeUid(Process.myUid())
        .build()
      val format = AudioFormat.Builder()
        .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
        .setSampleRate(FREQUENCE)
        .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
        .build()
      AudioRecord.Builder()
        .setAudioFormat(format)
        .setBufferSizeInBytes(FREQUENCE / 5 * 2 * Float.SIZE_BYTES)
        .setAudioPlaybackCaptureConfig(capture)
        .build()
    } catch (e: Exception) {
      Log.w(TAG, "son de la diffusion indisponible", e)
      return
    }
    filSon = Thread({
      try {
        enregistreur.startRecording()
        val pcm = FloatArray(FREQUENCE / 50 * 2)
        while (!arrete) {
          val n = enregistreur.read(pcm, 0, pcm.size, AudioRecord.READ_BLOCKING)
          if (n < 0) break
          if (n > 0) KiVideo.son(pcm, n, System.nanoTime() / 1000)
        }
      } catch (e: Exception) {
        Log.w(TAG, "son de la diffusion arrêté", e)
      } finally {
        enregistreur.release()
      }
    }, "diffusion-son").apply { start() }
  }

  private fun arreter() {
    if (arrete) return
    arrete = true
    try {
      getSystemService(DisplayManager::class.java).unregisterDisplayListener(suiviEcran)
    } catch (_: Exception) {
    }
    affichage?.release()
    affichage = null
    encodeur?.arreter()
    encodeur = null
    filSon?.join(300)
    filSon = null
    projection?.stop()
    projection = null
    KiVideo.arreterDiffusion()
    ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
    stopSelf()
  }

  override fun onDestroy() {
    arreter()
    super.onDestroy()
  }

  /** L'encodeur matériel, et le fil qui en sort les trames vers Rust. */
  private inner class Encodeur(val largeur: Int, val hauteur: Int) {
    private val codec = MediaCodec.createEncoderByType(MIME)
    val surface: Surface
    @Volatile private var stop = false
    private var kbps = qualite.kbps
    private var derniereCle = 0L
    private val fil: Thread

    init {
      configurer()
      surface = codec.createInputSurface()
      codec.start()
      fil = Thread({ boucle() }, "diffusion-video").apply { start() }
    }

    private fun format(strict: Boolean) = MediaFormat.createVideoFormat(MIME, largeur, hauteur).apply {
      setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
      setInteger(MediaFormat.KEY_BIT_RATE, kbps * 1000)
      setInteger(MediaFormat.KEY_FRAME_RATE, qualite.fps)
      // Un écran à 120 Hz ne doit pas lui donner deux fois trop d'images.
      setFloat("max-fps-to-encoder", qualite.fps.toFloat())
      setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, INTERVALLE_CLE_S)
      // Écran immobile : l'image repart quand même, pour qu'un nouveau spectateur
      // la voie — et que `trame` soit appelée, c'est elle qui rapporte les ordres.
      setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPETITION_US)
      setInteger(MediaFormat.KEY_PRIORITY, 0)
      if (strict) {
        // Baseline : aucune image B, ce que tout décodeur du groupe sait lire.
        setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
        setInteger(MediaFormat.KEY_BITRATE_MODE, MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
      }
    }

    private fun configurer() {
      try {
        codec.configure(format(strict = true), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
      } catch (e: Exception) {
        Log.i(TAG, "encodeur : réglages stricts refusés, on laisse les siens", e)
        codec.reset()
        codec.configure(format(strict = false), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
      }
    }

    private fun boucle() {
      val info = MediaCodec.BufferInfo()
      // SPS et PPS, remis devant chaque trame clé : un spectateur arrivé en cours de
      // route doit pouvoir démarrer sur n'importe laquelle.
      var parametres = ByteArray(0)
      var tampon = ByteBuffer.allocateDirect(1 shl 20)
      try {
        while (!stop) {
          val i = codec.dequeueOutputBuffer(info, 10_000)
          if (i < 0) continue
          val sortie = codec.getOutputBuffer(i)
          if (sortie != null && info.size > 0) {
            sortie.limit(info.offset + info.size)
            sortie.position(info.offset)
            if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) {
              parametres = ByteArray(info.size).also { sortie.get(it) }
            } else {
              val idr = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
              val total = info.size + if (idr) parametres.size else 0
              if (tampon.capacity() < total) tampon = ByteBuffer.allocateDirect(total * 2)
              tampon.clear()
              if (idr) tampon.put(parametres)
              tampon.put(sortie)
              val ordre = KiVideo.trame(tampon, 0, total, info.presentationTimeUs, idr, largeur, hauteur)
              if (ordre < 0) {
                principal.post { this@DiffusionService.arreter() }
              } else {
                obeir(ordre)
              }
            }
          }
          codec.releaseOutputBuffer(i, false)
        }
      } catch (e: IllegalStateException) {
        Log.i(TAG, "encodeur arrêté")
      }
    }

    private fun obeir(ordre: Int) {
      val maintenant = SystemClock.elapsedRealtime()
      // L'ordre se répète à chaque trame tant que la clé n'est pas sortie.
      if (ordre and 1 != 0 && maintenant - derniereCle > 500) {
        derniereCle = maintenant
        codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
      }
      // Jamais plus que la qualité choisie : le serveur ne connaît que ses paliers.
      val voulu = min(ordre ushr 1, qualite.kbps)
      if (voulu > 0 && voulu != kbps) {
        kbps = voulu
        codec.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, voulu * 1000) })
      }
    }

    fun arreter() {
      stop = true
      fil.join(500)
      try {
        codec.stop()
      } catch (_: Exception) {
      }
      codec.release()
      surface.release()
    }
  }

  private fun notification(): Notification {
    val ouvrir = packageManager.getLaunchIntentForPackage(packageName)
    val toucher = PendingIntent.getActivity(this, 0, ouvrir, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    val stop = PendingIntent.getService(
      this,
      1,
      Intent(this, DiffusionService::class.java).setAction(ACTION_ARRETER),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    return NotificationCompat.Builder(this, VocalService.CANAL_VOCAL)
      .setSmallIcon(R.mipmap.ic_launcher)
      .setContentTitle("ki-chat diffuse ton écran")
      .setContentText("Ton salon vocal voit tout ce qui s'affiche.")
      .setContentIntent(toucher)
      .setOngoing(true)
      .setSilent(true)
      .addAction(R.drawable.ic_raccrocher, "Arrêter", stop)
      .build()
  }

  /** Ce que propose la page. */
  private data class Qualite(val hauteur: Int, val fps: Int, val kbps: Int)

  companion object {
    private const val TAG = "ki-chat"
    private const val NOTIFICATION_ID = 7
    private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC
    private const val FREQUENCE = 48_000
    private const val INTERVALLE_CLE_S = 5
    private const val REPETITION_US = 100_000L
    private const val EXTRA_RESULTAT = "resultat"
    private const val EXTRA_DONNEES = "donnees"
    private const val EXTRA_QUALITE = "qualite"
    private const val ACTION_ARRETER = "fun.baws.kichat.ARRETER_DIFFUSION"
    private const val QUALITE_DEFAUT = "720p30"
    private val QUALITES = mapOf(
      "720p30" to Qualite(720, 30, 3_000),
      "720p60" to Qualite(720, 60, 5_000),
      "1080p30" to Qualite(1080, 30, 6_000),
    )

    /** L'utilisateur a accepté la fenêtre système : on démarre. */
    fun demarrer(context: Context, resultat: Int, donnees: Intent, qualite: String) {
      val intent = Intent(context, DiffusionService::class.java)
        .putExtra(EXTRA_RESULTAT, resultat)
        .putExtra(EXTRA_DONNEES, donnees)
        .putExtra(EXTRA_QUALITE, qualite)
      ContextCompat.startForegroundService(context, intent)
    }

    fun arreter(context: Context) {
      context.startService(Intent(context, DiffusionService::class.java).setAction(ACTION_ARRETER))
    }
  }
}
