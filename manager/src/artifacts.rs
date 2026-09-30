//! What a green build's jobs uploaded, collected into the build's `dist/`.
//!
//! The daemon runs act with `--artifact-server-path artifacts` and
//! `GITHUB_RUN_ID=<id>`, so upload-artifact@v4 leaves one zip per artifact in
//! `builds/<id>/artifacts/<id>/<name>/<name>.zip`. act's artifact server takes
//! uploads from anyone on the LAN, keeps the last of two uploads of one name,
//! and leaves a 0-byte zip behind an upload that failed. So a zip is only
//! taken when exactly one upload-artifact step said it uploaded it: its
//! `artifact-id` output is act's id for the name (fnv32a, [`fnv32a`]), and its
//! `artifact-digest` output is the zip's sha256. An upload-artifact@v3 layout
//! (plain files, no zip) is left, with a note.
//!
//! Each zip's entries must stay inside it (no absolute path, no `..`), and hold
//! no symlink. Its files are flattened to their base names: a `NAME.sha256`
//! beside NAME must match it, and then goes; other characters than
//! `[A-Za-z0-9._-]` become `.`. Two different files with one name, from two
//! artifacts or after that, are refused: a matrix's legs that each built
//! "their" CPU under act give two files of one name.
//!
//! Any refusal leaves `dist/` out altogether, with the reasons: a release is
//! never made of part of a build. The work is local (unzip and sha256sum, or
//! shasum on a Mac), so it takes seconds.

use crate::actlog::{self, Event};
use crate::results::Artifact;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Names bana's installer writes into dist/: a build's files cannot take them.
const RESERVED: [&str; 3] = ["SHA256SUMS", "install.sh", "install.ps1"];

/// A file in a build's dist/.
#[derive(Debug, Clone, PartialEq)]
pub struct DistFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    /// `linux-x64`, `macos-arm64`, `windows-x64`…: an archive the installer
    /// can install (`*-linux-x64.tar.gz`, `*-windows-arm64.zip`).
    pub platform: Option<String>,
}

/// What [`collect`] did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Collection {
    /// dist/'s files, by name; none when anything was refused.
    pub files: Vec<DistFile>,
    /// Each artifact, as results.jsonl lists it.
    pub artifacts: Vec<Artifact>,
    /// Why nothing was collected: each refused artifact and why.
    pub problem: Option<String>,
}

/// act's id for an artifact's name (artifactNameToID): FNV-1a, 32 bits.
pub fn fnv32a(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5, |h: u32, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// One step's upload-artifact outputs.
#[derive(Debug, Default)]
struct Upload {
    key: String,
    step: String,
    id: Option<u64>,
    digest: Option<String>,
}

/// The upload-artifact@v4 outputs in act's lines (`::set-output::
/// artifact-id=…` and `artifact-digest=…`), one [`Upload`] per job and step.
fn uploads(log: &str) -> Vec<Upload> {
    let mut by: BTreeMap<(String, String), Upload> = BTreeMap::new();
    for line in log.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some("set-output"), Some(name), Some(arg)) =
            (v["command"].as_str(), v["name"].as_str(), v["arg"].as_str())
        else {
            continue;
        };
        if name != "artifact-id" && name != "artifact-digest" {
            continue;
        }
        let Event::Job(l) = actlog::parse_line(line) else {
            continue;
        };
        // A step's ids: a composite action's inner step has its parent's first.
        let ids = v["stepID"].to_string();
        let u = by.entry((l.key.clone(), ids)).or_insert_with(|| Upload {
            key: l.key.clone(),
            step: l.step.clone().unwrap_or_default(),
            ..Upload::default()
        });
        match name {
            "artifact-id" => u.id = arg.trim().parse().ok(),
            _ => u.digest = Some(arg.trim().to_ascii_lowercase()),
        }
    }
    by.into_values().collect()
}

