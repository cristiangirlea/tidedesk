package app.tidedesk.viewer

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import kotlin.concurrent.thread

/**
 * Where to connect: a device ID or an address, and the access code shown on
 * that computer. The last computer is remembered; the code never is.
 */
class ConnectActivity : Activity() {
    private lateinit var target: EditText
    private lateinit var code: EditText
    private lateinit var connect: Button
    private lateinit var status: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val pad = (24 * resources.displayMetrics.density).toInt()
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad * 2, pad, pad)
            setBackgroundColor(BACKGROUND)
        }
        column.addView(TextView(this).apply {
            text = "Connect to a computer"
            textSize = 24f
            setTextColor(Color.WHITE)
        })
        column.addView(TextView(this).apply {
            text = "Experimental preview: things may change or not work yet."
            setTextColor(MUTED)
            setPadding(0, 0, 0, pad / 2)
        })
        target = field("Device ID or address", InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS)
        target.setText(getPreferences(MODE_PRIVATE).getString(LAST_TARGET, ""))
        code = field("Access code", InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS)
        code.imeOptions = EditorInfo.IME_ACTION_GO
        code.setOnEditorActionListener { _, _, _ -> start(); true }
        connect = Button(this).apply {
            text = "Connect"
            setOnClickListener { start() }
        }
        status = TextView(this).apply {
            setTextColor(MUTED)
            setPadding(0, pad / 2, 0, 0)
        }
        listOf(target, code, connect, status).forEach(column::addView)
        column.gravity = Gravity.TOP
        setContentView(column)
    }

    private fun field(hint: String, type: Int) = EditText(this).apply {
        this.hint = hint
        inputType = type
        setTextColor(Color.WHITE)
        setHintTextColor(MUTED)
        setSingleLine()
    }

    private fun start() {
        val where = target.text.toString().trim()
        val secret = code.text.toString().trim()
        if (where.isEmpty()) {
            status.text = "Enter the computer's device ID or address."
            return
        }
        getPreferences(MODE_PRIVATE).edit().putString(LAST_TARGET, where).apply()
        connect.isEnabled = false
        status.text = "Connecting to $where…"
        val name = Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME) ?: Build.MODEL
        // Connecting waits for the network: never on the main thread.
        thread(name = "connect") {
            val handle = Native.connect(where, secret, name, filesDir.absolutePath)
            val error = if (handle == 0L) Native.lastError() else null
            runOnUiThread {
                connect.isEnabled = true
                if (handle == 0L) {
                    status.text = "Could not connect: $error"
                } else {
                    status.text = ""
                    code.text.clear()
                    startActivity(Intent(this, SessionActivity::class.java).putExtra(SessionActivity.HANDLE, handle))
                }
            }
        }
    }

    companion object {
        private const val LAST_TARGET = "last-target"
        val BACKGROUND = Color.rgb(0x0D, 0x15, 0x22)
        val MUTED = Color.rgb(0x8F, 0xA2, 0xBA)
    }
}
