# Test control

For tests across two computers. With `--control` the viewer takes commands on its
standard input, one to a line, and answers each on its standard output, in the order
they came, with a line that starts with `ok` or `error`. Its log goes to standard
error. The window opens as always and can be used meanwhile.

```
tidedesk view 192.0.2.7 --code K7QM-3XPA-WZ --control
```

The option is not listed in `--help`: it is for tests, not for daily use.

## Commands

Places are pixels of the remote screen, as in the decoded picture: `0 0` is its top
left corner, whatever the window's size and place.

| Command | What it does | Answer |
| --- | --- | --- |
| `size` | | `ok 2560x1440` |
| `frames` | Pictures put up for the window so far. Game Boost leaves out pictures that a newer one has overtaken. | `ok 82 frames, the last 30 ms ago` |
| `stats` | | `ok 2560x1440, 82 frames, the last 30 ms ago, rtt 0.8 ms, 0 packets lost` |
| `crop X Y WIDTH HEIGHT FILE [SCALE]` | Saves that area of the decoded picture as a PNG file. | `ok 640x360 FILE` |
| `move X Y` | Moves the host's pointer there. | `ok move 10 20` |
| `click X Y [left\|right\|middle]` | Moves the pointer there, presses the button and lets go. | `ok click 10 20` |
| `press X Y [BUTTON]` | Presses without letting go, to drag with `move`. | `ok press 10 20` |
| `release [BUTTON]` | | `ok release` |
| `wheel LINES` | Turns the wheel up, or down with a negative number, by 100 lines at most. | `ok wheel -3` |
| `key SCANCODE [down\|up]` | Presses the key and lets go, or one of the two. | `ok key E04D` |
| `type TEXT` | Types the text, all that follows on the line. | `ok type 11 characters` |
| `quit` | Ends the session and the viewer. | `ok quit` |

**Crops** are the picture's own pixels, as the viewer decoded them, not what the
window shows. `SCALE` is a whole number from 2 to 8 to make each pixel that many
times as wide and high (repeated, not blended: small text can be looked at closely),
or `1/2` to `1/8` to make the picture smaller (each pixel the mean of those it stands
for: an overview of a whole screen). A crop has 59 million pixels at most: enlarge a
smaller area. A file name with spaces goes in double quotes.

**Keys** go by hardware scan code, in hexadecimal, with `E0` in front for extended
keys: `1E` is A on a United States keyboard, `1C` Enter, `E04D` the right arrow,
`2A` the left Shift. What they type is decided by the host's keyboard layout.
`type` uses the keys that make each character on the viewer's layout, so the text
comes out the same where the host's layout is the same; a character that no key
makes is an error, and nothing of the text is typed.

**Mouse commands** need mouse control to be allowed, in the viewer's settings and at
the host, as the mouse itself does. Each asks the host where its pointer is first,
as the host takes mouse events only with what it answers. If it does not answer
within two seconds the command fails. They do not move the pointer of the computer
the viewer runs on.

## What it cannot do

Nothing that a person at the viewer could not do: the commands go to the host the
viewer is connected to, with the access code it was started with.

## With one computer

Host and viewer can run on one computer to try `size`, `frames`, `stats` and `crop`.
Mouse and key commands would reach that computer's own desktop: leave them for two.