/// A file's sha256: sha256sum, else shasum (every Mac).
pub fn sha256(path: &Path) -> Result<String, String> {
    for (program, args) in [("sha256sum", &[][..]), ("shasum", &["-a", "256"][..])] {
        let out = match Command::new(program)
            .args(args)
            .arg(path)
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) => o,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{program}: {e}")),
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let sum = text.split_whitespace().next().unwrap_or("");
        if out.status.success() && sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(sum.to_ascii_lowercase());
        }
        return Err(format!(
            "{program} {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Err("no sha256sum or shasum here".into())
}

/// unzip's output: `unzip OPTIONS ZIP [-d INTO]`.
fn unzip(opts: &[&str], zip: &Path, into: Option<&Path>) -> Result<String, String> {
    let mut cmd = Command::new("unzip");
    cmd.args(opts).arg(zip);
    if let Some(d) = into {
        cmd.arg("-d").arg(d);
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("unzip: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "unzip: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The installer's platform for an archive's name.
pub fn platform(name: &str) -> Option<String> {
    let (stem, oses): (&str, &[&str]) = if let Some(s) = name.strip_suffix(".tar.gz") {
        (s, &["linux", "macos"])
    } else if let Some(s) = name.strip_suffix(".zip") {
        (s, &["windows"])
    } else {
        return None;
    };
    oses.iter().find_map(|os| {
        ["x64", "arm64"].iter().find_map(|arch| {
            let p = format!("{os}-{arch}");
            stem.strip_suffix(&p)
                .filter(|s| s.ends_with('-'))
                .map(|_| p.clone())
        })
    })
}

/// A file's name in dist/: other characters than `[A-Za-z0-9._-]` become `.`.
pub fn normalize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '.',
        })
        .collect()
}

/// One artifact's zip, bound to its upload, checked and unzipped into `into`:
/// its files there, companions checked and gone.
fn unpack(
    zip: &Path,
    a: &mut Artifact,
    ups: &[Upload],
    into: &Path,
) -> Result<Vec<PathBuf>, String> {
    a.bytes = std::fs::metadata(zip).map_err(|e| e.to_string())?.len();
    if a.bytes == 0 {
        return Err("0 bytes: act's placeholder for an upload that failed".into());
    }
    let sum = sha256(zip)?;
    a.sha256 = Some(sum.clone());
    let id = u64::from(fnv32a(&a.name));
    let by: Vec<&Upload> = ups.iter().filter(|u| u.id == Some(id)).collect();
    let u = match by[..] {
        [] => {
            return Err(
                "no upload-artifact step uploaded it (put there through act's artifact \
                 server, or an upload that never finished)"
                    .into(),
            )
        }
        [u] => u,
        _ => {
            let keys: Vec<&str> = by.iter().map(|u| u.key.as_str()).collect();
            return Err(format!(
                "uploaded by {} steps ({}): act kept only the last",
                by.len(),
                keys.join(", ")
            ));
        }
    };
    a.key = Some(u.key.clone());
    a.step = Some(u.step.clone()).filter(|s| !s.is_empty());
    if u.digest.as_deref() != Some(sum.as_str()) {
        return Err(format!(
            "its sha256 {sum} is not the digest {}'s upload gave ({})",
            u.key,
            u.digest.as_deref().unwrap_or("none")
        ));
    }
    // Entries stay inside, once each (in any case: a Mac's disk ignores it);
    // no symlinks (zipinfo's mode, `l…`).
    let mut seen = std::collections::BTreeSet::new();
    for e in unzip(&["-Z1"], zip, None)?.lines() {
        if e.starts_with('/') || e.split(['/', '\\']).any(|p| p == "..") {
            return Err(format!("an entry outside the artifact: {e}"));
        }
        if !seen.insert(e.to_lowercase()) {
            return Err(format!("the entry {e} twice"));
        }
    }
    if let Some(l) = unzip(&["-Z"], zip, None)?
        .lines()
        .find(|l| l.starts_with('l'))
    {
        let name = l.split_whitespace().skip(8).collect::<Vec<_>>().join(" ");
        return Err(format!("a symlink: {name}"));
    }
    std::fs::create_dir_all(into).map_err(|e| e.to_string())?;
    unzip(&["-qq", "-o"], zip, Some(into))?;
    let mut files = Vec::new();
    walk(into, into, &mut files)?;
    files.sort();
    // NAME.sha256 beside NAME: it must match, and goes.
    for f in &files {
        let companion = PathBuf::from(format!("{}.sha256", f.display()));
        if files.contains(&companion) {
            let text = std::fs::read_to_string(&companion).unwrap_or_default();
            let want = text
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if want != sha256(f)? {
                return Err(format!("{} does not match its file", rel(into, &companion)));
            }
        }
    }
    let of = |f: &PathBuf| f.to_str()?.strip_suffix(".sha256").map(PathBuf::from);
    let kept = files
        .iter()
        .filter(|f| !of(f).is_some_and(|c| files.contains(&c)));
    Ok(kept.cloned().collect())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// The regular files under `dir`; a symlink (or anything else) is refused.
fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for e in entries {
        let p = e.map_err(|e| e.to_string())?.path();
        let t = std::fs::symlink_metadata(&p)
            .map_err(|e| e.to_string())?
            .file_type();
        if t.is_dir() {
            walk(root, &p, out)?;
        } else if t.is_file() {
            out.push(p);
        } else {
            return Err(format!("not a plain file: {}", rel(root, &p)));
        }
    }
    Ok(())
}

/// Collects build `id`'s artifacts (in `dir`, its `builds/<id>`) into
/// `dir/dist`, made anew. Leaves `artifacts/` as it was: the caller removes it once the
/// installer is made.
pub fn collect(dir: &Path, id: u64) -> Collection {
    let log = std::fs::read(dir.join("act.jsonl")).unwrap_or_default();
    let ups = uploads(&String::from_utf8_lossy(&log));
    let root = dir.join("artifacts").join(id.to_string());
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let (tmp, part, dist) = (
        dir.join("dist.tmp"),
        dir.join("dist.part"),
        dir.join("dist"),
    );
    for d in [&tmp, &part, &dist] {
        let _ = std::fs::remove_dir_all(d);
    }
    let mut c = Collection::default();
    let mut refused = Vec::new();
    // dist/'s names, lowercased (a Mac's disk ignores case): the file, the
    // artifact and name it came from, and where it is now.
    let mut taken: BTreeMap<String, (DistFile, String, String, PathBuf)> = BTreeMap::new();
    for (i, name) in names.iter().enumerate() {
        let mut a = Artifact {
            name: name.clone(),
            ..Artifact::default()
        };
        let zip = root.join(name).join(format!("{name}.zip"));
        if !zip.is_file() {
            a.problem = Some("an upload-artifact@v3 layout (no zip): not collected".into());
            c.artifacts.push(a);
            continue;
        }
        let into = tmp.join(i.to_string());
        let got = unpack(&zip, &mut a, &ups, &into).and_then(|files| {
            let mut mine = Vec::new();
            for f in files {
                let base = f
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let n = normalize(&base);
                if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&n)) {
                    return Err(format!("{base}: bana's installer writes that name"));
                }
                let sum = sha256(&f)?;
                if let Some((was, from, first, _)) = taken.get(&n.to_ascii_lowercase()) {
                    if was.sha256 == sum && *first == base {
                        continue;
                    }
                    return Err(if *first == base {
                        format!(
                            "{base} comes from {from} too, with other content (a matrix's \
                             legs, each building its CPU under act?)"
                        )
                    } else {
                        format!(
                            "{base} and {from}'s {first} would both be {} in dist",
                            was.name
                        )
                    });
                }
                let bytes = std::fs::metadata(&f).map_err(|e| e.to_string())?.len();
                let file = DistFile {
                    platform: platform(&n),
                    name: n.clone(),
                    bytes,
                    sha256: sum,
                };
                taken.insert(n.to_ascii_lowercase(), (file, name.clone(), base, f));
                mine.push(n);
            }
            Ok(mine)
        });
        match got {
            Ok(mut files) => {
                files.sort();
                a.files = files;
            }
            Err(why) => {
                refused.push(format!("{name}: {why}"));
                a.problem = Some(why);
            }
        }
        c.artifacts.push(a);
    }
    if refused.is_empty() && !taken.is_empty() {
        let moved = std::fs::create_dir_all(&part)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                for (f, _, _, from) in taken.values() {
                    std::fs::rename(from, part.join(&f.name)).map_err(|e| e.to_string())?;
                }
                std::fs::rename(&part, &dist).map_err(|e| e.to_string())
            });
        match moved {
            Ok(()) => {
                c.files = taken.into_values().map(|(f, ..)| f).collect();
                c.files.sort_by(|a, b| a.name.cmp(&b.name));
            }
            Err(e) => refused.push(format!("dist: {e}")),
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::remove_dir_all(&part);
    if !refused.is_empty() {
        c.problem = Some(refused.join("; "));
    }
    c
}

