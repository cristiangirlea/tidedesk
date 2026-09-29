//! Test control: the viewer driven by lines of text, for tests across two
//! computers.
//!
//! Such tests used to drive the viewer's window from outside, with
//! screenshots and synthetic clicks and keys, which tells little exactly:
//! the window scales the picture, and its place on the screen changes. With
//! `--control` the viewer takes commands on its standard input, one to a
//! line, and answers each on its standard output with a line that starts
//! with `ok` or `error`:
//!
//! ```text
//! size                              the remote screen's size
//! frames                            pictures put up so far, and the last one's age
//! stats                             the above and the network path
//! crop X Y WIDTH HEIGHT FILE        that area of the decoded picture, as a PNG
//! crop X Y WIDTH HEIGHT FILE 3      each pixel three times as wide and high
//! crop X Y WIDTH HEIGHT FILE 1/2    half as wide and high, two by two pixels as one
//! move X Y                          the host's pointer to that pixel of its screen
//! click X Y [left|right|middle]     the pointer there, the button down and up
//! press X Y [BUTTON]                down only, to drag with move
//! release [BUTTON]
//! wheel LINES                       up, or down if negative
//! key SCANCODE [down|up]            by scan code (0x1E, or E04D for the right arrow)
//! type TEXT                         with the keys that make it on this keyboard layout
//! quit
//! ```
//!
//! Places are pixels of the remote screen, as in the decoded picture, not of
//! the window. The commands do what a person at the viewer could do, to the
//! host the viewer is connected to, and nothing else: mouse commands need
//! mouse control to be allowed, as the person's mouse does.

use std::path::PathBuf;

use tidedesk_core::protocol::MouseButton;

use crate::stream::Picture;

/// By how much a crop is enlarged or reduced, in whole numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Each pixel this many times as wide and high.
    Times(u32),
    /// This many pixels each way as one, their mean.
    Over(u32),
}

/// A rectangle of the remote screen, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Size,
    Frames,
    Stats,
    Crop {
        area: Area,
        file: PathBuf,
        scale: Scale,
    },
    Move {
        x: u32,
        y: u32,
    },
    Click {
        x: u32,
        y: u32,
        button: MouseButton,
    },
    Press {
        x: u32,
        y: u32,
        button: MouseButton,
    },
    Release {
        button: MouseButton,
    },
    Wheel {
        lines: i32,
    },
    /// Down and up when `pressed` is not given.
    Key {
        scancode: u16,
        pressed: Option<bool>,
    },
    Type {
        text: String,
    },
    Quit,
}

/// The most a crop is enlarged or reduced by.
const MOST: u32 = 8;

/// The most pixels in a crop: a 5120x2880 screen twice enlarged, 177 MB.
const LARGEST: u64 = 10_240 * 5_760;

/// The most lines the wheel is turned by at once.
const FAR: i32 = 100;

/// A pixel of `length` along the remote screen as the place that the host
/// turns back into that pixel (it rounds down).
pub fn place(pixel: u32, length: u32) -> u16 {
    let steps = u64::from(length.max(2) - 1);
    (u64::from(pixel) * 65535).div_ceil(steps).min(65535) as u16
}

/// The words of a line; what is between quotes is one word, spaces and all.
fn words(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let (word, after) = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"').ok_or("a quote is not closed")?,
            None => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
        };
        words.push(word.to_string());
        rest = after.trim_start();
    }
    Ok(words)
}

fn number<T: std::str::FromStr>(word: &str) -> Result<T, String> {
    word.parse()
        .map_err(|_| format!("{word:?} is not a number"))
}

fn button(word: Option<&String>) -> Result<MouseButton, String> {
    match word.map(String::as_str) {
        None | Some("left") => Ok(MouseButton::Left),
        Some("right") => Ok(MouseButton::Right),
        Some("middle") => Ok(MouseButton::Middle),
        Some(other) => Err(format!("{other:?} is not a mouse button")),
    }
}

