//! The scan before a release goes to a public repository (`bana split`):
//! what in its files would tell that repository's readers about the private
//! code ([`crate::release::leak`]'s needles).
//!
//! A file is read by what its first bytes say it is, never by its name:
//! - a compressed stream (gzip or compress, bzip2, xz, zstd) through its tool;
//! - an archive unpacked into a scratch directory: tar; zip and its kin (jar,
//!   whl, apk, nupkg: unzip); with bsdtar, ar (deb), rpm, cpio, 7z, xar (pkg),
//!   rar, cab and ISO 9660. Its listing (members' names, and link targets) is
//!   read too, then each member in turn, archives inside it as well;
//! - anything else as it is.
//!
//! A format bana cannot read (a dmg, squashfs and AppImage, lz4, lzip, an MSI,
//! or one whose tool is not here), archives nested more than [`MAX_DEPTH`]
//! deep, or more than [`MAX_BYTES`] unpacked, count as holding something: the
//! publish stops, naming the file.

use crate::release;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Archives and streams inside each other, at most.
pub const MAX_DEPTH: usize = 6;
/// Bytes unpacked for one release, at most.
pub const MAX_BYTES: u64 = 16 << 30;

/// What a file is, by its first (and last) bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A compressed stream: its tool's name.
    Stream(&'static str),
    Tar,
    Zip,
    /// An archive bsdtar reads: its format.
    Other(&'static str),
    /// A format bana does not read.
    Unread(&'static str),
    Plain,
}

fn kind(head: &[u8], tail: &[u8]) -> Kind {
    let at = |i: usize, m: &[u8]| head.get(i..i + m.len()) == Some(m);
    if at(0, &[0x1f, 0x8b]) || at(0, &[0x1f, 0x9d]) {
        Kind::Stream("gzip")
    } else if at(0, b"BZh") {
        Kind::Stream("bzip2")
    } else if at(0, &[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Kind::Stream("xz")
    } else if at(0, &[0x28, 0xb5, 0x2f, 0xfd]) {
        Kind::Stream("zstd")
    } else if at(257, b"ustar") {
        Kind::Tar
    } else if at(0, b"PK\x03\x04") || at(0, b"PK\x05\x06") {
        Kind::Zip
    } else if at(0, b"!<arch>\n") {
        Kind::Other("ar")
    } else if at(0, &[0xed, 0xab, 0xee, 0xdb]) {
        Kind::Other("rpm")
    } else if at(0, b"07070") || at(0, &[0xc7, 0x71]) || at(0, &[0x71, 0xc7]) {
        Kind::Other("cpio")
    } else if at(0, &[b'7', b'z', 0xbc, 0xaf, 0x27, 0x1c]) {
        Kind::Other("7z")
    } else if at(0, b"xar!") {
        Kind::Other("xar")
    } else if at(0, b"Rar!\x1a\x07") {
        Kind::Other("rar")
    } else if at(0, b"MSCF") {
        Kind::Other("cab")
    } else if at(0x8001, b"CD001") {
        Kind::Other("iso9660")
    } else if tail.len() >= 512 && &tail[tail.len() - 512..tail.len() - 508] == b"koly" {
        Kind::Unread("dmg")
    } else if at(0, b"hsqs") || at(0, b"sqsh") {
        Kind::Unread("squashfs")
    } else if at(0, b"\x7fELF") && at(8, b"AI\x02") {
        Kind::Unread("AppImage")
    } else if at(0, &[0x04, 0x22, 0x4d, 0x18]) {
        Kind::Unread("lz4")
    } else if at(0, b"LZIP") {
        Kind::Unread("lzip")
    } else if at(0, &[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        Kind::Unread("an MSI or other OLE file")
    } else {
        Kind::Plain
    }
}

/// The first of `names` (files in `dir`) that holds one of `needles`, and
/// what (or why bana cannot tell): `name` for the file itself, `name: member`
/// for what an archive holds. The tools run with `path` as PATH; `scratch`
/// (made, then removed) takes what archives unpack to.
pub fn scan(
    dir: &Path,
    names: &[String],
    needles: &[(String, bool)],
    path: &str,
    scratch: &Path,
) -> Option<(String, String)> {
    let _ = std::fs::remove_dir_all(scratch);
    if let Err(e) = std::fs::create_dir_all(scratch) {
        return Some((
            scratch.display().to_string(),
            format!("no scratch directory: {e}"),
        ));
    }
    let mut s = Scanner {
        needles,
        path,
        scratch,
        n: 0,
        bytes: 0,
    };
    let found = names
        .iter()
        .find_map(|name| s.file(&dir.join(name), name, 0).err());
    let _ = std::fs::remove_dir_all(scratch);
    found
}

struct Scanner<'a> {
    needles: &'a [(String, bool)],
    path: &'a str,
    scratch: &'a Path,
    /// Scratch names given.
    n: u64,
    /// Bytes unpacked so far.
    bytes: u64,
}

type Found = (String, String);

fn cannot(label: &str, why: &str) -> Found {
    (label.to_string(), format!("what bana cannot read ({why})"))
}

impl Scanner<'_> {
    fn temp(&mut self) -> PathBuf {
        self.n += 1;
        self.scratch.join(self.n.to_string())
    }

    fn tool(&self, program: &str) -> Command {
        let mut c = Command::new(program);
        c.env("PATH", self.path)
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        c
    }

    /// bsdtar, as `bsdtar` or as `tar` (macOS's).
    fn bsdtar(&self) -> Option<&'static str> {
        ["bsdtar", "tar"].into_iter().find(|t| {
            self.tool(t)
                .arg("--version")
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("bsdtar"))
        })
    }

    fn leaks(&self, label: &str, bytes: &[u8]) -> Result<(), Found> {
        match release::leak(bytes, self.needles) {
            Some(what) => Err((label.to_string(), what.to_string())),
            None => Ok(()),
        }
    }

    /// `f`, as [`kind`] says it is.
    fn file(&mut self, f: &Path, label: &str, depth: usize) -> Result<(), Found> {
        let (head, tail) = ends(f).map_err(|e| cannot(label, &e.to_string()))?;
        let k = kind(&head, &tail);
        if k == Kind::Plain {
            return self.plain(f, label);
        }
        if depth >= MAX_DEPTH {
            return Err(cannot(label, "archives nested deeper than bana reads"));
        }
        match k {
            Kind::Stream(tool) => {
                let out = self.temp();
                let r = self.unpack(f, tool, &out, label);
                let r = r.and_then(|_| self.file(&out, label, depth + 1));
                let _ = std::fs::remove_file(&out);
                r
            }
            Kind::Unread(what) => Err(cannot(label, what)),
            Kind::Tar | Kind::Zip | Kind::Other(_) => {
                let to = self.temp();
                let r = self.archive(f, k, &to, label, depth);
                let _ = std::fs::remove_dir_all(&to);
                r
            }
            Kind::Plain => unreachable!(),
        }
    }

    /// A plain file, read a MiB at a time (each read with the end of the one before).
    fn plain(&self, f: &Path, label: &str) -> Result<(), Found> {
        let mut file = std::fs::File::open(f).map_err(|e| cannot(label, &e.to_string()))?;
        let keep = self
            .needles
            .iter()
            .map(|(t, _)| t.len() * 2 + 4)
            .max()
            .unwrap_or(0);
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = vec![0u8; 1 << 20];
        loop {
            let n = file
                .read(&mut chunk)
                .map_err(|e| cannot(label, &e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            self.leaks(label, &buf)?;
            let cut = buf.len().saturating_sub(keep);
            buf.drain(..cut);
        }
    }

    /// `f` through `tool -dc` into `out`, within [`MAX_BYTES`].
    fn unpack(&mut self, f: &Path, tool: &str, out: &Path, label: &str) -> Result<(), Found> {
        let input = std::fs::File::open(f).map_err(|e| cannot(label, &e.to_string()))?;
        let mut child = self
            .tool(tool)
            .arg("-dc")
            .stdin(input)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| cannot(label, &format!("{tool} is not here")))?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut file = std::fs::File::create(out).map_err(|e| cannot(label, &e.to_string()))?;
        let room = MAX_BYTES.saturating_sub(self.bytes);
        let copied = std::io::copy(&mut (&mut stdout).take(room + 1), &mut file);
        drop(stdout);
        let n = match copied {
            Ok(n) if n <= room => n,
            over => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match over {
                    Ok(_) => cannot(label, "more than bana unpacks"),
                    Err(e) => cannot(label, &e.to_string()),
                });
            }
        };
        let ok = child.wait().is_ok_and(|s| s.success());
        if !ok {
            return Err(cannot(label, &format!("{tool} could not read it")));
        }
        file.flush().map_err(|e| cannot(label, &e.to_string()))?;
        self.bytes += n;
        Ok(())
    }

    /// An archive: its listing, then what it unpacks to (in `to`).
    fn archive(
        &mut self,
        f: &Path,
        k: Kind,
        to: &Path,
        label: &str,
        depth: usize,
    ) -> Result<(), Found> {
        std::fs::create_dir_all(to).map_err(|e| cannot(label, &e.to_string()))?;
        let (list, unpack): (Vec<&str>, [String; 1]) = match k {
            Kind::Tar => (vec!["tar", "-tvf"], ["tar".into()]),
            Kind::Zip if self.found("unzip") => {
                // unzip -l's last line: the bytes it unpacks to.
                let o = self
                    .tool("unzip")
                    .arg("-l")
                    .arg(f)
                    .output()
                    .map_err(|_| cannot(label, "unzip is not here"))?;
                let total = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .and_then(|l| l.split_whitespace().next()?.parse::<u64>().ok());
                match total {
                    Some(n) if o.status.success() => {
                        if n > MAX_BYTES.saturating_sub(self.bytes) {
                            return Err(cannot(label, "more than bana unpacks"));
                        }
                    }
                    _ => return Err(cannot(label, "unzip could not list it")),
                }
                (vec!["unzip", "-Z1"], ["unzip".into()])
            }
            Kind::Zip | Kind::Other(_) => {
                let what = match k {
                    Kind::Other(w) => w,
                    _ => "zip",
                };
                let Some(b) = self.bsdtar() else {
                    return Err(cannot(label, &format!("{what}, and bsdtar is not here")));
                };
                (vec![b, "-tvf"], [b.into()])
            }
            _ => unreachable!(),
        };
        let listed = self
            .tool(list[0])
            .args(&list[1..])
            .arg(f)
            .output()
            .map_err(|_| cannot(label, &format!("{} is not here", list[0])))?;
        if !listed.status.success() {
            return Err(cannot(label, &format!("{} could not list it", list[0])));
        }
        self.leaks(label, &listed.stdout)?;
        let mut c = self.tool(&unpack[0]);
        if unpack[0] == "unzip" {
            c.args(["-qq", "-o"]).arg(f).arg("-d").arg(to);
        } else {
            c.args(["-x", "--no-same-owner", "-f"])
                .arg(f)
                .arg("-C")
                .arg(to);
        }
        let ok = c.stdout(Stdio::null()).status().is_ok_and(|s| s.success());
        if !ok {
            return Err(cannot(label, &format!("{} could not unpack it", unpack[0])));
        }
        self.walk(to, to, label, depth)
    }

    fn found(&self, program: &str) -> bool {
        self.path
            .split(':')
            .any(|d| !d.is_empty() && Path::new(d).join(program).is_file())
    }

    /// What an archive unpacked to: each file, as `label: path`; a link's
    /// target read as text; anything else (a fifo) left alone.
    fn walk(&mut self, root: &Path, dir: &Path, label: &str, depth: usize) -> Result<(), Found> {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| cannot(label, &e.to_string()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for p in entries {
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            let inner = format!("{label}: {rel}");
            let meta = std::fs::symlink_metadata(&p).map_err(|e| cannot(&inner, &e.to_string()))?;
            let t = meta.file_type();
            if t.is_symlink() {
                let to = std::fs::read_link(&p).map_err(|e| cannot(&inner, &e.to_string()))?;
                self.leaks(&inner, to.as_os_str().as_encoded_bytes())?;
            } else if t.is_dir() {
                self.walk(root, &p, label, depth)?;
            } else if t.is_file() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
                self.bytes += meta.len();
                if self.bytes > MAX_BYTES {
                    return Err(cannot(label, "more than bana unpacks"));
                }
                self.file(&p, &inner, depth + 1)?;
            }
        }
        Ok(())
    }
}

