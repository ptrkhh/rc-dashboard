//! The media cache and the rules for uploaded names and types. Uploads live
//! under `<cache>/uploads/<session>/<id>/`, images shown in chat under
//! `<cache>/media/<sha256>.<ext>`. Folders are 0700, files 0600.

use super::hub::{new_epoch, Attachment, Kind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{Error, ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};
use tokio::io::AsyncWriteExt;

pub const MAX_UPLOAD: u64 = 20 * 1024 * 1024;
pub const MAX_FILES: usize = 10;
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
/// The one image type table: (MIME type, file extension).
pub const IMAGES: [(&str, &str); 4] = [("image/png", "png"), ("image/jpeg", "jpg"), ("image/gif", "gif"), ("image/webp", "webp")];

fn root_from(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    match xdg.map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(x) => x.join("cdash"),
        None => PathBuf::from(home.unwrap_or_else(|| "/".into())).join(".cache/cdash"),
    }
}

#[cfg(not(test))]
pub fn cache_root() -> PathBuf {
    root_from(std::env::var_os("XDG_CACHE_HOME"), std::env::var_os("HOME"))
}
#[cfg(test)]
pub fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!("cdash-test-{}", std::process::id())).join("cdash")
}

/// `None` unless `session` is one plain path component (pane names are `cdash-[A-Za-z0-9_-]+`).
pub fn uploads_dir(session: &str) -> Option<PathBuf> {
    let plain = !session.is_empty() && session != ".." && session != "." && !session.contains(['/', '\\', '\0']);
    plain.then(|| cache_root().join("uploads").join(session))
}
pub fn media_dir() -> PathBuf {
    cache_root().join("media")
}

/// Create `dir`, then set 0700 on it and on every folder up to the cache root.
pub(crate) fn private_dirs(dir: &Path) -> std::io::Result<()> {
    std::os::unix::fs::DirBuilderExt::mode(std::fs::DirBuilder::new().recursive(true), 0o700).create(dir)?;
    let root = cache_root();
    for d in dir.ancestors().take_while(|d| d.starts_with(&root)) {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The type from the first bytes, never from a name.
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else if head.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

pub fn image_ext(mime: &str) -> Option<&'static str> {
    IMAGES.iter().find(|(m, _)| *m == mime).map(|(_, e)| *e)
}

/// A safe base name: no folders, only `A-Za-z0-9._- `, at most 100 bytes.
pub fn sanitize_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ');
    let clean: String = base.chars().map(|c| if ok(c) { c } else { '_' }).collect();
    let clean = clean.trim_start_matches(['.', ' ']).trim_end_matches(' ');
    if clean.is_empty() {
        return "file".into();
    }
    if clean.len() <= 100 {
        return clean.into();
    }
    match clean.rfind('.').filter(|i| (2..=17).contains(&(clean.len() - i))) {
        Some(i) => format!("{}{}", clean[..i.min(100 - (clean.len() - i))].trim_end(), &clean[i..]),
        None => clean[..100].trim_end().into(),
    }
}

/// `name` with its extension replaced by `ext`, at most 100 bytes. `name` must
/// come from `sanitize_name` (ASCII, so the byte cut is safe).
pub(crate) fn image_name(name: &str, ext: &str) -> String {
    let stem = name.rfind('.').map_or(name, |i| &name[..i]);
    format!("{}.{ext}", &stem[..stem.len().min(99 - ext.len())])
}

/// One upload in progress: `uploads_dir(session)/<id>/.part` until `finish`
/// renames it. Dropped unfinished, it removes its folder.
pub struct Upload {
    dir: PathBuf,
    id: String,
    name: String,
    file: tokio::fs::File,
    size: u64,
    got: u64,
    head: Vec<u8>,
    done: bool,
}

