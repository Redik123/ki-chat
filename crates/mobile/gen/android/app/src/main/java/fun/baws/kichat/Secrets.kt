package `fun`.baws.kichat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Le mot de passe retenu, chiffré par une clé du Keystore Android.
 *
 * La clé AES ne quitte jamais le Keystore (matériel sécurisé quand le
 * téléphone en a un) : ce qui est écrit dans les préférences de l'appli
 * n'est qu'un texte chiffré, inutile sans ce téléphone-là. Le pendant, sur
 * PC, du gestionnaire d'identifiants de Windows (voir secret.rs).
 */
object Secrets {
  private const val ALIAS = "ki-chat-secrets"
  private const val FICHIER = "secrets"
  private const val GCM_BITS = 128

  private fun cle(): SecretKey {
    val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (ks.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
    val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
    gen.init(
      KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
        .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
        .setKeySize(256)
        .build()
    )
    return gen.generateKey()
  }

  fun retenir(context: Context, nom: String, valeur: String) {
    val chiffre = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, cle()) }
    val donnees = chiffre.doFinal(valeur.toByteArray(Charsets.UTF_8))
    // Le vecteur d'initialisation, tiré par le Keystore, voyage devant.
    val tout = chiffre.iv + donnees
    context.getSharedPreferences(FICHIER, Context.MODE_PRIVATE).edit()
      .putString(nom, Base64.encodeToString(tout, Base64.NO_WRAP))
      .apply()
  }

  /** `null` s'il n'y a rien, ou si la clé a disparu (appli réinstallée). */
  fun relire(context: Context, nom: String): String? {
    val brut = context.getSharedPreferences(FICHIER, Context.MODE_PRIVATE).getString(nom, null) ?: return null
    return try {
      val tout = Base64.decode(brut, Base64.NO_WRAP)
      val iv = tout.copyOfRange(0, 12)
      val dechiffre = Cipher.getInstance("AES/GCM/NoPadding")
        .apply { init(Cipher.DECRYPT_MODE, cle(), GCMParameterSpec(GCM_BITS, iv)) }
      String(dechiffre.doFinal(tout, 12, tout.size - 12), Charsets.UTF_8)
    } catch (e: Exception) {
      oublier(context, nom)
      null
    }
  }

  fun oublier(context: Context, nom: String) {
    context.getSharedPreferences(FICHIER, Context.MODE_PRIVATE).edit().remove(nom).apply()
  }
}
