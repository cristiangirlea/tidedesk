package app.tidedesk.viewer

import android.app.Activity
import android.media.MediaCodec
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewConfiguration
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import android.widget.FrameLayout
import java.util.concurrent.LinkedBlockingQueue
import kotlin.concurrent.thread
import kotlin.math.abs
import kotlin.math.hypot

/**
 * The remote screen. Battery comes first:
 *  - the phone's hardware decoder draws each frame straight onto the screen
 *    (MediaCodec onto the SurfaceView's surface), so frames never pass
 *    through the CPU;
 *  - nothing polls: the decoder says when it has a free input buffer and when
 *    a frame is ready, and the feeder thread waits for the next frame;
 *  - leaving the screen ends the session: nothing streams in the background.
 *
 * Touch: a tap clicks, a long press right-clicks, one finger dragging drags
 * with the left button, two fingers scroll.
 */
class SessionActivity : Activity(), SurfaceHolder.Callback {
    private var handle = 0L
    private lateinit var surface: SurfaceView
    private var codec: MediaCodec? = null
    private var codecThread: HandlerThread? = null
    private var feeder: Thread? = null
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
        surface = SurfaceView(this)
        surface.holder.addCallback(this)
        surface.setOnTouchListener { _, event -> touch(event); true }
        val frame = FrameLayout(this).apply { setBackgroundColor(android.graphics.Color.BLACK) }
        frame.addView(surface, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT, Gravity.CENTER))
        setContentView(frame)
        title = Native.hostName(handle)
        hideSystemBars()
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
        val parent = surface.parent as View
        val scale = minOf(parent.width.toFloat() / width, parent.height.toFloat() / height)
        surface.layoutParams = FrameLayout.LayoutParams((width * scale).toInt(), (height * scale).toInt(), Gravity.CENTER)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        stop()
    }

    // --- Touch -------------------------------------------------------------

    private val ui = Handler(Looper.getMainLooper())
    private val slop by lazy { ViewConfiguration.get(this).scaledTouchSlop }
    private var downX = 0f
    private var downY = 0f
    private var dragging = false
    private var twoFingers = false
    private var longPressed = false
    private var scrolled = 0f
    private var lastY = 0f
    private val rightClick = Runnable {
        longPressed = true
        click(downX, downY, RIGHT)
    }

    private fun touch(event: MotionEvent) {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                downX = event.x
                downY = event.y
                dragging = false
                twoFingers = false
                longPressed = false
                ui.postDelayed(rightClick, ViewConfiguration.getLongPressTimeout().toLong())
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                ui.removeCallbacks(rightClick)
                twoFingers = true
                scrolled = 0f
                lastY = averageY(event)
            }
            MotionEvent.ACTION_MOVE -> when {
                twoFingers -> {
                    val y = averageY(event)
                    scrolled += y - lastY
                    lastY = y
                    val notches = (scrolled / SCROLL_STEP).toInt()
                    if (notches != 0) {
                        // Swiping up scrolls down, as on the phone.
                        Native.wheel(handle, notches)
                        scrolled -= notches * SCROLL_STEP
                    }
                }
                longPressed -> {}
                dragging -> move(event.x, event.y)
                hypot(event.x - downX, event.y - downY) > slop -> {
                    ui.removeCallbacks(rightClick)
                    dragging = true
                    move(downX, downY)
                    Native.button(handle, LEFT, true)
                    move(event.x, event.y)
                }
            }
            MotionEvent.ACTION_UP -> {
                ui.removeCallbacks(rightClick)
                when {
                    dragging -> {
                        move(event.x, event.y)
                        Native.button(handle, LEFT, false)
                    }
                    !twoFingers && !longPressed -> click(event.x, event.y, LEFT)
                }
            }
            MotionEvent.ACTION_CANCEL -> {
                ui.removeCallbacks(rightClick)
                if (dragging) Native.button(handle, LEFT, false)
            }
        }
    }

    private fun averageY(event: MotionEvent): Float {
        var sum = 0f
        for (i in 0 until event.pointerCount) sum += event.getY(i)
        return sum / event.pointerCount
    }

    /** Where a touch on the surface lands on the remote screen, 0 to 1. */
    private fun move(x: Float, y: Float) {
        val fx = (x / surface.width).coerceIn(0f, 1f)
        val fy = (y / surface.height).coerceIn(0f, 1f)
        Native.pointer(handle, fx, fy)
    }

    private fun click(x: Float, y: Float, button: Int) {
        move(x, y)
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
        Native.close(handle)
        feeder?.interrupt()
        feeder?.join(1000)
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
        /** Room for a large keyframe from a 4K screen. */
        private const val MAX_FRAME = 8 * 1024 * 1024
        private const val LEFT = 0
        private const val RIGHT = 1
        /** Pixels of two-finger movement per scroll notch. */
        private const val SCROLL_STEP = 40f
    }
}
