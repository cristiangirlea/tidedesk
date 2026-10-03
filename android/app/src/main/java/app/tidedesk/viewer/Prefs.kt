package app.tidedesk.viewer

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.provider.Settings

/** The viewer's settings, kept in the app's own storage (see [SettingsActivity]). */
class Prefs(private val context: Context) {
    private val store: SharedPreferences = context.getSharedPreferences("settings", Context.MODE_PRIVATE)

    /** Sessions start as a touchpad instead of direct touch. */
    var startAsTouchpad: Boolean
        get() = store.getBoolean("touchpad", false)
        set(value) = store.edit().putBoolean("touchpad", value).apply()

    /** How far the pointer goes for a finger's movement on the touchpad: 0.6 to 3. */
    var touchpadSpeed: Float
        get() = store.getFloat("pad-speed", 1.6f)
        set(value) = store.edit().putFloat("pad-speed", value.coerceIn(0.6f, 3f)).apply()

    /** Swiping up scrolls down, as on the phone; off: as a mouse wheel. */
    var naturalScrolling: Boolean
        get() = store.getBoolean("natural-scroll", true)
        set(value) = store.edit().putBoolean("natural-scroll", value).apply()

    /** Scroll speed from 1 (slow) to 5 (fast). */
    var scrollSpeed: Int
        get() = store.getInt("scroll-speed", 3)
        set(value) = store.edit().putInt("scroll-speed", value.coerceIn(1, 5)).apply()

    /** Pixels of two-finger movement per scroll notch, from [scrollSpeed]. */
    val scrollStep: Float get() = floatArrayOf(80f, 60f, 40f, 28f, 20f)[scrollSpeed - 1]

    var vibrate: Boolean
        get() = store.getBoolean("vibrate", true)
        set(value) = store.edit().putBoolean("vibrate", value).apply()

    /** Draw the computer's mouse pointer. */
    var showPointer: Boolean
        get() = store.getBoolean("pointer", true)
        set(value) = store.edit().putBoolean("pointer", value).apply()

    /** "auto", "landscape" or "portrait". */
    var orientation: String
        get() = store.getString("orientation", "landscape") ?: "landscape"
        set(value) = store.edit().putString("orientation", value).apply()

    var keepScreenOn: Boolean
        get() = store.getBoolean("screen-on", true)
        set(value) = store.edit().putBoolean("screen-on", value).apply()

    /** Smoother video (60 frames a second): more battery and data. */
    var smoothVideo: Boolean
        get() = store.getBoolean("smooth", false)
        set(value) = store.edit().putBoolean("smooth", value).apply()

    /** Ask before connecting over mobile data. */
    var askOnMobileData: Boolean
        get() = store.getBoolean("ask-mobile", true)
        set(value) = store.edit().putBoolean("ask-mobile", value).apply()

    /** The name computers see for this phone; empty means the phone's own. */
    var name: String
        get() = store.getString("name", "") ?: ""
        set(value) = store.edit().putString("name", value.trim()).apply()

    /** The name sent to computers: the one chosen, or the phone's. */
    val shownName: String
        get() = name.ifEmpty {
            Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME) ?: Build.MODEL
        }
}
