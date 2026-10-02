//! Files copied between the two computers, each on its own QUIC stream,
//! directly: they never pass through a server. A stream starts with
//! [`MAGIC`], then the length of a postcard [`Header`], the header, and the
//! file's bytes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: &[u8; 4] = b"TDF1";
/// File streams yield to the picture, input and sound.
pub const PRIORITY: i32 = -1;
const MAX_HEADER: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub size: u64,
}

/// A file name that is safe to create in a folder: no folders of its own,
/// no characters Windows refuses, no reserved device names. `None` when
/// nothing usable is left.
pub fn safe_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return None;
    }
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    let cleaned = if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    };
    Some(cleaned.chars().take(200).collect())
}

/// `name` in `dir`, or "name (2).ext" and so on when it is taken.
pub fn unique(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() && !part(&first).exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists() && !part(p).exists())
        .expect("a free name")
}

fn part(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Where received files go: `Downloads\TideDesk` of the user signed in.
pub fn downloads() -> Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .context("cannot find the user's folder")?;
    let dir = PathBuf::from(home).join("Downloads").join("TideDesk");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Whether sending failed because the other side stopped the stream: it
/// refused the file and says why itself.
pub fn stopped(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        let write = cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io| io.get_ref())
            .and_then(|inner| inner.downcast_ref::<quinn::WriteError>())
            .or_else(|| cause.downcast_ref::<quinn::WriteError>());
        matches!(write, Some(quinn::WriteError::Stopped(_)))
    })
}

/// Writes the file at `path` to a stream: what the other side receives.
pub async fn send<W: AsyncWrite + Unpin>(stream: &mut W, path: &Path) -> Result<u64> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let size = file.metadata().await?.len();
    let name = path
        .file_name()
        .context("not a file")?
        .to_string_lossy()
        .into_owned();
    let header = postcard::to_stdvec(&Header { name, size })?;
    stream.write_all(MAGIC).await?;
    stream
        .write_all(&(header.len() as u32).to_le_bytes())
        .await?;
    stream.write_all(&header).await?;
    let copied = tokio::io::copy(&mut (&mut file).take(size), stream).await?;
    if copied != size {
        bail!("the file changed while it was being sent");
    }
    Ok(size)
}

/// Reads a file from a stream into `dir`: where it was saved. A file that
/// does not arrive whole is removed.
pub async fn receive<R: AsyncRead + Unpin>(stream: &mut R, dir: &Path) -> Result<PathBuf> {
    let header = read_header(stream).await?;
    save(stream, &header, dir).await
}

/// Reads what a file stream says it carries.
pub async fn read_header<R: AsyncRead + Unpin>(stream: &mut R) -> Result<Header> {
    let mut magic = [0u8; 4];
    stream.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        bail!("not a TideDesk file");
    }
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_HEADER {
        bail!("the file's header is too long");
    }
    let mut header = vec![0u8; len];
    stream.read_exact(&mut header).await?;
    postcard::from_bytes(&header).context("the file's header")
}