/// A file's first 0x8006 bytes (enough for an ISO's mark) and its last 512.
fn ends(f: &Path) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let mut file = std::fs::File::open(f)?;
    let mut head = Vec::new();
    (&mut file).take(0x8006).read_to_end(&mut head)?;
    let len = file.metadata()?.len();
    let mut tail = Vec::new();
    if len >= 512 {
        file.seek(SeekFrom::Start(len - 512))?;
        file.take(512).read_to_end(&mut tail)?;
    }
    Ok((head, tail))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as Std;

    fn run(dir: &Path, cmd: &str) {
        let ok = Std::new("sh")
            .args(["-c", cmd])
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "{cmd}");
    }

    #[test]
    fn a_public_releases_files_are_read_by_what_they_are() {
        let dir = std::env::temp_dir().join(format!("bana-scan-{}", std::process::id()));
        let scratch = dir.join("scratch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("demo-1/bin")).unwrap();
        let w = |p: &str, t: &[u8]| std::fs::write(dir.join(p), t).unwrap();
        let needles = vec![
            ("o/r".to_string(), true),
            ("/home/runner/work/".to_string(), false),
        ];
        let path = std::env::var("PATH").unwrap();
        let scan = |n: &[&str]| {
            let names: Vec<String> = n.iter().map(|x| x.to_string()).collect();
            super::scan(&dir, &names, &needles, &path, &scratch)
        };
        w("install.sh", b"REPO='o/r-releases'\n");
        w("SHA256SUMS", b"abc  demo-1-linux-x64.tar.gz\n");
        assert_eq!(scan(&["install.sh", "SHA256SUMS"]), None);

        // Inside a tar.gz, with its member named.
        w("demo-1/bin/demo", b"built from git@github.com:o/r.git\n");
        run(&dir, "tar -czf demo.tar.gz demo-1");
        assert_eq!(
            scan(&["install.sh", "demo.tar.gz"]),
            Some(("demo.tar.gz: demo-1/bin/demo".into(), "o/r".into())),
            "inside the archive"
        );
        // By its bytes, not its name; and through more than one layer.
        run(
            &dir,
            "cp demo.tar.gz demo.bin && tar -cf outer.tar demo.bin && gzip -c outer.tar >outer.dat",
        );
        assert_eq!(
            scan(&["outer.dat"]),
            Some(("outer.dat: demo.bin: demo-1/bin/demo".into(), "o/r".into())),
            "a tar.gz in a tar.gz, named .dat"
        );
        // A zip (a jar, a wheel) inside a tar.
        w("demo-1/bin/demo", b"clean\n");
        run(&dir, "rm -f demo.tar.gz && mkdir -p j && printf 'at /home/runner/work/x\\n' >j/A.class && (cd j && zip -q -r ../lib.jar .) && tar -czf pkg.tgz lib.jar demo-1");
        assert_eq!(
            scan(&["pkg.tgz"]),
            Some((
                "pkg.tgz: lib.jar: A.class".into(),
                "/home/runner/work/".into()
            ))
        );
        // A member's name, and a link's target.
        run(
            &dir,
            "rm -rf n && mkdir -p 'n/o/r' && echo x >'n/o/r/x' && tar -cf names.tar n",
        );
        assert_eq!(
            scan(&["names.tar"]),
            Some(("names.tar".into(), "o/r".into()))
        );
        run(
            &dir,
            "rm -rf l && mkdir l && ln -s /home/runner/work/a l/link && tar -cf link.tar l",
        );
        assert_eq!(
            scan(&["link.tar"]),
            Some(("link.tar".into(), "/home/runner/work/".into())),
            "tar -tv lists a link's target"
        );
        // UTF-16LE, as Windows binaries hold strings.
        let wide: Vec<u8> = "path o/r here".bytes().flat_map(|b| [b, 0]).collect();
        w("demo.exe", &wide);
        assert_eq!(scan(&["demo.exe"]), Some(("demo.exe".into(), "o/r".into())));
        // xz, when xz is here.
        if Std::new("xz").arg("--version").output().is_ok() {
            run(&dir, "printf 'from o/r\\n' | xz -c >notes.xz");
            assert_eq!(scan(&["notes.xz"]), Some(("notes.xz".into(), "o/r".into())));
        }
        // What bana cannot read stops the publish, naming the file.
        w("broken.tar.gz", b"\x1f\x8bnot gzip");
        assert_eq!(
            scan(&["broken.tar.gz"]),
            Some((
                "broken.tar.gz".into(),
                "what bana cannot read (gzip could not read it)".into()
            ))
        );
        let mut dmg = vec![0u8; 2048];
        dmg[2048 - 512..2048 - 508].copy_from_slice(b"koly");
        w("demo.dmg", &dmg);
        assert_eq!(
            scan(&["demo.dmg"]),
            Some(("demo.dmg".into(), "what bana cannot read (dmg)".into()))
        );
        w("demo.lz4", b"\x04\x22\x4d\x18 frames");
        assert_eq!(
            scan(&["demo.lz4"]),
            Some(("demo.lz4".into(), "what bana cannot read (lz4)".into()))
        );
        // Nested deeper than bana reads.
        run(
            &dir,
            "printf clean >d0 && for i in 1 2 3 4 5 6 7; do gzip -c d$((i - 1)) >d$i; done",
        );
        assert_eq!(
            scan(&["d7"]),
            Some((
                "d7".into(),
                "what bana cannot read (archives nested deeper than bana reads)".into()
            ))
        );
        assert_eq!(scan(&["d5"]), None);
        assert!(!scratch.exists(), "the scratch directory goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kinds_by_their_bytes() {
        assert_eq!(kind(b"\x1f\x8b\x08", &[]), Kind::Stream("gzip"));
        assert_eq!(kind(b"BZh91AY", &[]), Kind::Stream("bzip2"));
        assert_eq!(kind(b"\x28\xb5\x2f\xfd", &[]), Kind::Stream("zstd"));
        assert_eq!(kind(b"PK\x03\x04", &[]), Kind::Zip);
        assert_eq!(kind(b"!<arch>\ndebian-binary", &[]), Kind::Other("ar"));
        assert_eq!(kind(b"\xed\xab\xee\xdb", &[]), Kind::Other("rpm"));
        assert_eq!(kind(b"xar!", &[]), Kind::Other("xar"));
        assert_eq!(kind(b"hsqs", &[]), Kind::Unread("squashfs"));
        assert_eq!(
            kind(b"\x7fELF\x02\x01\x01\x00AI\x02", &[]),
            Kind::Unread("AppImage")
        );
        assert_eq!(kind(b"\x7fELF\x02\x01\x01\x00\x00\x00", &[]), Kind::Plain);
        assert_eq!(kind(b"#!/bin/sh\n", &[]), Kind::Plain);
    }
}
