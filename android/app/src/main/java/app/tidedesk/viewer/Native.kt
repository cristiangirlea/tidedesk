package app.tidedesk.viewer

/**
 * The Rust side (crates/tidedesk-android): a session is a handle, 0 for none.
 * [connect] waits for the network, so it is never called on the main thread;
 * [nextFrame] blocks until a frame arrives and runs on the decoder's feeder
 * thread; [free] comes last, after [close] and once the feeder has stopped.
 */
object Native {
    init {
        System.loadLibrary("tidedesk_android")
    }

    /** Connects to a device ID or an address and proves the access code. */
    @JvmStatic external fun connect(target: String, code: String, name: String, dataDir: String): Long

    /** Why the last [connect] failed. */
    @JvmStatic external fun lastError(): String

    @JvmStatic external fun hostName(handle: Long): String

    /** One byte of flags (1: keyframe), then one H.264 access unit; null at the end. */
    @JvmStatic external fun nextFrame(handle: Long): ByteArray?

    /** Moves the host pointer; [x] and [y] run from 0 to 1 across its screen. */
    @JvmStatic external fun pointer(handle: Long, x: Float, y: Float)

    /** [button]: 0 left, 1 right, 2 middle. */
    @JvmStatic external fun button(handle: Long, button: Int, pressed: Boolean)

    /** Positive scrolls up. */
    @JvmStatic external fun wheel(handle: Long, notches: Int)

    @JvmStatic external fun requestKeyframe(handle: Long)

    @JvmStatic external fun close(handle: Long)

    @JvmStatic external fun free(handle: Long)
}