/// Saves the rest of a file stream, after its header, into `dir`.
pub async fn save<R: AsyncRead + Unpin>(
    stream: &mut R,
    header: &Header,
    dir: &Path,
) -> Result<PathBuf> {
    let name = safe_name(&header.name).context("the file has no usable name")?;
    // Claim a name: two files of the same name arriving at once each get
    // their own, as the `.part` is only ever created new.
    let (path, mut file) = loop {
        let path = unique(dir, &name);
        let created = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(part(&path))
            .await;
        match created {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        }
    };
    let partial = part(&path);
    let result = async {
        let copied = tokio::io::copy(&mut stream.take(header.size), &mut file).await?;
        if copied != header.size {
            bail!(
                "the file arrived cut short ({copied} of {} bytes)",
                header.size
            );
        }
        let mut rest = [0u8; 1];
        if stream.read(&mut rest).await? != 0 {
            bail!("more arrived than the file's size");
        }
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&partial, &path).await?;
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("report.pdf").as_deref(), Some("report.pdf"));
        assert_eq!(
            safe_name(r"..\..\Windows\evil.dll").as_deref(),
            Some("evil.dll")
        );
        assert_eq!(safe_name("../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(safe_name("a<b>:c?.txt").as_deref(), Some("a_b__c_.txt"));
        assert_eq!(safe_name("con.txt").as_deref(), Some("_con.txt"));
        assert_eq!(safe_name("COM1").as_deref(), Some("_COM1"));
        assert_eq!(safe_name("compass.txt").as_deref(), Some("compass.txt"));
        assert_eq!(safe_name("trailing. . ").as_deref(), Some("trailing"));
        assert_eq!(safe_name(".."), None);
        assert_eq!(safe_name("dir/"), None);
        assert_eq!(safe_name(&"x".repeat(300)).map(|n| n.len()), Some(200));
    }

    #[test]
    fn a_stopped_send_is_told_apart() {
        let stopped_send = anyhow::Error::new(std::io::Error::other(quinn::WriteError::Stopped(
            1u32.into(),
        )));
        assert!(stopped(&stopped_send));
        assert!(stopped(&stopped_send.context("sending a.txt")));
        assert!(!stopped(&anyhow::anyhow!("the disk is full")));
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let dir = temp("unique");
        assert_eq!(unique(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), "1").unwrap();
        assert_eq!(unique(&dir, "a.txt"), dir.join("a (2).txt"));
        std::fs::write(dir.join("a (2).txt.part"), "").unwrap();
        assert_eq!(unique(&dir, "a.txt"), dir.join("a (3).txt"));
        assert_eq!(unique(&dir, ".hidden"), dir.join(".hidden"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Two files of the same name arriving at once keep both.
    #[tokio::test]
    async fn files_of_the_same_name_at_once_keep_both() {
        let to = temp("same");
        let stream = |byte: u8| {
            let header = postcard::to_stdvec(&Header {
                name: "same.txt".into(),
                size: 3,
            })
            .unwrap();
            let mut bytes = MAGIC.to_vec();
            bytes.extend((header.len() as u32).to_le_bytes());
            bytes.extend(&header);
            bytes.extend([byte; 3]);
            bytes
        };
        let (a, b) = (stream(b'a'), stream(b'b'));
        let (mut a, mut b) = (a.as_slice(), b.as_slice());
        let (first, second) = tokio::join!(receive(&mut a, &to), receive(&mut b, &to));
        let (first, second) = (first.unwrap(), second.unwrap());
        assert_ne!(first, second);
        let mut contents = [
            std::fs::read_to_string(&first).unwrap(),
            std::fs::read_to_string(&second).unwrap(),
        ];
        contents.sort();
        assert_eq!(contents, ["aaa", "bbb"]);
        std::fs::remove_dir_all(&to).unwrap();
    }

    #[tokio::test]
    async fn a_file_arrives_whole_or_not_at_all() {
        let from = temp("from");
        let to = temp("to");
        let source = from.join("photo.bin");
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&source, &bytes).unwrap();

        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let sending = tokio::spawn(async move {
            let size = send(&mut a, &source).await.unwrap();
            a.shutdown().await.unwrap();
            size
        });
        let saved = receive(&mut b, &to).await.unwrap();
        assert_eq!(sending.await.unwrap(), bytes.len() as u64);
        assert_eq!(saved, to.join("photo.bin"));
        assert_eq!(std::fs::read(&saved).unwrap(), bytes);

        // Cut short: nothing is left behind.
        let header = postcard::to_stdvec(&Header {
            name: "cut.bin".into(),
            size: 10,
        })
        .unwrap();
        let mut stream = MAGIC.to_vec();
        stream.extend((header.len() as u32).to_le_bytes());
        stream.extend(&header);
        stream.extend([1, 2, 3]);
        let why = receive(&mut stream.as_slice(), &to).await.unwrap_err();
        assert!(why.to_string().contains("cut short"), "{why}");
        assert!(!to.join("cut.bin").exists() && !to.join("cut.bin.part").exists());

        assert!(receive(&mut b"nope".as_slice(), &to).await.is_err());
        std::fs::remove_dir_all(&from).unwrap();
        std::fs::remove_dir_all(&to).unwrap();
    }
}