/// The artifacts into the build's results.jsonl (made from act.jsonl and
/// build.json when there is none yet), so the CI report lists them.
pub fn record(dir: &Path, artifacts: &[Artifact]) -> Result<(), String> {
    let mut r = crate::report::read_build(dir)?;
    r.artifacts = artifacts.to_vec();
    let (path, part) = (dir.join("results.jsonl"), dir.join("results.jsonl.part"));
    std::fs::write(&part, r.to_jsonl())
        .and_then(|_| std::fs::rename(&part, &path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/artifacts");

    /// A directory of its own (tests run side by side).
    fn scratch(name: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("bana-artifacts-{name}-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    /// A build directory from one of research's real act runs.
    fn fixture(name: &str) -> PathBuf {
        let d = scratch(name);
        copy(&Path::new(FIXTURES).join(name), &d);
        d
    }

    fn crc32(data: &[u8]) -> u32 {
        !data.iter().fold(!0u32, |c, &b| {
            (0..8).fold(c ^ u32::from(b), |c, _| {
                (c >> 1) ^ (0xedb8_8320 & (c & 1).wrapping_neg())
            })
        })
    }

    /// A zip of stored entries: (name, content, unix mode).
    fn zip(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let (mut out, mut central) = (Vec::new(), Vec::new());
        let u16s = |v: &mut Vec<u8>, xs: &[u16]| xs.iter().for_each(|x| v.extend(x.to_le_bytes()));
        let u32s = |v: &mut Vec<u8>, xs: &[u32]| xs.iter().for_each(|x| v.extend(x.to_le_bytes()));
        for (name, data, mode) in entries {
            let (at, crc, n) = (out.len() as u32, crc32(data), data.len() as u32);
            u32s(&mut out, &[0x0403_4b50]);
            u16s(&mut out, &[20, 0, 0, 0, 0x21]);
            u32s(&mut out, &[crc, n, n]);
            u16s(&mut out, &[name.len() as u16, 0]);
            out.extend(name.as_bytes());
            out.extend(*data);
            u32s(&mut central, &[0x0201_4b50]);
            u16s(&mut central, &[0x0314, 20, 0, 0, 0, 0x21]);
            u32s(&mut central, &[crc, n, n]);
            u16s(&mut central, &[name.len() as u16, 0, 0, 0, 0]);
            u32s(&mut central, &[mode << 16, at]);
            central.extend(name.as_bytes());
        }
        let (at, size, count) = (out.len() as u32, central.len() as u32, entries.len() as u16);
        out.extend(&central);
        u32s(&mut out, &[0x0605_4b50]);
        u16s(&mut out, &[0, 0, count, count]);
        u32s(&mut out, &[size, at]);
        u16s(&mut out, &[0]);
        out
    }

    const FILE: u32 = 0o100_644;

    /// Build 1: each zip uploaded by a job of its own, whose outputs say so.
    fn build(name: &str, zips: &[(&str, Vec<u8>)]) -> PathBuf {
        let d = scratch(name);
        let mut log = String::new();
        for (n, bytes) in zips {
            let z = d.join(format!("artifacts/1/{n}/{n}.zip"));
            std::fs::create_dir_all(z.parent().unwrap()).unwrap();
            std::fs::write(&z, bytes).unwrap();
            let sum = if bytes.is_empty() {
                "e3b0".into()
            } else {
                sha256(&z).unwrap()
            };
            for (k, v) in [
                ("artifact-id", fnv32a(n).to_string()),
                ("artifact-digest", sum),
            ] {
                log += &serde_json::json!({
                    "arg": v, "command": "set-output", "jobID": "package",
                    "matrix": {"target": n}, "msg": format!("  ⚙  ::set-output:: {k}={v}"),
                    "name": k, "stage": "Main", "step": "actions/upload-artifact@v4",
                    "stepID": ["1"], "level": "info", "job": "ci/package",
                })
                .to_string();
                log.push('\n');
            }
        }
        std::fs::write(d.join("act.jsonl"), log).unwrap();
        d
    }

    fn names(c: &Collection) -> Vec<&str> {
        c.files.iter().map(|f| f.name.as_str()).collect()
    }

    fn refused(d: &Path, c: &Collection, why: &str) {
        let p = c.problem.as_deref().unwrap_or("");
        assert!(p.contains(why), "{p}");
        assert!(
            c.files.is_empty() && !d.join("dist").exists(),
            "nothing collected"
        );
        assert!(!d.join("dist.tmp").exists() && !d.join("dist.part").exists());
    }

    #[test]
    fn fnv32a_is_acts_artifact_id() {
        assert_eq!(fnv32a("demo-nightly-linux-x64"), 2190529846);
        assert_eq!(fnv32a("demo-nightly-linux-arm64"), 502717012);
        assert_eq!(fnv32a("same"), 3440134715);
    }

    #[test]
    fn b10_collects_its_package_and_skips_v3() {
        let d = fixture("b10");
        let c = collect(&d, 10);
        assert_eq!(c.problem, None);
        assert_eq!(
            names(&c),
            [
                "demo-nightly-abc123-linux-x64.tar.gz",
                // '~' and '+' become '.'.
                "demo_0.0.0.nightly202609290728.gabc123_amd64.deb",
            ]
        );
        let tar = &c.files[0];
        assert_eq!(tar.platform.as_deref(), Some("linux-x64"));
        assert_eq!(c.files[1].platform, None);
        let on_disk = std::fs::metadata(d.join("dist").join(&tar.name))
            .unwrap()
            .len();
        assert_eq!(tar.bytes, on_disk);
        assert_eq!(sha256(&d.join("dist").join(&tar.name)).unwrap(), tar.sha256);
        // The .sha256 companions matched, and went.
        let mut dist: Vec<String> = std::fs::read_dir(d.join("dist"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        dist.sort();
        assert_eq!(dist, names(&c));
        let a = &c.artifacts;
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].name, "demo-nightly-linux-x64");
        assert_eq!(a[0].key.as_deref(), Some("package (linux-x64)"));
        assert_eq!(a[0].step.as_deref(), Some("actions/upload-artifact@v4"));
        assert_eq!(
            a[0].sha256.as_deref(),
            Some("c0790662af6c536395f239dc9f7890ca3c0e6cd934b4fc0d867611e8cd5117cf")
        );
        assert_eq!(
            (a[0].bytes, a[0].files.len(), &a[0].problem),
            (1160, 2, &None)
        );
        assert_eq!(a[1].name, "old-style");
        assert!(a[1]
            .problem
            .as_deref()
            .unwrap()
            .contains("upload-artifact@v3"));
        // artifacts/ stays, for the caller.
        assert!(d.join("artifacts/10/demo-nightly-linux-x64").exists());
        // Again: dist/ is made anew.
        std::fs::write(d.join("dist/stale"), "x").unwrap();
        assert_eq!(names(&collect(&d, 10)).len(), 2);
        assert!(!d.join("dist/stale").exists());
    }

    #[test]
    fn b9_legs_with_one_name_collide() {
        let d = fixture("b9");
        let c = collect(&d, 9);
        refused(&d, &c, "demo-nightly-linux-x64: demo-nightly-abc123-linux-x86_64.tar.gz comes from demo-nightly-linux-arm64 too, with other content");
        assert_eq!(
            c.artifacts[0].problem, None,
            "the first leg itself was fine"
        );
    }

    #[test]
    fn dup_last_writer_is_refused() {
        let d = fixture("dup");
        let c = collect(&d, 13);
        refused(
            &d,
            &c,
            "same: uploaded by 2 steps (up (a), up (b)): act kept only the last",
        );
    }

    #[test]
    fn planted_zip_is_refused() {
        let d = fixture("planted");
        let c = collect(&d, 11);
        refused(
            &d,
            &c,
            "example-release-evil: no upload-artifact step uploaded it",
        );
        assert_eq!(c.artifacts[0].key, None);
    }

    #[test]
    fn a_flipped_byte_is_refused() {
        let d = fixture("b10");
        let z = d.join("artifacts/10/demo-nightly-linux-x64/demo-nightly-linux-x64.zip");
        let mut bytes = std::fs::read(&z).unwrap();
        bytes[100] ^= 1;
        std::fs::write(&z, bytes).unwrap();
        let c = collect(&d, 10);
        refused(
            &d,
            &c,
            "is not the digest package (linux-x64)'s upload gave (c0790662",
        );
    }

    #[test]
    fn a_failed_upload_is_refused() {
        let d = build("empty", &[("pkg", Vec::new())]);
        refused(&d, &collect(&d, 1), "pkg: 0 bytes");
    }

    #[test]
    fn entries_outside_and_symlinks_are_refused() {
        let d = build("dotdot", &[("pkg", zip(&[("a/../../x", b"x", FILE)]))]);
        refused(
            &d,
            &collect(&d, 1),
            "an entry outside the artifact: a/../../x",
        );
        let d = build("abs", &[("pkg", zip(&[("/etc/x", b"x", FILE)]))]);
        refused(&d, &collect(&d, 1), "an entry outside the artifact: /etc/x");
        let link = zip(&[("bin/demo", b"/etc/passwd", 0o120_777)]);
        let d = build("link", &[("pkg", link)]);
        refused(&d, &collect(&d, 1), "a symlink: bin/demo");
    }

    #[test]
    fn companions_must_match() {
        let good = zip(&[
            ("out/a.tar.gz", b"archive", FILE),
            ("out/a.tar.gz.sha256", b"0000  a.tar.gz\n", FILE),
        ]);
        let d = build("badsum", &[("pkg", good)]);
        refused(
            &d,
            &collect(&d, 1),
            "pkg: out/a.tar.gz.sha256 does not match its file",
        );
        // A .sha256 with no file beside it is a file like any other.
        let d = build(
            "lone",
            &[("pkg", zip(&[("SUMS.sha256", b"0000  x\n", FILE)]))],
        );
        assert_eq!(names(&collect(&d, 1)), ["SUMS.sha256"]);
    }

    #[test]
    fn one_file_twice_is_kept_once_and_clashes_are_refused() {
        let same = zip(&[("notes.txt", b"one", FILE)]);
        let d = build("same", &[("a", same.clone()), ("b", same)]);
        let c = collect(&d, 1);
        assert_eq!((names(&c), c.problem.as_deref()), (vec!["notes.txt"], None));
        let d = build(
            "clash",
            &[
                ("a", zip(&[("x~1", b"one", FILE)])),
                ("b", zip(&[("x+1", b"two", FILE)])),
            ],
        );
        refused(
            &d,
            &collect(&d, 1),
            "b: x+1 and a's x~1 would both be x.1 in dist",
        );
        let d = build(
            "case",
            &[("a", zip(&[("A", b"1", FILE), ("sub/a", b"2", FILE)]))],
        );
        refused(
            &d,
            &collect(&d, 1),
            "a: a and a's A would both be A in dist",
        );
        let d = build(
            "reserved",
            &[("a", zip(&[("install.sh", b"#!/bin/sh", FILE)]))],
        );
        refused(
            &d,
            &collect(&d, 1),
            "install.sh: bana's installer writes that name",
        );
    }

    #[test]
    fn nothing_uploaded_nothing_collected() {
        let d = scratch("none");
        let c = collect(&d, 1);
        assert_eq!(c, Collection::default());
        assert!(!d.join("dist").exists());
    }

    #[test]
    fn platforms() {
        for (name, p) in [
            ("demo-v1-linux-x64.tar.gz", Some("linux-x64")),
            ("demo-macos-arm64.tar.gz", Some("macos-arm64")),
            ("demo-windows-arm64.zip", Some("windows-arm64")),
            ("demo-linux-x64.zip", None),
            ("demo-windows-x64.tar.gz", None),
            ("linux-x64.tar.gz", None),
            ("demo-linux-x86_64.tar.gz", None),
        ] {
            assert_eq!(platform(name).as_deref(), p, "{name}");
        }
    }

    #[test]
    fn record_puts_them_in_results() {
        let d = fixture("b10");
        std::fs::write(
            d.join("build.json"),
            r#"{"request":{"id":10,"sha":"abc","ref":"refs/heads/main","tier":"nightly","trigger":"push"},"build":{"state":"success"}}"#,
        )
        .unwrap();
        let c = collect(&d, 10);
        if let Err(e) = record(&d, &c.artifacts) {
            panic!("{e}");
        }
        let text = std::fs::read_to_string(d.join("results.jsonl")).unwrap();
        let r = crate::results::Results::from_jsonl(&text);
        assert_eq!(r.artifacts, c.artifacts);
        let line = text
            .lines()
            .find(|l| l.contains(r#""kind":"artifact""#))
            .unwrap();
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["name"], "demo-nightly-linux-x64");
        assert_eq!(v["files"][0], "demo-nightly-abc123-linux-x64.tar.gz");
    }
}