impl Upload {
    pub async fn begin(session: &str, name: &str, size: u64) -> Result<Upload, String> {
        if size == 0 {
            return Err("empty file".into());
        }
        if size > MAX_UPLOAD {
            return Err("file over 20 MB".into());
        }
        let dir = uploads_dir(session).ok_or("bad session")?;
        let id = new_epoch();
        let dir = dir.join(&id);
        let opened = async {
            private_dirs(&dir)?;
            let file = tokio::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(".part")).await?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600)).await?;
            Ok::<_, Error>(file)
        };
        match opened.await {
            Ok(file) => Ok(Upload { dir, id, name: sanitize_name(name), file, size, got: 0, head: vec![], done: false }),
            Err(_) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err("upload failed".into())
            }
        }
    }

    /// `Ok(true)` once every declared byte has arrived.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<bool, String> {
        if self.got + chunk.len() as u64 > self.size {
            let _ = std::fs::remove_dir_all(&self.dir);
            return Err("more bytes than declared".into());
        }
        self.file.write_all(chunk).await.map_err(|_| "upload failed")?;
        self.got += chunk.len() as u64;
        let need = 16usize.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..need]);
        Ok(self.got == self.size)
    }

    pub async fn finish(mut self) -> Result<(String, Attachment), String> {
        if self.got != self.size {
            return Err("upload failed".into());
        }
        self.file.flush().await.map_err(|_| "upload failed")?;
        let mime = sniff(&self.head).unwrap_or("application/octet-stream");
        let (kind, name) = match image_ext(mime) {
            Some(ext) => (Kind::Image, image_name(&self.name, ext)),
            None => (Kind::File, self.name.clone()),
        };
        let path = self.dir.join(&name);
        tokio::fs::rename(self.dir.join(".part"), &path).await.map_err(|_| "upload failed")?;
        self.done = true;
        Ok((self.id.clone(), Attachment { path, kind, mime: mime.into(), name, size: self.size }))
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Store an image once under its hash; a rewrite only refreshes its age.
pub fn stash(bytes: &[u8], mime: &str) -> std::io::Result<String> {
    let ext = image_ext(mime).filter(|_| sniff(bytes) == Some(mime)).ok_or_else(|| Error::new(ErrorKind::InvalidData, "not an image"))?;
    let r = format!("{:x}", Sha256::digest(bytes));
    let target = media_dir().join(format!("{r}.{ext}"));
    match std::fs::File::options().write(true).open(&target) {
        Ok(f) => {
            f.set_modified(SystemTime::now())?;
            return Ok(r);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    private_dirs(&media_dir())?;
    static N: AtomicU64 = AtomicU64::new(0);
    let tmp = media_dir().join(format!("{r}.{ext}.{}-{}.tmp", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let written = (|| {
        let mut f = std::fs::File::options().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(bytes)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, &target)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|()| r)
}

fn older(p: &Path, max_age: Duration) -> bool {
    std::fs::symlink_metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e > max_age)
}

pub(crate) fn sweep_in(root: &Path, max_age: Duration) {
    // `file_type` does not follow links: only real folders are entered, and a link is removed itself, never its target.
    let entries = |p: &Path| {
        std::fs::read_dir(p).into_iter().flatten().flatten().filter_map(|e| Some((e.path(), e.file_type().ok()?))).collect::<Vec<_>>()
    };
    for (session, ft) in entries(&root.join("uploads")) {
        if !ft.is_dir() {
            continue;
        }
        let ids = entries(&session);
        if ids.is_empty() {
            if older(&session, max_age) {
                let _ = std::fs::remove_dir(&session);
            }
            continue;
        }
        for (id, _) in ids.into_iter().filter(|(id, ft)| ft.is_dir() && older(id, max_age)) {
            let _ = std::fs::remove_dir_all(id);
        }
    }
    for (f, ft) in entries(&root.join("media")) {
        if !ft.is_dir() && older(&f, max_age) {
            let _ = std::fs::remove_file(f);
        }
    }
}

pub fn sweep(max_age: Duration) {
    sweep_in(&cache_root(), max_age);
}

/// Sweep now and then once a day.
pub async fn sweep_daily() {
    let mut tick = tokio::time::interval(Duration::from_secs(24 * 3600));
    loop {
        tick.tick().await;
        let _ = tokio::task::spawn_blocking(|| sweep(MAX_AGE)).await;
    }
}

/// The text, then one `Attached: <path>` line per path.
pub fn with_attached(text: &str, paths: &[&Path]) -> String {
    let mut lines: Vec<String> = if text.is_empty() { vec![] } else { vec![text.into()] };
    lines.extend(paths.iter().map(|p| format!("Attached: {}", p.display())));
    lines.join("\n")
}

/// Undo `with_attached`: trailing upload lines become `file` entries.
pub fn split_attached(text: &str) -> (String, Vec<Value>) {
    let (mut rest, mut files) = (text, vec![]);
    while let Some(line) = rest.rsplit('\n').next().filter(|l| l.starts_with("Attached: /") && l.contains("/cdash/uploads/")) {
        files.push(json!({"kind": "file", "name": line.rsplit('/').next()}));
        rest = rest.len().checked_sub(line.len() + 1).map_or("", |n| &rest[..n]);
    }
    files.reverse();
    (rest.trim_end().to_string(), files)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    pub(crate) fn mode(p: &Path) -> u32 { std::fs::metadata(p).unwrap().permissions().mode() & 0o777 }
    fn age(p: &Path, by: Duration) { std::fs::File::open(p).unwrap().set_modified(SystemTime::now() - by).unwrap(); }

    #[test]
    fn the_cache_root_follows_xdg_only_when_it_is_absolute() {
        let r = |x: Option<&str>, h: Option<&str>| root_from(x.map(Into::into), h.map(Into::into));
        assert_eq!(r(Some("/x/cache"), Some("/home/u")), PathBuf::from("/x/cache/cdash"));
        assert_eq!(r(Some("relative"), Some("/home/u")), PathBuf::from("/home/u/.cache/cdash"));
        assert_eq!(r(None, None), PathBuf::from("/.cache/cdash"));
        assert_eq!(uploads_dir("cdash-a"), Some(cache_root().join("uploads/cdash-a")));
    }
    #[test]
    fn names_are_reduced_to_a_safe_base_name_that_keeps_its_extension() {
        for (raw, want) in [("../x", "x"), ("..", "file"), (".hidden", "hidden"), ("a\0b<c>.txt", "a_b_c_.txt"),
            ("C:\\fakepath\\Shot 1.png", "Shot 1.png"), ("日本.png", "__.png"), ("", "file"), (" . ", "file")] {
            assert_eq!(sanitize_name(raw), want, "{raw:?}");
        }
        let long = sanitize_name(&format!("{}.pdf", "x".repeat(300)));
        assert_eq!((long.len(), long.ends_with(".pdf")), (100, true));
        assert_eq!(sanitize_name(&"é".repeat(150)), "_".repeat(100));
        assert_eq!([image_name("Shot.JPEG", "jpg"), image_name("notes.txt", "png"), image_name("file", "png")], ["Shot.jpg", "notes.png", "file.png"]);
        assert_eq!(image_name(&"z".repeat(100), "webp").len(), 100);
    }
    #[test]
    fn types_come_from_the_bytes_never_from_the_name() {
        let cases: [(&[u8], Option<&str>); 10] = [(PNG, Some("image/png")), (b"\xff\xd8\xff\xe0", Some("image/jpeg")),
            (b"GIF87a", Some("image/gif")), (b"GIF89a", Some("image/gif")), (b"RIFF\0\0\0\0WEBPVP8 ", Some("image/webp")),
            (b"%PDF-1.7", Some("application/pdf")), (b"a text file named x.png", None), (b"RIFF\0\0\0\0WAVEfmt ", None), (b"\x89PN", None), (b"", None)];
        for (head, want) in cases { assert_eq!(sniff(head), want, "{head:?}"); }
        assert_eq!(["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"].map(image_ext), [Some("png"), Some("jpg"), Some("gif"), Some("webp"), None]);
    }
    #[test]
    fn an_image_is_stored_once_by_its_hash_privately_and_a_rewrite_refreshes_it() {
        let png = [PNG, b"stash-test"].concat();
        let r = stash(&png, "image/png").unwrap();
        assert_eq!(r, format!("{:x}", Sha256::digest(&png)));
        let f = media_dir().join(format!("{r}.png"));
        assert_eq!((mode(&f), mode(&media_dir()), mode(&cache_root())), (0o600, 0o700, 0o700));
        age(&f, Duration::from_secs(86_400));
        assert_eq!(stash(&png, "image/png").unwrap(), r);
        assert!(std::fs::metadata(&f).unwrap().modified().unwrap() > SystemTime::now() - Duration::from_secs(60), "a rewrite refreshes its age");
        let copies = std::fs::read_dir(media_dir()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(&r)).count();
        assert_eq!(copies, 1, "one file, no temp left behind");
        for (bytes, mime) in [(&b"plain text"[..], "image/png"), (&png[..], "image/jpeg"), (&b"%PDF-1.7"[..], "application/pdf")] {
            assert_eq!(stash(bytes, mime).unwrap_err().kind(), std::io::ErrorKind::InvalidData, "{mime}");
        }
        let _ = std::fs::remove_file(&f); // not the whole root: other tests share it
    }
    #[test]
    fn the_sweep_removes_what_has_not_been_touched_for_the_limit() {
        let root = std::env::temp_dir().join(format!("cdash-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let file = |rel: &str| { let p = root.join(rel); std::fs::create_dir_all(p.parent().unwrap()).unwrap(); std::fs::write(&p, "x").unwrap(); p };
        let (old_up, new_up) = (file("uploads/cdash-a/aaaa/x.txt"), file("uploads/cdash-a/bbbb/y.txt"));
        let (old_media, new_media) = (file("media/1.png"), file("media/2.png"));
        for s in ["uploads/cdash-b", "uploads/cdash-c"] { std::fs::create_dir_all(root.join(s)).unwrap(); }
        for p in [old_up.parent().unwrap().to_path_buf(), root.join("uploads/cdash-b"), old_media.clone()] { age(&p, MAX_AGE + Duration::from_secs(3600)); }
        sweep_in(&root, MAX_AGE);
        assert!(!old_up.exists() && new_up.exists());
        assert!(!root.join("uploads/cdash-b").exists() && root.join("uploads/cdash-c").exists(), "only old empty session folders go");
        assert!(!old_media.exists() && new_media.exists());
        sweep_in(&root.join("missing"), MAX_AGE); // nothing there, no panic
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn trailing_attached_lines_for_uploads_become_file_entries_and_nothing_else_does() {
        let up = "/home/u/.cache/cdash/uploads/cdash-a/0123456789abcdef";
        let (pdf, txt) = (format!("{up}/r.pdf"), format!("{up}/notes 1.txt"));
        let sent = with_attached("look at these", &[Path::new(&pdf), Path::new(&txt)]);
        assert_eq!(sent, format!("look at these\nAttached: {pdf}\nAttached: {txt}"));
        assert_eq!(split_attached(&sent), ("look at these".into(), vec![json!({"kind":"file","name":"r.pdf"}), json!({"kind":"file","name":"notes 1.txt"})]));
        assert_eq!(split_attached(&with_attached("", &[Path::new(&pdf)])), (String::new(), vec![json!({"kind":"file","name":"r.pdf"})]));
        for kept in ["Attached: /etc/passwd".to_string(), "Attached: rel/cdash/uploads/x".into(), format!("Attached: {pdf}\nthen more")] {
            assert_eq!(split_attached(&kept), (kept.clone(), vec![]), "{kept}");
        }
    }

    #[test]
    fn the_sweep_never_follows_a_link_out_of_the_cache() {
        let base = std::env::temp_dir().join(format!("cdash-sweep-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, outside) = (base.join("root"), base.join("outside"));
        std::fs::create_dir_all(outside.join("id")).unwrap();
        std::fs::create_dir_all(root.join("uploads")).unwrap();
        std::fs::create_dir_all(root.join("media")).unwrap();
        let victim = outside.join("id/x.txt");
        std::fs::write(&victim, "x").unwrap();
        let secret = outside.join("secret.png");
        std::fs::write(&secret, "x").unwrap();
        let old = MAX_AGE + Duration::from_secs(3600);
        for p in [&victim, &outside.join("id"), &secret] { age(p, old); }
        std::os::unix::fs::symlink(&outside, root.join("uploads/link")).unwrap();
        std::os::unix::fs::symlink(&secret, root.join("media/s.png")).unwrap();
        sweep_in(&root, Duration::ZERO);
        assert!(victim.exists() && secret.exists(), "targets survive");
        assert!(root.join("uploads/link").exists(), "a linked session folder is skipped");
        assert!(std::fs::symlink_metadata(root.join("media/s.png")).is_err(), "the link itself goes");
        let _ = std::fs::remove_dir_all(&base);
    }
    #[test]
    fn the_sweep_keeps_an_old_session_folder_with_fresh_ids_and_skips_stray_files() {
        let root = std::env::temp_dir().join(format!("cdash-sweep-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let fresh = root.join("uploads/cdash-a/fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        let stray = root.join("uploads/cdash-a/stray.txt");
        std::fs::write(&stray, "x").unwrap();
        let old = MAX_AGE + Duration::from_secs(3600);
        age(&root.join("uploads/cdash-a"), old);
        age(&stray, old);
        sweep_in(&root, MAX_AGE);
        assert!(fresh.exists() && stray.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
    #[test]
    fn a_session_name_that_is_not_one_plain_component_has_no_folder() {
        for bad in ["", ".", "..", "a/b", "../x"] { assert_eq!(uploads_dir(bad), None, "{bad:?}"); }
        assert!(uploads_dir("cdash-a_1").is_some());
    }
    #[tokio::test]
    async fn an_upload_over_the_limit_or_empty_is_refused_before_anything_is_written() {
        let dir = uploads_dir("cdash-up-limit").unwrap();
        assert_eq!(Upload::begin("cdash-up-limit", "big.bin", MAX_UPLOAD + 1).await.err().as_deref(), Some("file over 20 MB"));
        assert_eq!(Upload::begin("cdash-up-limit", "e.txt", 0).await.err().as_deref(), Some("empty file"));
        assert_eq!(Upload::begin("../x", "a.txt", 1).await.err().as_deref(), Some("bad session"));
        assert!(!dir.exists(), "nothing was created");
        drop(Upload::begin("cdash-up-limit", "ok.bin", MAX_UPLOAD).await.unwrap());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "an unfinished upload leaves no folder");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn bytes_past_the_declared_size_abort_the_upload_and_delete_it() {
        let mut u = Upload::begin("cdash-up-over", "a.txt", 4).await.unwrap();
        assert_eq!(u.write(b"ab").await, Ok(false));
        assert_eq!(u.write(b"cdef").await, Err("more bytes than declared".to_string()));
        drop(u);
        let dir = uploads_dir("cdash-up-over").unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn a_finished_upload_is_named_typed_by_its_bytes_and_private() {
        let up = uploads_dir("cdash-up-done").unwrap();
        let png = [PNG, b"0123"].concat();
        let mut u = Upload::begin("cdash-up-done", "../My Shot.JPG", png.len() as u64).await.unwrap();
        assert_eq!(u.write(&png[..5]).await, Ok(false));
        assert_eq!(u.write(&png[5..]).await, Ok(true));
        let (id, a) = u.finish().await.unwrap();
        assert!(id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(a.path, up.join(&id).join("My Shot.png"), "a PNG named .JPG is stored as .png");
        assert_eq!((a.kind, a.mime.as_str(), a.name.as_str(), a.size), (Kind::Image, "image/png", "My Shot.png", 12));
        assert_eq!((std::fs::read(&a.path).unwrap(), mode(&a.path)), (png, 0o600));
        assert!(!up.join(&id).join(".part").exists(), "the part file was renamed");
        for d in [cache_root().join("uploads"), up.clone(), up.join(&id)] { assert_eq!(mode(&d), 0o700, "{d:?}"); }
        let mut t = Upload::begin("cdash-up-done", "notes.png", 5).await.unwrap();
        assert_eq!(t.write(b"hello").await, Ok(true));
        let (_, f) = t.finish().await.unwrap();
        assert_eq!((f.kind, f.mime.as_str(), f.name.as_str()), (Kind::File, "application/octet-stream", "notes.png"), "a text file named .png stays a file");
        let _ = std::fs::remove_dir_all(&up);
    }
    #[test]
    fn a_trimmed_long_name_never_ends_in_a_space() {
        let n = sanitize_name(&format!("{} {}.pdf", "a".repeat(95), "b".repeat(20)));
        assert!(n.len() <= 100 && n.ends_with(".pdf") && !n.contains(" .pdf"), "{n:?}");
    }
}
