package `fun`.baws.kichat

import android.view.Surface
import java.nio.ByteBuffer

/**
 * Le pont avec le cœur Rust pour les partages d'écran (crates/mobile/src/flux.rs).
 * Les noms comptent : ce sont ceux des fonctions JNI, et proguard-rules.pro les
 * protège du renommage en version release.
 */
object KiVideo {
  init {
    System.loadLibrary("ki_mobile_lib")
  }

  /** La surface où peindre le stream regardé, ou `null` quand elle disparaît. */
  @JvmStatic external fun surface(surface: Surface?)

  /** Ce qui est peint : `(largeur shl 32) or hauteur`, 0 avant la première image. */
  @JvmStatic external fun dimensions(): Long

  /** Sert le stream à la télé : rend `port/jeton/flux.mp4`, ou `null` si c'est impossible. */
  @JvmStatic external fun teleDemarrer(): String?

  @JvmStatic external fun teleArreter()

  /** Change quand la télé doit recharger l'adresse. */
  @JvmStatic external fun teleGeneration(): Int

  /** Fluide (un peu de retard, image régulière) ou faible latence. */
  @JvmStatic external fun fluide(oui: Boolean)

  /** Les chiffres du stream regardé (plusieurs lignes), vide au tout début. */
  @JvmStatic external fun chiffres(): String?

  /** Annonce la diffusion au serveur. Faux : pas connecté, ou pas en vocal. */
  @JvmStatic external fun demarrerDiffusion(largeur: Int, hauteur: Int, fps: Int, kbps: Int): Boolean

  /**
   * Une trame encodée, dans un tampon direct. Rend -1 si la diffusion est finie,
   * sinon `(kbps shl 1) or trameCle` : le débit à tenir, et s'il faut une trame clé.
   */
  @JvmStatic external fun trame(
    tampon: ByteBuffer,
    debut: Int,
    taille: Int,
    ptsUs: Long,
    idr: Boolean,
    largeur: Int,
    hauteur: Int,
  ): Int

  /** Du son capturé : flottants stéréo entrelacés à 48 kHz. */
  @JvmStatic external fun son(pcm: FloatArray, n: Int, finUs: Long)

  @JvmStatic external fun arreterDiffusion()
}
