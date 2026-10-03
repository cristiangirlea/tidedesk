package app.tidedesk.viewer

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import kotlin.concurrent.thread

/**
 * Where to connect: a device ID or an address, and the access code shown on
 * that computer. Every computer connected to is remembered by its name, for
 * one tap next time; access codes are not, since they change after a session.
 */
class ConnectActivity : Activity() {
    private lateinit var target: EditText
    private lateinit var code: EditText
    private lateinit var connect: Button
    private lateinit var status: TextView
    private lateinit var saved: LinearLayout
    private lateinit var computers: Computers

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        computers = Computers(getSharedPreferences("computers", MODE_PRIVATE))
        val pad = dp(24)
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad * 2, pad, pad)
            gravity = Gravity.TOP
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
        target.setText(computers.list().firstOrNull()?.target ?: "")
        code = field("Access code", InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS)
        code.imeOptions = EditorInfo.IME_ACTION_GO
        code.setOnEditorActionListener { _, _, _ -> start(); true }
        connect = Button(this).apply {
            text = "Connect"
            setOnClickListener { start() }
        }
        status = TextView(this).apply {
            setTextColor(MUTED)
            setPadding(0, pad / 2, 0, pad / 2)
        }
        listOf(target, code, connect, status).forEach(column::addView)
        column.addView(TextView(this).apply {
            text = "My computers"
            textSize = 18f
            setTextColor(Color.WHITE)
            setPadding(0, pad / 2, 0, pad / 3)
        })
        saved = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        column.addView(saved)
        showSaved()
        setContentView(ScrollView(this).apply {
            setBackgroundColor(BACKGROUND)
            addView(column)
        })
    }

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    private fun field(hint: String, type: Int) = EditText(this).apply {
        this.hint = hint
        inputType = type
        setTextColor(Color.WHITE)
        setHintTextColor(MUTED)
        setSingleLine()
    }

    /** The remembered computers as cards: tap to use, long-press to forget. */
    private fun showSaved() {
        saved.removeAllViews()
        val list = computers.list()
        if (list.isEmpty()) {
            saved.addView(TextView(this).apply {
                text = "Computers you connect to appear here."
                setTextColor(MUTED)
            })
        }
        for (computer in list) {
            saved.addView(LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(dp(14), dp(10), dp(14), dp(10))
                background = GradientDrawable().apply {
                    setColor(SURFACE)
                    cornerRadius = dp(10).toFloat()
                }
                addView(TextView(context).apply {
                    text = computer.name
                    textSize = 16f
                    setTextColor(Color.WHITE)
                })
                addView(TextView(context).apply {
                    text = computer.target
                    typeface = android.graphics.Typeface.MONOSPACE
                    setTextColor(MUTED)
                })
                setOnClickListener {
                    target.setText(computer.target)
                    code.requestFocus()
                    status.text = "Enter the access code shown on ${computer.name}."
                }
                setOnLongClickListener {
                    AlertDialog.Builder(context)
                        .setMessage("Forget ${computer.name}?")
                        .setPositiveButton("Forget") { _, _ ->
                            computers.forget(computer.target)
                            showSaved()
                        }
                        .setNegativeButton("Keep", null)
                        .show()
                    true
                }
            }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply {
                bottomMargin = dp(8)
            })
        }
    }

    private fun start() {
        val where = target.text.toString().trim()
        val secret = code.text.toString().trim()
        if (where.isEmpty()) {
            status.text = "Enter the computer's device ID or address."
            return
        }
        connect.isEnabled = false
        status.text = "Connecting to $where…"
        val name = Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME) ?: Build.MODEL
        // Connecting waits for the network: never on the main thread.
        thread(name = "connect") {
            val handle = Native.connect(where, secret, name, filesDir.absolutePath)
            val error = if (handle == 0L) Native.lastError() else null
            val host = if (handle != 0L) Native.hostName(handle) else ""
            runOnUiThread {
                connect.isEnabled = true
                if (handle == 0L) {
                    status.text = "Could not connect: $error"
                } else {
                    status.text = ""
                    code.text.clear()
                    computers.remember(host.ifEmpty { where }, where)
                    showSaved()
                    startActivity(Intent(this, SessionActivity::class.java).putExtra(SessionActivity.HANDLE, handle))
                }
            }
        }
    }

    companion object {
        val BACKGROUND = Color.rgb(0x0D, 0x15, 0x22)
        val SURFACE = Color.rgb(0x14, 0x20, 0x33)
        val MUTED = Color.rgb(0x8F, 0xA2, 0xBA)
    }
}

/**
 * The computers this phone connected to: a name and how to reach it (a
 * device ID or an address), the most recent first. Kept in the app's own
 * storage; no access code is kept.
 */
class Computers(private val store: android.content.SharedPreferences) {
    data class Computer(val name: String, val target: String)

    fun list(): List<Computer> =
        store.getString(KEY, "").orEmpty().lines().mapNotNull { line ->
            val parts = line.split('\t')
            if (parts.size == 2 && parts[1].isNotBlank()) Computer(parts[0], parts[1]) else null
        }

    /** Remembers [target] under [name], first in the list. */
    fun remember(name: String, target: String) {
        val clean = { s: String -> s.replace('\t', ' ').replace('\n', ' ').trim() }
        val rest = list().filter { !it.target.equals(target, ignoreCase = true) }
        save(listOf(Computer(clean(name), clean(target))) + rest)
    }

    fun forget(target: String) {
        save(list().filter { it.target != target })
    }

    private fun save(list: List<Computer>) {
        val text = list.take(MAX).joinToString("\n") { "${it.name}\t${it.target}" }
        store.edit().putString(KEY, text).apply()
    }

    private companion object {
        const val KEY = "computers"
        const val MAX = 20
    }
}
