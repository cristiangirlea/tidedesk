package app.tidedesk.viewer

import android.app.Activity
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.drawable.GradientDrawable
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.text.InputType
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewConfiguration
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import android.widget.FrameLayout
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import java.util.concurrent.LinkedBlockingQueue
import kotlin.concurrent.thread
import kotlin.math.hypot

/**
 * The remote screen. Battery comes first:
 *  - the phone's hardware decoder draws each frame straight onto the screen
 *    (MediaCodec onto the SurfaceView's surface), so frames never pass
 *    through the CPU;
 *  - nothing polls: the decoder says when it has a free input buffer and when
 *    a frame is ready; one thread waits for the next frame and one for the
 *    next cursor move;
 *  - leaving the screen ends the session: nothing streams in the background.
 *
 * The host's mouse pointer is not in the video, so the phone draws it.
 *
 * Touch, directly: a tap clicks where it lands, a long press right-clicks,
 * one finger dragging drags with the left button, two fingers scroll.
 * As a touchpad: one finger moves the pointer, a tap clicks under it, a
 * two-finger tap right-clicks, a long press then moving drags, two fingers
 * scroll. A mouse and a keyboard plugged into the phone work as on a PC.
 */
class SessionActivity : Activity(), SurfaceHolder.Callback {
    private var handle = 0L
    private lateinit var root: FrameLayout
    private lateinit var surface: SurfaceView
    private lateinit var pointerView: PointerView
    private lateinit var keyCatcher: KeyCatcher
    private lateinit var keysRow: View
    private lateinit var modeButton: TextView
    private var codec: MediaCodec? = null
    private var codecThread: HandlerThread? = null
    private var feeder: Thread? = null
    private var cursorWatcher: Thread? = null
    private val freeInputs = LinkedBlockingQueue<Int>()
    @Volatile private var running = true
    /** After a decoder error, frames wait for the next keyframe. */
    @Volatile private var waitingForKeyframe = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handle = intent.getLongExtra(HANDLE, 0L)
        if (handle == 0L) {
            finish()
            return
        }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= 30) window.setDecorFitsSystemWindows(false)

        root = FrameLayout(this).apply { setBackgroundColor(Color.BLACK) }
        surface = SurfaceView(this)
        surface.holder.addCallback(this)
        surface.setOnTouchListener { _, event -> touch(event); true }
        surface.setOnGenericMotionListener { _, event -> genericMotion(event) }
        root.addView(surface, FrameLayout.LayoutParams(MATCH, MATCH, Gravity.CENTER))

        pointerView = PointerView(this)
        root.addView(pointerView, FrameLayout.LayoutParams(dp(24), dp(24)))

        keyCatcher = KeyCatcher(this)
        root.addView(keyCatcher, FrameLayout.LayoutParams(1, 1))

        keysRow = specialKeys()
        keysRow.visibility = View.GONE
        root.addView(keysRow, FrameLayout.LayoutParams(MATCH, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM))

        root.addView(toolbar(), FrameLayout.LayoutParams(FrameLayout.LayoutParams.WRAP_CONTENT, FrameLayout.LayoutParams.WRAP_CONTENT, Gravity.TOP or Gravity.END).apply {
            setMargins(0, dp(8), dp(8), 0)
        })
        // The special keys sit just above the phone's keyboard.
        root.setOnApplyWindowInsetsListener { _, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val ime = insets.getInsets(WindowInsets.Type.ime()).bottom
                keysRow.translationY = -ime.toFloat()
                keysRow.visibility = if (ime > 0) View.VISIBLE else View.GONE
            }
            insets
        }
        setContentView(root)
        title = Native.hostName(handle)
        hideSystemBars()
        cursorWatcher = thread(name = "cursor") { watchCursor() }
    }

    private fun hideSystemBars() {
        if (Build.VERSION.SDK_INT >= 30) {
            window.insetsController?.let {
                it.hide(WindowInsets.Type.systemBars())
                it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            }
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = View.SYSTEM_UI_FLAG_FULLSCREEN or
                View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
        }
    }

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    // --- Controls ----------------------------------------------------------

    private fun chip(label: String, onClick: () -> Unit) = TextView(this).apply {
        text = label
        textSize = 14f
        setTextColor(Color.WHITE)
        setPadding(dp(12), dp(8), dp(12), dp(8))
        background = GradientDrawable().apply {
            setColor(Color.argb(170, 0x14, 0x20, 0x33))
            cornerRadius = dp(8).toFloat()
        }
        setOnClickListener {
            it.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
            onClick()
        }
    }

    /** Keyboard, touch or touchpad, and disconnect. */
    private fun toolbar(): View {
        val bar = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        val gap = { v: View -> bar.addView(v, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(6) }) }
        gap(chip("Keyboard") { toggleKeyboard() })
        modeButton = chip("Touch") { toggleMode() }
        gap(modeButton)
        gap(chip("Disconnect") { finish() })
        return bar
    }

    /** Esc, Tab, the modifiers, arrows and the rest, above the phone's keyboard. */
    private fun specialKeys(): View {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setPadding(dp(4), dp(4), dp(4), dp(4))
            setBackgroundColor(Color.argb(230, 0x0D, 0x15, 0x22))
        }
        val add = { v: View -> row.addView(v, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginEnd = dp(4) }) }
        for ((label, code) in listOf("Ctrl" to Keys.CTRL, "Alt" to Keys.ALT, "Win" to Keys.WIN, "Shift" to Keys.SHIFT)) {
            lateinit var button: TextView
            button = chip(label) { toggleModifier(code, button) }
            add(button)
        }
        for ((label, code) in listOf(
            "Esc" to Keys.ESC, "Tab" to Keys.TAB, "←" to Keys.LEFT, "↑" to Keys.UP, "↓" to Keys.DOWN,
            "→" to Keys.RIGHT, "Home" to Keys.HOME, "End" to Keys.END, "PgUp" to Keys.PAGE_UP,
            "PgDn" to Keys.PAGE_DOWN, "Del" to Keys.DELETE,
        )) add(chip(label) { press(code) })
        Keys.F.forEachIndexed { i, code -> add(chip("F${i + 1}") { press(code) }) }
        return HorizontalScrollView(this).apply {
            isHorizontalScrollBarEnabled = false
            addView(row)
        }
    }

    private fun toggleKeyboard() {
        val input = getSystemService(Context.INPUT_METHOD_SERVICE) as InputMethodManager
        if (keysRow.visibility == View.VISIBLE) {
            input.hideSoftInputFromWindow(keyCatcher.windowToken, 0)
        } else {
            keyCatcher.requestFocus()
            input.showSoftInput(keyCatcher, InputMethodManager.SHOW_IMPLICIT)
            if (Build.VERSION.SDK_INT < 30) keysRow.visibility = View.VISIBLE
        }
    }

    // --- Keys --------------------------------------------------------------

    /** Ctrl, Alt, Win and Shift from the row: held for the next key only. */
    private val heldModifiers = mutableMapOf<Int, TextView>()

    private fun toggleModifier(code: Int, button: TextView) {
        if (heldModifiers.remove(code) != null) {
            Native.key(handle, code, false)
            button.alpha = 1f
        } else {
            Native.key(handle, code, true)
            heldModifiers[code] = button
            button.alpha = 0.55f
        }
    }

    private fun releaseModifiers() {
        for ((code, button) in heldModifiers) {
            Native.key(handle, code, false)
            button.alpha = 1f
        }
        heldModifiers.clear()
    }

    /** One key, pressed and released, then the held modifiers let go. */
    private fun press(code: Int, shift: Boolean = false) {
        if (shift) Native.key(handle, Keys.SHIFT, true)
        Native.key(handle, code, true)
        Native.key(handle, code, false)
        if (shift) Native.key(handle, Keys.SHIFT, false)
        releaseModifiers()
    }

    /** Text from the phone's keyboard, as the keys that type it. */
    private fun type(text: CharSequence) {
        for (c in text) Keys.forChar(c)?.let { (code, shift) -> press(code, shift) }
    }

    /** A keyboard plugged into the phone: every key goes to the host. */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        val code = Keys.scancode(event.keyCode)
        val physical = event.deviceId != KeyCharacterMap.VIRTUAL_KEYBOARD && event.isFromSource(InputDevice.SOURCE_KEYBOARD)
        if (code == 0 || !physical || event.keyCode == KeyEvent.KEYCODE_BACK) return super.dispatchKeyEvent(event)
        when (event.action) {
            KeyEvent.ACTION_DOWN -> Native.key(handle, code, true)
            KeyEvent.ACTION_UP -> Native.key(handle, code, false)
        }
        return true
    }

    /**
     * Takes what the phone's keyboard types. As a password field: no
     * suggestions and no half-written words, so every key arrives at once.
     */
    private inner class KeyCatcher(context: Context) : View(context) {
        init {
            isFocusable = true
            isFocusableInTouchMode = true
        }

        override fun onCheckIsTextEditor() = true

        override fun onCreateInputConnection(info: EditorInfo): InputConnection {
            info.inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD or
                InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_FLAG_NO_EXTRACT_UI or
                EditorInfo.IME_ACTION_NONE
            return object : BaseInputConnection(this, false) {
                private var composing: CharSequence = ""

                override fun commitText(text: CharSequence, newCursorPosition: Int): Boolean {
                    composing = ""
                    type(text)
                    return true
                }

                override fun setComposingText(text: CharSequence, newCursorPosition: Int): Boolean {
                    composing = text
                    return true
                }

                override fun finishComposingText(): Boolean {
                    type(composing)
                    composing = ""
                    return true
                }

                override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                    repeat(beforeLength.coerceAtMost(64)) { press(Keys.BACKSPACE) }
                    repeat(afterLength.coerceAtMost(64)) { press(Keys.DELETE) }
                    return true
                }

                override fun sendKeyEvent(event: KeyEvent): Boolean {
                    // Enter, Backspace and the like from the phone's keyboard.
                    if (event.action == KeyEvent.ACTION_DOWN) {
                        val code = Keys.scancode(event.keyCode)
                        if (code != 0) press(code)
                    }
                    return true
                }
            }
        }
    }

    // --- Video -------------------------------------------------------------

    override fun surfaceCreated(holder: SurfaceHolder) {
        val thread = HandlerThread("decoder").also { it.start() }
        codecThread = thread
        val decoder = MediaCodec.createDecoderByType(MIME)
        decoder.setCallback(object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {
                freeInputs.put(index)
            }

            override fun onOutputBufferAvailable(codec: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                // Straight onto the screen.
                codec.releaseOutputBuffer(index, true)
            }

            override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {
                val width = format.getInteger(MediaFormat.KEY_WIDTH)
                val height = format.getInteger(MediaFormat.KEY_HEIGHT)
                runOnUiThread { fit(width, height) }
            }

            override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
                waitingForKeyframe = true
                Native.requestKeyframe(handle)
            }
        }, Handler(thread.looper))
        // The real size comes with the first keyframe; this only reserves room.
        val format = MediaFormat.createVideoFormat(MIME, 1920, 1080)
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, MAX_FRAME)
        if (Build.VERSION.SDK_INT >= 30) format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        decoder.configure(format, holder.surface, null, 0)
        decoder.start()
        codec = decoder
        feeder = thread(name = "frames") { feed(decoder) }
    }

    /** Hands each frame to the decoder as soon as it has room for it. */
    private fun feed(decoder: MediaCodec) {
        val started = System.nanoTime()
        while (running) {
            val frame = Native.nextFrame(handle) ?: break
            val keyframe = frame[0].toInt() and 1 == 1
            if (waitingForKeyframe && !keyframe) continue
            waitingForKeyframe = false
            val index = try {
                freeInputs.take()
            } catch (_: InterruptedException) {
                break
            }
            val size = frame.size - 1
            val buffer = decoder.getInputBuffer(index) ?: continue
            if (size > buffer.capacity()) {
                // Too large for this decoder: drop it, and start again from a keyframe.
                decoder.queueInputBuffer(index, 0, 0, 0, 0)
                waitingForKeyframe = true
                Native.requestKeyframe(handle)
                continue
            }
            buffer.clear()
            buffer.put(frame, 1, size)
            val flags = if (keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
            try {
                decoder.queueInputBuffer(index, 0, size, (System.nanoTime() - started) / 1000, flags)
            } catch (_: IllegalStateException) {
                break // the decoder was stopped
            }
        }
        // The host ended the session, or this screen closed.
        if (running) runOnUiThread { finish() }
    }

    /** Letterboxes the surface to the remote screen's shape. */
    private fun fit(width: Int, height: Int) {
        val scale = minOf(root.width.toFloat() / width, root.height.toFloat() / height)
        surface.layoutParams = FrameLayout.LayoutParams((width * scale).toInt(), (height * scale).toInt(), Gravity.CENTER)
        surface.post { drawPointer() }
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        stop()
    }

    // --- The host's pointer --------------------------------------------------

    /** Where the pointer is drawn, 0 to 1 across the screen. */
    private var pointerX = 0.5f
    private var pointerY = 0.5f
    private var pointerShown = false
    /** While a finger drives the pointer, the phone's own position wins. */
    private var steering = false

    /** Waits for the host's pointer to move, and draws it where it went. */
    private fun watchCursor() {
        while (running) {
            val cursor = Native.nextCursor(handle)
            if (cursor == -1L) break
            val visible = cursor shr 32 and 1L == 1L
            val x = (cursor shr 16 and 0xFFFF).toFloat() / 65535f
            val y = (cursor and 0xFFFF).toFloat() / 65535f
            runOnUiThread {
                if (!steering) {
                    pointerX = x
                    pointerY = y
                }
                pointerShown = visible
                drawPointer()
            }
        }
    }

    private fun drawPointer() {
        pointerView.visibility = if (pointerShown && surface.width > 0) View.VISIBLE else View.INVISIBLE
        pointerView.translationX = surface.x + pointerX * surface.width
        pointerView.translationY = surface.y + pointerY * surface.height
    }

    /** An arrow, white with a dark edge, its tip at the view's corner. */
    private class PointerView(context: Context) : View(context) {
        private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
        private val edge = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = Color.BLACK
            style = Paint.Style.STROKE
            strokeWidth = 2f
        }

        override fun onDraw(canvas: Canvas) {
            val s = width / 24f
            val arrow = Path().apply {
                moveTo(1 * s, 1 * s)
                lineTo(1 * s, 19 * s)
                lineTo(6 * s, 14 * s)
                lineTo(10 * s, 22 * s)
                lineTo(13 * s, 21 * s)
                lineTo(9 * s, 13 * s)
                lineTo(16 * s, 13 * s)
                close()
            }
            canvas.drawPath(arrow, fill)
            canvas.drawPath(arrow, edge)
        }
    }

    // --- Touch and mouse -----------------------------------------------------

    private var touchpad = false

    private fun toggleMode() {
        touchpad = !touchpad
        modeButton.text = if (touchpad) "Touchpad" else "Touch"
    }

    private val ui = Handler(Looper.getMainLooper())
    private val slop by lazy { ViewConfiguration.get(this).scaledTouchSlop }
    private var downX = 0f
    private var downY = 0f
    private var lastX = 0f
    private var lastY = 0f
    private var moved = false
    private var dragging = false
    private var twoFingers = false
    private var twoMoved = false
    private var longPressed = false
    private var scrolled = 0f
    private val longPress = Runnable {
        longPressed = true
        surface.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
        if (touchpad) {
            // A long press, then moving, drags.
            dragging = true
            Native.button(handle, LEFT, true)
        } else {
            click(downX, downY, RIGHT)
        }
    }

    private fun touch(event: MotionEvent) {
        if (event.isFromSource(InputDevice.SOURCE_MOUSE)) {
            mouse(event)
            return
        }
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = event.x
                downY = event.y
                lastX = event.x
                lastY = event.y
                moved = false
                dragging = false
                twoFingers = false
                twoMoved = false
                longPressed = false
                steering = touchpad
                ui.postDelayed(longPress, ViewConfiguration.getLongPressTimeout().toLong())
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                ui.removeCallbacks(longPress)
                twoFingers = true
                scrolled = 0f
                lastY = averageY(event)
            }
            MotionEvent.ACTION_MOVE -> when {
                twoFingers -> scroll(event)
                touchpad -> pad(event)
                longPressed -> {}
                dragging -> move(event.x, event.y)
                hypot(event.x - downX, event.y - downY) > slop -> {
                    ui.removeCallbacks(longPress)
                    dragging = true
                    move(downX, downY)
                    Native.button(handle, LEFT, true)
                    move(event.x, event.y)
                }
            }
            MotionEvent.ACTION_UP -> {
                ui.removeCallbacks(longPress)
                when {
                    dragging -> {
                        if (!touchpad) move(event.x, event.y)
                        Native.button(handle, LEFT, false)
                    }
                    twoFingers && !twoMoved -> clickAtPointer(RIGHT)
                    twoFingers || longPressed -> {}
                    touchpad && !moved -> clickAtPointer(LEFT)
                    !touchpad -> click(event.x, event.y, LEFT)
                }
                steering = false
            }
            MotionEvent.ACTION_CANCEL -> {
                ui.removeCallbacks(longPress)
                if (dragging) Native.button(handle, LEFT, false)
                steering = false
            }
        }
    }

    /** Touchpad: the finger moves the pointer by how far it went. */
    private fun pad(event: MotionEvent) {
        val dx = event.x - lastX
        val dy = event.y - lastY
        lastX = event.x
        lastY = event.y
        if (!moved && hypot(event.x - downX, event.y - downY) <= slop) return
        if (!moved) {
            moved = true
            if (!dragging) ui.removeCallbacks(longPress)
        }
        pointerX = (pointerX + dx / surface.width * PAD_SPEED).coerceIn(0f, 1f)
        pointerY = (pointerY + dy / surface.height * PAD_SPEED).coerceIn(0f, 1f)
        Native.pointer(handle, pointerX, pointerY)
        drawPointer()
    }

    private fun scroll(event: MotionEvent) {
        val y = averageY(event)
        scrolled += y - lastY
        lastY = y
        if (kotlin.math.abs(scrolled) > slop) twoMoved = true
        val notches = (scrolled / SCROLL_STEP).toInt()
        if (notches != 0) {
            // Swiping up scrolls down, as on the phone.
            Native.wheel(handle, notches)
            scrolled -= notches * SCROLL_STEP
        }
    }

    /** A mouse plugged into the phone: hovering moves, buttons and wheel as on a PC. */
    private fun genericMotion(event: MotionEvent): Boolean {
        if (!event.isFromSource(InputDevice.SOURCE_MOUSE)) return false
        mouse(event)
        return true
    }

    private var mouseButtons = 0
    private var wheel = 0f

    private fun mouse(event: MotionEvent) {
        if (event.actionMasked == MotionEvent.ACTION_SCROLL) {
            wheel += event.getAxisValue(MotionEvent.AXIS_VSCROLL)
            val notches = wheel.toInt()
            if (notches != 0) {
                Native.wheel(handle, notches)
                wheel -= notches
            }
            return
        }
        move(event.x, event.y)
        val buttons = event.buttonState
        for ((mask, button) in MOUSE_BUTTONS) {
            val now = buttons and mask != 0
            if (now != (mouseButtons and mask != 0)) Native.button(handle, button, now)
        }
        mouseButtons = buttons
    }

    private fun averageY(event: MotionEvent): Float {
        var sum = 0f
        for (i in 0 until event.pointerCount) sum += event.getY(i)
        return sum / event.pointerCount
    }

    /** Where a touch on the surface lands on the remote screen, 0 to 1. */
    private fun move(x: Float, y: Float) {
        pointerX = (x / surface.width).coerceIn(0f, 1f)
        pointerY = (y / surface.height).coerceIn(0f, 1f)
        Native.pointer(handle, pointerX, pointerY)
        drawPointer()
    }

    private fun click(x: Float, y: Float, button: Int) {
        move(x, y)
        Native.button(handle, button, true)
        Native.button(handle, button, false)
    }

    private fun clickAtPointer(button: Int) {
        Native.pointer(handle, pointerX, pointerY)
        Native.button(handle, button, true)
        Native.button(handle, button, false)
    }

    // --- End ---------------------------------------------------------------

    override fun onStop() {
        super.onStop()
        // Nothing streams in the background.
        finish()
    }

    private fun stop() {
        if (!running) return
        running = false
        releaseModifiers()
        Native.close(handle)
        feeder?.interrupt()
        feeder?.join(1000)
        cursorWatcher?.join(1000)
        codec?.let {
            try {
                it.stop()
            } catch (_: IllegalStateException) {
            }
            it.release()
        }
        codec = null
        codecThread?.quitSafely()
    }

    override fun onDestroy() {
        stop()
        if (handle != 0L) {
            Native.free(handle)
            handle = 0L
        }
        super.onDestroy()
    }

    companion object {
        const val HANDLE = "handle"
        private const val MIME = "video/avc"
        private const val MATCH = FrameLayout.LayoutParams.MATCH_PARENT
        /** Room for a large keyframe from a 4K screen. */
        private const val MAX_FRAME = 8 * 1024 * 1024
        private const val LEFT = 0
        private const val RIGHT = 1
        private const val MIDDLE = 2
        /** Pixels of two-finger movement per scroll notch. */
        private const val SCROLL_STEP = 40f
        /** How far the pointer goes for a finger's movement on the touchpad. */
        private const val PAD_SPEED = 1.6f
        private val MOUSE_BUTTONS = listOf(
            MotionEvent.BUTTON_PRIMARY to LEFT,
            MotionEvent.BUTTON_SECONDARY to RIGHT,
            MotionEvent.BUTTON_TERTIARY to MIDDLE,
        )
    }
}
