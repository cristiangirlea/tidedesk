package app.tidedesk.viewer

import android.view.KeyEvent

/**
 * Keys as the host wants them: PC/AT set-1 scancodes, with 0xE0 in the high
 * byte for extended keys. The host types them with its own keyboard layout,
 * so characters are mapped as on a US keyboard; characters with no key there
 * (such as accented letters) cannot be typed yet.
 */
object Keys {
    const val SHIFT = 0x2A
    const val CTRL = 0x1D
    const val ALT = 0x38
    const val WIN = 0xE05B
    const val ESC = 0x01
    const val TAB = 0x0F
    const val ENTER = 0x1C
    const val BACKSPACE = 0x0E
    const val DELETE = 0xE053
    const val HOME = 0xE047
    const val END = 0xE04F
    const val PAGE_UP = 0xE049
    const val PAGE_DOWN = 0xE051
    const val UP = 0xE048
    const val DOWN = 0xE050
    const val LEFT = 0xE04B
    const val RIGHT = 0xE04D

    /** F1 to F12. */
    val F = intArrayOf(0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58)

    /** A hardware keyboard's key, or 0 when the host has no such key. */
    fun scancode(keyCode: Int): Int = when (keyCode) {
        in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> LETTERS[keyCode - KeyEvent.KEYCODE_A]
        in KeyEvent.KEYCODE_0..KeyEvent.KEYCODE_9 -> if (keyCode == KeyEvent.KEYCODE_0) 0x0B else 0x02 + keyCode - KeyEvent.KEYCODE_1
        in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> F[keyCode - KeyEvent.KEYCODE_F1]
        KeyEvent.KEYCODE_ESCAPE -> ESC
        KeyEvent.KEYCODE_TAB -> TAB
        KeyEvent.KEYCODE_ENTER -> ENTER
        KeyEvent.KEYCODE_NUMPAD_ENTER -> 0xE01C
        KeyEvent.KEYCODE_DEL -> BACKSPACE
        KeyEvent.KEYCODE_FORWARD_DEL -> DELETE
        KeyEvent.KEYCODE_SPACE -> 0x39
        KeyEvent.KEYCODE_MINUS -> 0x0C
        KeyEvent.KEYCODE_EQUALS -> 0x0D
        KeyEvent.KEYCODE_LEFT_BRACKET -> 0x1A
        KeyEvent.KEYCODE_RIGHT_BRACKET -> 0x1B
        KeyEvent.KEYCODE_BACKSLASH -> 0x2B
        KeyEvent.KEYCODE_SEMICOLON -> 0x27
        KeyEvent.KEYCODE_APOSTROPHE -> 0x28
        KeyEvent.KEYCODE_GRAVE -> 0x29
        KeyEvent.KEYCODE_COMMA -> 0x33
        KeyEvent.KEYCODE_PERIOD -> 0x34
        KeyEvent.KEYCODE_SLASH -> 0x35
        KeyEvent.KEYCODE_SHIFT_LEFT -> SHIFT
        KeyEvent.KEYCODE_SHIFT_RIGHT -> 0x36
        KeyEvent.KEYCODE_CTRL_LEFT -> CTRL
        KeyEvent.KEYCODE_CTRL_RIGHT -> 0xE01D
        KeyEvent.KEYCODE_ALT_LEFT -> ALT
        KeyEvent.KEYCODE_ALT_RIGHT -> 0xE038
        KeyEvent.KEYCODE_META_LEFT -> WIN
        KeyEvent.KEYCODE_META_RIGHT -> 0xE05C
        KeyEvent.KEYCODE_MENU -> 0xE05D
        KeyEvent.KEYCODE_CAPS_LOCK -> 0x3A
        KeyEvent.KEYCODE_NUM_LOCK -> 0x45
        KeyEvent.KEYCODE_SCROLL_LOCK -> 0x46
        KeyEvent.KEYCODE_SYSRQ -> 0xE037
        KeyEvent.KEYCODE_BREAK -> 0xE046
        KeyEvent.KEYCODE_INSERT -> 0xE052
        KeyEvent.KEYCODE_MOVE_HOME -> HOME
        KeyEvent.KEYCODE_MOVE_END -> END
        KeyEvent.KEYCODE_PAGE_UP -> PAGE_UP
        KeyEvent.KEYCODE_PAGE_DOWN -> PAGE_DOWN
        KeyEvent.KEYCODE_DPAD_UP -> UP
        KeyEvent.KEYCODE_DPAD_DOWN -> DOWN
        KeyEvent.KEYCODE_DPAD_LEFT -> LEFT
        KeyEvent.KEYCODE_DPAD_RIGHT -> RIGHT
        in KeyEvent.KEYCODE_NUMPAD_0..KeyEvent.KEYCODE_NUMPAD_9 -> NUMPAD[keyCode - KeyEvent.KEYCODE_NUMPAD_0]
        KeyEvent.KEYCODE_NUMPAD_DIVIDE -> 0xE035
        KeyEvent.KEYCODE_NUMPAD_MULTIPLY -> 0x37
        KeyEvent.KEYCODE_NUMPAD_SUBTRACT -> 0x4A
        KeyEvent.KEYCODE_NUMPAD_ADD -> 0x4E
        KeyEvent.KEYCODE_NUMPAD_DOT -> 0x53
        else -> 0
    }

    /** The key, and whether Shift goes with it, that types [c]; null when none does. */
    fun forChar(c: Char): Pair<Int, Boolean>? {
        if (c in 'a'..'z') return LETTERS[c - 'a'] to false
        if (c in 'A'..'Z') return LETTERS[c - 'A'] to true
        if (c in '1'..'9') return (0x02 + (c - '1')) to false
        val plain = "0-=[]\\;',./` \n\t"
        val plainCodes = intArrayOf(0x0B, 0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x27, 0x28, 0x33, 0x34, 0x35, 0x29, 0x39, ENTER, TAB)
        plain.indexOf(c).takeIf { it >= 0 }?.let { return plainCodes[it] to false }
        val shifted = ")!@#$%^&*(_+{}|:\"<>?~"
        val shiftedCodes = intArrayOf(0x0B, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0C, 0x0D, 0x1A, 0x1B, 0x2B, 0x27, 0x28, 0x33, 0x34, 0x35, 0x29)
        shifted.indexOf(c).takeIf { it >= 0 }?.let { return shiftedCodes[it] to true }
        return null
    }

    /** A to Z on a US keyboard. */
    private val LETTERS = intArrayOf(
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32,
        0x31, 0x18, 0x19, 0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    )

    /** Keypad 0 to 9. */
    private val NUMPAD = intArrayOf(0x52, 0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49)
}