fn scale(word: Option<&String>) -> Result<Scale, String> {
    let Some(word) = word else {
        return Ok(Scale::Times(1));
    };
    let scale = match word.strip_prefix("1/") {
        Some(over) => over.parse().ok().filter(|n| *n >= 2).map(Scale::Over),
        None => word.parse().ok().filter(|n| *n >= 1).map(Scale::Times),
    };
    scale
        .filter(|scale| matches!(scale, Scale::Times(n) | Scale::Over(n) if *n <= MOST))
        .ok_or_else(|| format!("a scale is 1 to {MOST}, or 1/2 to 1/{MOST}: {word:?}"))
}

/// Reads a command from a line of text; an error that says what is wrong
/// with it.
pub fn parse(line: &str) -> Result<Command, String> {
    // What is typed is taken as it is, spaces and all.
    if let Some(text) = line.trim_start().strip_prefix("type") {
        return match text.strip_prefix(char::is_whitespace) {
            Some(text) if !text.is_empty() => Ok(Command::Type { text: text.into() }),
            Some(_) | None if text.trim().is_empty() => Err("type TEXT".into()),
            _ => Err(format!("unknown command {:?}", line.trim())),
        };
    }
    let words = words(line)?;
    let Some((command, given)) = words.split_first() else {
        return Err("no command".into());
    };
    // How the command is written, the most it takes, and the least.
    let (usage, most, least) = match command.as_str() {
        "size" | "frames" | "stats" | "quit" => (command.as_str(), 0, 0),
        "move" => ("move X Y", 2, 2),
        "click" => ("click X Y [left|right|middle]", 3, 2),
        "press" => ("press X Y [left|right|middle]", 3, 2),
        "release" => ("release [left|right|middle]", 1, 0),
        "wheel" => ("wheel LINES", 1, 1),
        "key" => ("key SCANCODE [down|up]", 2, 1),
        "crop" => ("crop X Y WIDTH HEIGHT FILE [SCALE]", 6, 5),
        other => return Err(format!("unknown command {other:?}")),
    };
    if given.len() < least {
        return Err(usage.into());
    }
    if let Some(extra) = given.get(most) {
        return Err(format!("too much: {extra:?} ({usage})"));
    }
    let place = || Ok::<_, String>((number(&given[0])?, number(&given[1])?));
    Ok(match command.as_str() {
        "size" => Command::Size,
        "frames" => Command::Frames,
        "stats" => Command::Stats,
        "quit" => Command::Quit,
        "move" => {
            let (x, y) = place()?;
            Command::Move { x, y }
        }
        "click" => {
            let ((x, y), button) = (place()?, button(given.get(2))?);
            Command::Click { x, y, button }
        }
        "press" => {
            let ((x, y), button) = (place()?, button(given.get(2))?);
            Command::Press { x, y, button }
        }
        "release" => Command::Release {
            button: button(given.first())?,
        },
        "wheel" => {
            let lines = number::<i64>(&given[0])?;
            if lines.abs() > i64::from(FAR) {
                return Err(format!("the wheel turns {FAR} lines at most"));
            }
            Command::Wheel {
                lines: lines as i32,
            }
        }
        "key" => {
            let word = &given[0];
            let digits = word.strip_prefix("0x").unwrap_or(word);
            let scancode = u16::from_str_radix(digits, 16)
                .ok()
                .filter(|scancode| *scancode != 0)
                .ok_or_else(|| format!("{word:?} is not a scan code (as 0x1E, or E04D)"))?;
            let pressed = match given.get(1).map(String::as_str) {
                None => None,
                Some("down") => Some(true),
                Some("up") => Some(false),
                Some(other) => return Err(format!("{other:?} is neither down nor up")),
            };
            Command::Key { scancode, pressed }
        }
        _ => {
            let ((x, y), width, height) = (place()?, number(&given[2])?, number(&given[3])?);
            if width == 0 || height == 0 {
                return Err("an area has a width and a height".into());
            }
            Command::Crop {
                area: Area {
                    x,
                    y,
                    width,
                    height,
                },
                file: PathBuf::from(&given[4]),
                scale: scale(given.get(5))?,
            }
        }
    })
}

