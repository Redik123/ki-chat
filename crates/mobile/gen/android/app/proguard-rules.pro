# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile
# La page appelle window.KiAndroid : R8 ne doit ni renommer ni retirer ses
# méthodes, qu'il croit inutilisées (aucun appel Kotlin ne les vise).
-keepclassmembers class fun.baws.kichat.KiAndroid {
    @android.webkit.JavascriptInterface <methods>;
}

# Le pont avec Rust : les noms des fonctions JNI sont ceux de la classe et de ses
# méthodes, et R8 ne doit ni les renommer ni les retirer.
-keepclasseswithmembernames class fun.baws.kichat.KiVideo {
    native <methods>;
}
-keep class fun.baws.kichat.KiVideo { *; }

# Le SDK Cast retrouve cette classe par son nom, écrit dans le manifeste.
-keep class fun.baws.kichat.ReglagesCast { *; }
