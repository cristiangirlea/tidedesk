package app.tidedesk.viewer

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.graphics.Typeface
import android.net.Uri
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.SeekBar
import android.widget.Switch
import android.widget.TextView

/**
 * The viewer's settings, how to control a computer, and what this app is.
 * Changes are saved at once and apply from the next session.
 */
class SettingsActivity : Activity() {
    private lateinit var prefs: Prefs
    private lateinit var column: LinearLayout

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = Prefs(this)
        val pad = dp(24)
        column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad * 2, pad, pad * 2)
        }
        column.addView(TextView(this).apply {
            text = "Settings"
            textSize = 24f
            setTextColor(Color.WHITE)
        })
        note("Saved at once; they apply from the next session.")

        section("Control")
        choice(
            "A session starts with",
            listOf("Touch: tap where you want to click" to false, "Touchpad: move a pointer" to true),
            prefs.startAsTouchpad,
        ) { prefs.startAsTouchpad = it }
        slider("Touchpad speed", 6, 30, (prefs.touchpadSpeed * 10).toInt()) { prefs.touchpadSpeed = it / 10f }
        toggle("Natural scrolling", "Swiping up scrolls down, as on the phone. Off: as a mouse wheel.", prefs.naturalScrolling) {
            prefs.naturalScrolling = it
        }
        slider("Scroll speed", 1, 5, prefs.scrollSpeed) { prefs.scrollSpeed = it }
        toggle("Vibrate on taps", null, prefs.vibrate) { prefs.vibrate = it }
        toggle("Show the computer's pointer", "Its mouse pointer, drawn on the phone.", prefs.showPointer) {
            prefs.showPointer = it
        }

        section("Screen")
        choice(
            "Orientation during a session",
            listOf("Landscape" to "landscape", "Portrait" to "portrait", "Turn with the phone" to "auto"),
            prefs.orientation,
        ) { prefs.orientation = it }
        toggle("Keep the screen on during a session", "Off: the phone's own screen timeout applies.", prefs.keepScreenOn) {
            prefs.keepScreenOn = it
        }

        section("Connection")
        label("Name computers see for this phone")
        column.addView(EditText(this).apply {
            setText(prefs.name)
            hint = prefs.shownName
            setTextColor(Color.WHITE)
            setHintTextColor(MUTED)
            setSingleLine()
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
                override fun afterTextChanged(s: Editable?) {
                    prefs.name = s.toString()
                }
            })
        })
        toggle(
            "Smoother video",
            "60 frames a second with less buffering, as the desktop's Game Boost. Uses more battery and data.",
            prefs.smoothVideo,
        ) { prefs.smoothVideo = it }
        toggle(
            "Ask before connecting on mobile data",
            "A session can use tens of megabytes a minute.",
            prefs.askOnMobileData,
        ) { prefs.askOnMobileData = it }

        section("My computers")
        note("Kept on this phone: each computer's name and device ID or address, never its access code.")
        column.addView(Button(this).apply {
            text = "Forget all computers"
            setOnClickListener {
                AlertDialog.Builder(context)
                    .setMessage("Forget every computer this phone connected to?")
                    .setPositiveButton("Forget") { _, _ ->
                        getSharedPreferences("computers", MODE_PRIVATE).edit().clear().apply()
                    }
                    .setNegativeButton("Keep", null)
                    .show()
            }
        })

        section("How to control a computer")
        note(
            "Touch: tap to click where you tap. Long press to right-click. Drag with one finger " +
                "to drag (zoomed in, it moves the view instead)."
        )
        note(
            "Touchpad: one finger moves the pointer, tap to click under it, tap with two fingers " +
                "to right-click, long press then move to drag."
        )
        note("Both: slide two fingers to scroll, pinch to zoom. Fit, under ☰, shows the whole screen again.")
        note(
            "Keyboard, under ☰, types into the computer (US keyboard layout), with a row of keys " +
                "for Esc, Tab, Ctrl, Alt, Win, Shift, arrows and F1 to F12. Ctrl, Alt, Win and Shift " +
                "hold for the next key. A mouse or keyboard plugged into the phone works as on a PC."
        )
        note("Leaving the session screen ends the session: nothing streams in the background.")

        section("About")
        note("TideDesk ${packageManager.getPackageInfo(packageName, 0).versionName}, the viewer for Android.")
        note(
            "Experimental preview: it connects with the access code only (no saved password or " +
                "trusted viewer yet), types only what a US keyboard can, and has no sound, files or chat yet."
        )
        note("Free for personal, non-commercial use under the TideDesk Personal Use Source License 1.0.")
        link("Privacy", "https://github.com/cristiangirlea/tidedesk/blob/main/docs/code-signing-policy.md#privacy")
        link("Terms of use", "https://github.com/cristiangirlea/tidedesk/blob/main/docs/terms-of-use.md")
        link("License", "https://github.com/cristiangirlea/tidedesk/blob/main/LICENSE")
        link("Releases", "https://github.com/cristiangirlea/tidedesk/releases")

        setContentView(ScrollView(this).apply {
            setBackgroundColor(ConnectActivity.BACKGROUND)
            addView(column)
        })
    }

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    private fun section(title: String) {
        column.addView(TextView(this).apply {
            text = title
            textSize = 18f
            setTypeface(typeface, Typeface.BOLD)
            setTextColor(Color.WHITE)
            setPadding(0, dp(24), 0, dp(6))
        })
    }

    private fun label(text: String) {
        column.addView(TextView(this).apply {
            this.text = text
            setTextColor(Color.WHITE)
            setPadding(0, dp(8), 0, 0)
        })
    }

    private fun note(text: String) {
        column.addView(TextView(this).apply {
            this.text = text
            setTextColor(MUTED)
            setPadding(0, dp(4), 0, dp(4))
        })
    }

    private fun toggle(title: String, detail: String?, value: Boolean, changed: (Boolean) -> Unit) {
        column.addView(Switch(this).apply {
            text = title
            isChecked = value
            setTextColor(Color.WHITE)
            setPadding(0, dp(8), 0, if (detail == null) dp(8) else 0)
            setOnCheckedChangeListener { _, on -> changed(on) }
        })
        if (detail != null) note(detail)
    }

    private fun slider(title: String, min: Int, max: Int, value: Int, changed: (Int) -> Unit) {
        label(title)
        column.addView(SeekBar(this).apply {
            this.max = max - min
            progress = value.coerceIn(min, max) - min
            setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
                override fun onProgressChanged(bar: SeekBar?, progress: Int, fromUser: Boolean) {
                    if (fromUser) changed(progress + min)
                }
                override fun onStartTrackingTouch(bar: SeekBar?) {}
                override fun onStopTrackingTouch(bar: SeekBar?) {}
            })
        })
    }

    private fun <T> choice(title: String, options: List<Pair<String, T>>, value: T, changed: (T) -> Unit) {
        label(title)
        val group = RadioGroup(this)
        for ((text, option) in options) {
            group.addView(RadioButton(this).apply {
                id = View.generateViewId()
                this.text = text
                setTextColor(Color.WHITE)
                isChecked = option == value
                setOnCheckedChangeListener { _, on -> if (on) changed(option) }
            })
        }
        column.addView(group)
    }

    private fun link(text: String, url: String) {
        column.addView(TextView(this).apply {
            this.text = text
            setTextColor(ACCENT)
            setPadding(0, dp(8), 0, dp(8))
            setOnClickListener { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) }
        })
    }

    companion object {
        val MUTED = ConnectActivity.MUTED
        val ACCENT = Color.rgb(0x33, 0xC3, 0xB0)
    }
}