/// The pixels of `area` of the picture, scaled: width, height, and red,
/// green and blue for each pixel, row after row.
pub fn crop(picture: &Picture, area: Area, scale: Scale) -> Result<(u32, u32, Vec<u8>), String> {
    let (wide, high) = (picture.width, picture.height);
    if wide == 0 || high == 0 || picture.pixels.len() < (wide * high) as usize {
        return Err("no picture yet".into());
    }
    let Area {
        x,
        y,
        width,
        height,
    } = area;
    let inside =
        |from: u32, length: u32, of: u32| from.checked_add(length).is_some_and(|to| to <= of);
    if !inside(x, width, wide) || !inside(y, height, high) {
        return Err(format!(
            "{width}x{height} at {x},{y} is not inside: the picture is {wide}x{high}"
        ));
    }
    let pixel = |x: u32, y: u32| {
        let [blue, green, red, _] = picture.pixels[(y * wide + x) as usize].to_le_bytes();
        [red, green, blue]
    };
    let (out_width, out_height) = match scale {
        Scale::Times(times) => (width * times, height * times),
        Scale::Over(over) => (width / over, height / over),
    };
    if out_width == 0 || out_height == 0 {
        return Err(format!("nothing is left of {width}x{height} at that scale"));
    }
    if u64::from(out_width) * u64::from(out_height) > LARGEST {
        return Err(format!(
            "{out_width}x{out_height} is too large: enlarge a smaller area"
        ));
    }
    let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);
    for row in 0..out_height {
        for column in 0..out_width {
            match scale {
                Scale::Times(times) => rgb.extend(pixel(x + column / times, y + row / times)),
                Scale::Over(over) => {
                    let mut sums = [0u32; 3];
                    for (dx, dy) in (0..over).flat_map(|dy| (0..over).map(move |dx| (dx, dy))) {
                        let part = pixel(x + column * over + dx, y + row * over + dy);
                        for (sum, part) in sums.iter_mut().zip(part) {
                            *sum += u32::from(part);
                        }
                    }
                    rgb.extend(sums.map(|sum| (sum / (over * over)) as u8));
                }
            }
        }
    }
    Ok((out_width, out_height, rgb))
}

/// Saves pixels as [`crop`] gives them as a PNG file.
pub fn save(
    file: &std::path::Path,
    (width, height, rgb): (u32, u32, Vec<u8>),
) -> Result<(), String> {
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        let file = std::io::BufWriter::new(std::fs::File::create(file)?);
        let mut encoder = png::Encoder::new(file, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgb)?;
        Ok(())
    };
    write().map_err(|e| format!("{} was not written: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: u32, y: u32, width: u32, height: u32) -> Area {
        Area {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn commands_are_read_from_lines() {
        use Command::*;
        use MouseButton::*;
        let file = PathBuf::from("out.png");
        for (line, command) in [
            ("size", Size),
            ("  frames  ", Frames),
            ("stats", Stats),
            ("quit", Quit),
            ("move 10 20", Move { x: 10, y: 20 }),
            (
                "click 10 20",
                Click {
                    x: 10,
                    y: 20,
                    button: Left,
                },
            ),
            (
                "click 10 20 right",
                Click {
                    x: 10,
                    y: 20,
                    button: Right,
                },
            ),
            (
                "press 0 0 middle",
                Press {
                    x: 0,
                    y: 0,
                    button: Middle,
                },
            ),
            ("release", Release { button: Left }),
            ("release right", Release { button: Right }),
            ("wheel -3", Wheel { lines: -3 }),
            (
                "key 0x1E",
                Key {
                    scancode: 0x1E,
                    pressed: None,
                },
            ),
            (
                "key 1e down",
                Key {
                    scancode: 0x1E,
                    pressed: Some(true),
                },
            ),
            (
                "key E04D up",
                Key {
                    scancode: 0xE04D,
                    pressed: Some(false),
                },
            ),
            (
                "type hello  world ",
                Type {
                    text: "hello  world ".into(),
                },
            ),
            (
                "crop 1 2 30 40 out.png",
                Crop {
                    area: area(1, 2, 30, 40),
                    file: file.clone(),
                    scale: Scale::Times(1),
                },
            ),
            (
                "crop 1 2 30 40 out.png 3",
                Crop {
                    area: area(1, 2, 30, 40),
                    file: file.clone(),
                    scale: Scale::Times(3),
                },
            ),
            (
                "crop 1 2 30 40 out.png 1/2",
                Crop {
                    area: area(1, 2, 30, 40),
                    file: file.clone(),
                    scale: Scale::Over(2),
                },
            ),
        ] {
            assert_eq!(parse(line), Ok(command), "{line:?}");
        }
    }

    #[test]
    fn a_file_name_may_have_spaces_in_quotes() {
        let command = parse("crop 0 0 8 8 \"C:\\test kit\\a b.png\" 2");
        let file = PathBuf::from("C:\\test kit\\a b.png");
        let area = area(0, 0, 8, 8);
        let scale = Scale::Times(2);
        assert_eq!(command, Ok(Command::Crop { area, file, scale }));
    }

    #[test]
    fn what_is_wrong_with_a_line_is_said() {
        for (line, says) in [
            ("", "no command"),
            ("fly", "unknown command \"fly\""),
            ("click 10", "click X Y [left|right|middle]"),
            ("click ten 20", "\"ten\" is not a number"),
            ("click 10 20 thumb", "\"thumb\" is not a mouse button"),
            ("click 10 20 left now", "too much: \"now\""),
            ("key", "key SCANCODE [down|up]"),
            ("key 0xZZ", "\"0xZZ\" is not a scan code"),
            ("key 1E sideways", "\"sideways\" is neither down nor up"),
            ("key 0", "\"0\" is not a scan code"),
            ("type", "type TEXT"),
            ("crop 0 0 0 10 a.png", "an area has a width and a height"),
            ("crop 0 0 10 10", "crop X Y WIDTH HEIGHT FILE [SCALE]"),
            ("crop 0 0 10 10 a.png 9", "a scale is 1 to 8, or 1/2 to 1/8"),
            (
                "crop 0 0 10 10 a.png 1/1",
                "a scale is 1 to 8, or 1/2 to 1/8",
            ),
            (
                "crop 0 0 10 10 a.png big",
                "a scale is 1 to 8, or 1/2 to 1/8",
            ),
            ("crop 0 0 10 10 \"a.png", "a quote is not closed"),
        ] {
            let error = parse(line).unwrap_err();
            assert!(error.contains(says), "{line:?}: {error}");
        }
    }

    /// Four by three, each pixel its own number, red in the high byte.
    fn picture() -> Picture {
        Picture {
            width: 4,
            height: 3,
            pixels: (0..12)
                .map(|i| ((i * 16) << 16) | (i << 8) | (255 - i))
                .collect(),
            ..Picture::default()
        }
    }

    fn rgb(i: u32) -> [u8; 3] {
        [(i * 16) as u8, i as u8, (255 - i) as u8]
    }

    #[test]
    fn a_crop_is_the_pictures_own_pixels() {
        let (width, height, pixels) = crop(&picture(), area(1, 1, 2, 2), Scale::Times(1)).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(pixels, [rgb(5), rgb(6), rgb(9), rgb(10)].concat());
        let whole = crop(&picture(), area(0, 0, 4, 3), Scale::Times(1)).unwrap();
        assert_eq!(whole.2.len(), 4 * 3 * 3);
    }

    #[test]
    fn enlarged_pixels_are_repeated_not_blended() {
        let (width, height, pixels) = crop(&picture(), area(2, 0, 2, 1), Scale::Times(2)).unwrap();
        assert_eq!((width, height), (4, 2));
        let row = [rgb(2), rgb(2), rgb(3), rgb(3)].concat();
        assert_eq!(pixels, [row.clone(), row].concat());
    }

    #[test]
    fn reduced_pixels_are_the_mean_of_those_they_stand_for() {
        let (width, height, pixels) = crop(&picture(), area(0, 0, 4, 2), Scale::Over(2)).unwrap();
        assert_eq!((width, height), (2, 1));
        // Pixels 0, 1, 4 and 5, then 2, 3, 6 and 7.
        let mean = |of: [u32; 4]| -> Vec<u8> {
            (0..3)
                .map(|c| (of.iter().map(|&i| u32::from(rgb(i)[c])).sum::<u32>() / 4) as u8)
                .collect()
        };
        assert_eq!(pixels, [mean([0, 1, 4, 5]), mean([2, 3, 6, 7])].concat());
        // What is left over at the edges is left out.
        let (width, height, _) = crop(&picture(), area(0, 0, 3, 3), Scale::Over(2)).unwrap();
        assert_eq!((width, height), (1, 1));
    }

    #[test]
    fn an_area_outside_the_picture_is_an_error() {
        for outside in [area(3, 0, 2, 1), area(0, 2, 1, 2), area(4, 0, 1, 1)] {
            let error = crop(&picture(), outside, Scale::Times(1)).unwrap_err();
            assert!(error.contains("the picture is 4x3"), "{error}");
        }
        let error = crop(&picture(), area(0, 0, 1, 1), Scale::Over(2)).unwrap_err();
        assert!(error.contains("nothing is left"), "{error}");
        let none = crop(&Picture::default(), area(0, 0, 1, 1), Scale::Times(1));
        assert!(none.unwrap_err().contains("no picture yet"));
    }

    /// A whole 4K screen eight times enlarged would be 1.6 GB.
    #[test]
    fn a_crop_has_its_limits() {
        let large = Picture {
            width: 3840,
            height: 2160,
            pixels: vec![0; 3840 * 2160],
            ..Picture::default()
        };
        let error = crop(&large, area(0, 0, 3840, 2160), Scale::Times(8)).unwrap_err();
        assert!(error.contains("30720x17280 is too large"), "{error}");
        // The whole of it as it is, and a part of it enlarged, are not.
        assert!(crop(&large, area(0, 0, 3840, 2160), Scale::Times(1)).is_ok());
        assert!(crop(&large, area(100, 100, 640, 360), Scale::Times(8)).is_ok());
    }

    #[test]
    fn the_wheel_has_its_limits() {
        assert_eq!(parse("wheel 100"), Ok(Command::Wheel { lines: 100 }));
        assert_eq!(parse("wheel -100"), Ok(Command::Wheel { lines: -100 }));
        for line in ["wheel 101", "wheel -101", "wheel 20000000"] {
            let error = parse(line).unwrap_err();
            assert!(error.contains("100 lines at most"), "{line:?}: {error}");
        }
    }

    #[test]
    fn a_saved_crop_reads_back_as_it_was() {
        let file = std::env::temp_dir().join(format!("tidedesk-crop-{}.png", std::process::id()));
        let cropped = crop(&picture(), area(0, 0, 4, 3), Scale::Times(1)).unwrap();
        save(&file, cropped.clone()).unwrap();
        let decoder =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&file).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (4, 3));
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!(pixels[..info.buffer_size()], cropped.2[..]);
        let _ = std::fs::remove_file(&file);
        // Where it cannot be written, that is said.
        let nowhere = std::env::temp_dir().join("no such folder").join("a.png");
        assert!(save(&nowhere, cropped).unwrap_err().contains("a.png"));
    }
}
