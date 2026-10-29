use crate::memory::sibling_with_suffix;
use crate::subtitle::release::{version_newer, Asset};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const EXE_NAME: &str = "sherpa-onnx-vad-with-offline-asr.exe";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub runtime_version: String,
    pub runtime_digest: String,
    pub model_date: String,
    pub model_digest: String,
    pub vad_digest: String,
}

/// The newest files on offer, as picked from the releases.
#[derive(Clone, Debug)]
pub struct Latest {
    pub runtime: Asset,
    pub runtime_version: String,
    pub model: Asset,
    pub model_date: String,
    pub vad: Asset,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub enum UpdateState {
    NotInstalled,
    UpToDate,
    Available { runtime: bool, model: bool, vad: bool },
}

/// What differs between what is installed and what is on offer. Never a downgrade.
pub fn compare(installed: Option<&Manifest>, latest: &Latest) -> UpdateState {
    let Some(m) = installed else {
        return UpdateState::NotInstalled;
    };
    let runtime = version_newer(&m.runtime_version, &latest.runtime_version);
    let model = latest.model_date > m.model_date;
    let vad = m.vad_digest != latest.vad.digest;
    if runtime || model || vad {
        UpdateState::Available { runtime, model, vad }
    } else {
        UpdateState::UpToDate
    }
}

pub fn sha256_hex(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `expected` must look like `sha256:<hex>`; anything else is refused.
pub fn verify_digest(path: &Path, expected: &str) -> Result<(), String> {
    let want = expected
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("校验失败：不支持的摘要格式 {expected}"))?;
    let got = sha256_hex(path).map_err(|e| format!("校验失败：{e}"))?;
    if got.eq_ignore_ascii_case(want) {
        Ok(())
    } else {
        Err("校验失败：下载的文件与官方摘要不一致".to_string())
    }
}

#[derive(Serialize, Deserialize)]
struct ActivePointer {
    dir: String,
}

/// The install directory `active.json` points at, if it is a plain child of `root`
/// that holds a manifest.
pub fn active_dir(root: &Path) -> Option<PathBuf> {
    let bytes = fs::read(root.join("active.json")).ok()?;
    let pointer: ActivePointer = serde_json::from_slice(&bytes).ok()?;
    let mut parts = Path::new(&pointer.dir).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(_)), None) => {}
        _ => return None,
    }
    let dir = root.join(&pointer.dir);
    dir.join("manifest.json").is_file().then_some(dir)
}

pub fn load_manifest(dir: &Path) -> Option<Manifest> {
    serde_json::from_slice(&fs::read(dir.join("manifest.json")).ok()?).ok()
}

/// The single top-level directory of an unpacked official archive.
fn only_child_dir(extracted: &Path) -> Result<PathBuf, String> {
    let mut dirs = fs::read_dir(extracted)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .map(|e| e.path());
    match (dirs.next(), dirs.next()) {
        (Some(only), None) => Ok(only),
        _ => Err("压缩包结构不符合预期".to_string()),
    }
}

/// Copies the recognizer exe plus every dll (from `bin/` and `lib/`) flat into `dest/runtime/`.
pub fn arrange_runtime(extracted: &Path, dest: &Path) -> Result<(), String> {
    let top = only_child_dir(extracted)?;
    if !top.join("bin").join(EXE_NAME).is_file() {
        return Err(format!("压缩包里没有 {EXE_NAME}"));
    }
    let out = dest.join("runtime");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    fs::copy(top.join("bin").join(EXE_NAME), out.join(EXE_NAME)).map_err(|e| e.to_string())?;
    for sub in ["bin", "lib"] {
        let Ok(entries) = fs::read_dir(top.join(sub)) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            let is_dll = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("dll"));
            if is_dll {
                fs::copy(&path, out.join(entry.file_name())).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// Copies the model, its tokens and the package's own `test_wavs/zh.wav` (as `selftest.wav`).
pub fn arrange_model(extracted: &Path, dest: &Path) -> Result<(), String> {
    let top = only_child_dir(extracted)?;
    let out = dest.join("model");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    for (from, to) in [
        ("model.int8.onnx", "model.int8.onnx"),
        ("tokens.txt", "tokens.txt"),
        ("test_wavs/zh.wav", "selftest.wav"),
    ] {
        let src = top.join(from);
        if !src.is_file() {
            return Err(format!("模型包里缺少 {from}"));
        }
        fs::copy(&src, out.join(to)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Unpacks with Windows' own `tar`. Both names are plain ASCII, relative to `cwd`: that works
/// from a working directory containing Chinese characters and spaces.
pub fn tar_extract(cwd: &Path, archive_name: &str, out_name: &str) -> Result<(), String> {
    let tar = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("tar.exe"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("tar"));
    let mut cmd = std::process::Command::new(tar);
    cmd.args(["-xjf", archive_name, "-C", out_name])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let output = cmd.output().map_err(|e| format!("无法运行 tar：{e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("解压失败：{}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

pub fn uninstall(root: &Path) -> io::Result<()> {
    match fs::remove_dir_all(root) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes leftovers of an interrupted install: every `*.tmp` directory and every `inst-*`
/// directory that is not the active one. Files at the top level are left alone.
pub fn cleanup_stale(root: &Path) {
    let active = active_dir(root);
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let stale = name.ends_with(".tmp") || (name.starts_with("inst-") && active.as_deref() != Some(&path));
        if stale {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

pub struct Hooks<'a> {
    /// Fetches the asset into the given path, reporting (done, total) bytes.
    pub download: &'a dyn Fn(&Asset, &Path, &dyn Fn(u64, u64)) -> Result<(), String>,
    /// Unpacks `archive` into `out_dir`.
    pub extract: &'a dyn Fn(&Path, &Path) -> Result<(), String>,
    /// Runs the recognizer inside the freshly built directory; `Ok` only if it produced text.
    pub selftest: &'a dyn Fn(&Path) -> Result<(), String>,
}

/// Deletes the directory when dropped, so every early return cleans up after itself.
struct TempDirGuard(PathBuf);

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn next_install_number(root: &Path) -> u32 {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name.strip_prefix("inst-")?;
            n.strip_suffix(".tmp").unwrap_or(n).parse::<u32>().ok()
        })
        .max()
        .map_or(1, |n| n + 1)
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("cancelled".to_string())
    } else {
        Ok(())
    }
}

/// Builds a complete new install next to the current one and switches to it only when every
/// step, including the self-test, has passed. On any failure the current install is untouched
/// and nothing is left behind.
///
/// Components `state` marks as unchanged are copied from the current install; the rest are
/// downloaded, verified against their official digest and unpacked.
pub fn install(
    root: &Path,
    latest: &Latest,
    state: &UpdateState,
    hooks: &Hooks,
    progress: &dyn Fn(&str, u64, u64),
    cancel: &AtomicBool,
) -> Result<Manifest, String> {
    check_cancel(cancel)?;
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let old_dir = active_dir(root);
    let old_manifest = old_dir.as_deref().and_then(load_manifest);

    let (want_runtime, want_model, want_vad) = match (state, &old_manifest) {
        (UpdateState::Available { runtime, model, vad }, Some(_)) => (*runtime, *model, *vad),
        (UpdateState::UpToDate, Some(_)) => return Ok(old_manifest.expect("checked above")),
        _ => (true, true, true),
    };

    let number = next_install_number(root);
    let tmp = root.join(format!("inst-{number}.tmp"));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let _guard = TempDirGuard(tmp.clone());
    let scratch = tmp.join("dl");
    fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;

    let reuse = |name: &str| -> Result<(), String> {
        let from = old_dir.as_ref().ok_or("没有可复用的旧版本")?.join(name);
        if from.is_dir() {
            copy_dir(&from, &tmp.join(name)).map_err(|e| e.to_string())
        } else {
            fs::copy(&from, tmp.join(name)).map(|_| ()).map_err(|e| e.to_string())
        }
    };

    let fetch = |asset: &Asset, stage: &str| -> Result<PathBuf, String> {
        check_cancel(cancel)?;
        let dest = scratch.join(&asset.name);
        (hooks.download)(asset, &dest, &|done, total| progress(stage, done, total))?;
        check_cancel(cancel)?;
        verify_digest(&dest, &asset.digest)?;
        Ok(dest)
    };

    let unpack = |archive: &Path, name: &str| -> Result<PathBuf, String> {
        let out = scratch.join(name);
        fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        (hooks.extract)(archive, &out)?;
        Ok(out)
    };

    if want_runtime {
        let archive = fetch(&latest.runtime, "runtime")?;
        arrange_runtime(&unpack(&archive, "runtime-x")?, &tmp)?;
    } else {
        reuse("runtime")?;
    }
    if want_model {
        let archive = fetch(&latest.model, "model")?;
        arrange_model(&unpack(&archive, "model-x")?, &tmp)?;
    } else {
        reuse("model")?;
    }
    if want_vad {
        let file = fetch(&latest.vad, "vad")?;
        fs::copy(&file, tmp.join("silero_vad.onnx")).map_err(|e| e.to_string())?;
    } else {
        reuse("silero_vad.onnx")?;
    }
    fs::create_dir_all(tmp.join("work")).map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&scratch);

    check_cancel(cancel)?;
    progress("selftest", 0, 1);
    (hooks.selftest)(&tmp)?;
    check_cancel(cancel)?;

    let manifest = Manifest {
        runtime_version: if want_runtime {
            latest.runtime_version.clone()
        } else {
            old_manifest.as_ref().map(|m| m.runtime_version.clone()).unwrap_or_default()
        },
        runtime_digest: if want_runtime {
            latest.runtime.digest.clone()
        } else {
            old_manifest.as_ref().map(|m| m.runtime_digest.clone()).unwrap_or_default()
        },
        model_date: if want_model {
            latest.model_date.clone()
        } else {
            old_manifest.as_ref().map(|m| m.model_date.clone()).unwrap_or_default()
        },
        model_digest: if want_model {
            latest.model.digest.clone()
        } else {
            old_manifest.as_ref().map(|m| m.model_digest.clone()).unwrap_or_default()
        },
        vad_digest: if want_vad {
            latest.vad.digest.clone()
        } else {
            old_manifest.as_ref().map(|m| m.vad_digest.clone()).unwrap_or_default()
        },
    };
    let json = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(tmp.join("manifest.json"), json).map_err(|e| e.to_string())?;

    let final_dir = root.join(format!("inst-{number}"));
    fs::rename(&tmp, &final_dir).map_err(|e| e.to_string())?;
    // `tmp` no longer exists after the rename, so the guard's cleanup is a harmless no-op.

    let pointer = serde_json::to_vec(&ActivePointer { dir: format!("inst-{number}") })
        .map_err(|e| e.to_string())?;
    let pointer_path = root.join("active.json");
    let pointer_tmp = sibling_with_suffix(&pointer_path, ".tmp");
    let switched = fs::write(&pointer_tmp, pointer).and_then(|_| fs::rename(&pointer_tmp, &pointer_path));
    if let Err(e) = switched {
        let _ = fs::remove_file(&pointer_tmp);
        let _ = fs::remove_dir_all(&final_dir);
        return Err(e.to_string());
    }
    if let Some(old) = old_dir {
        let _ = fs::remove_dir_all(old);
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::release::Asset;
    use sha2::{Digest, Sha256};
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};

    // ---------- helpers ----------

    fn hex_of(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    fn digest_of(bytes: &[u8]) -> String {
        format!("sha256:{}", hex_of(bytes))
    }

    fn asset_with(name: &str, bytes: &[u8]) -> Asset {
        Asset {
            name: name.to_string(),
            size: bytes.len() as u64,
            digest: digest_of(bytes),
            url: format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/x/{name}"),
        }
    }

    const RUNTIME_BYTES: &[u8] = b"runtime-archive";
    const MODEL_BYTES: &[u8] = b"model-archive";
    const VAD_BYTES: &[u8] = b"vad-file";

    fn latest(runtime_ver: &str, model_date: &str) -> Latest {
        Latest {
            runtime: asset_with("runtime.tar.bz2", RUNTIME_BYTES),
            runtime_version: runtime_ver.to_string(),
            model: asset_with("model.tar.bz2", MODEL_BYTES),
            model_date: model_date.to_string(),
            vad: asset_with("silero_vad.onnx", VAD_BYTES),
        }
    }

    fn manifest(runtime_ver: &str, model_date: &str) -> Manifest {
        Manifest {
            runtime_version: runtime_ver.to_string(),
            runtime_digest: digest_of(RUNTIME_BYTES),
            model_date: model_date.to_string(),
            model_digest: digest_of(MODEL_BYTES),
            vad_digest: digest_of(VAD_BYTES),
        }
    }

    /// Fake network/archive/self-test. `extract` lays out the official package shape.
    struct Fake {
        downloads: RefCell<Vec<String>>,
        corrupt: Cell<bool>,
        selftest_ok: Cell<bool>,
        cancel_during_download: Option<&'static AtomicBool>,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                downloads: RefCell::new(Vec::new()),
                corrupt: Cell::new(false),
                selftest_ok: Cell::new(true),
                cancel_during_download: None,
            }
        }

        fn download(&self, a: &Asset, dest: &Path, progress: &dyn Fn(u64, u64)) -> Result<(), String> {
            self.downloads.borrow_mut().push(a.name.clone());
            if let Some(flag) = self.cancel_during_download {
                flag.store(true, Ordering::Relaxed);
            }
            let bytes: &[u8] = if self.corrupt.get() {
                b"tampered"
            } else if a.name == "runtime.tar.bz2" {
                RUNTIME_BYTES
            } else if a.name == "model.tar.bz2" {
                MODEL_BYTES
            } else {
                VAD_BYTES
            };
            fs::write(dest, bytes).map_err(|e| e.to_string())?;
            progress(bytes.len() as u64, bytes.len() as u64);
            Ok(())
        }

        fn extract(&self, archive: &Path, out_dir: &Path) -> Result<(), String> {
            let name = archive.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("runtime") {
                let top = out_dir.join("sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts");
                fs::create_dir_all(top.join("bin")).map_err(|e| e.to_string())?;
                fs::create_dir_all(top.join("lib")).map_err(|e| e.to_string())?;
                fs::write(top.join("bin").join("sherpa-onnx-vad-with-offline-asr.exe"), b"exe").unwrap();
                fs::write(top.join("bin").join("sherpa-onnx-offline.exe"), b"other-exe").unwrap();
                fs::write(top.join("bin").join("onnxruntime.dll"), b"dll1").unwrap();
                fs::write(top.join("lib").join("sherpa-onnx-c-api.dll"), b"dll2").unwrap();
                fs::write(top.join("lib").join("sherpa-onnx-c-api.lib"), b"lib").unwrap();
            } else {
                let top = out_dir.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09");
                fs::create_dir_all(top.join("test_wavs")).map_err(|e| e.to_string())?;
                fs::write(top.join("model.int8.onnx"), b"model").unwrap();
                fs::write(top.join("tokens.txt"), b"tokens").unwrap();
                fs::write(top.join("test_wavs").join("zh.wav"), b"wav").unwrap();
            }
            Ok(())
        }

        fn selftest(&self, _install_dir: &Path) -> Result<(), String> {
            if self.selftest_ok.get() {
                Ok(())
            } else {
                Err("selftest produced no text".to_string())
            }
        }
    }

    fn run_install(
        root: &Path,
        fake: &Fake,
        latest: &Latest,
        state: &UpdateState,
        cancel: &AtomicBool,
    ) -> Result<Manifest, String> {
        let download = |a: &Asset, d: &Path, p: &dyn Fn(u64, u64)| fake.download(a, d, p);
        let extract = |a: &Path, o: &Path| fake.extract(a, o);
        let selftest = |d: &Path| fake.selftest(d);
        let hooks = Hooks { download: &download, extract: &extract, selftest: &selftest };
        install(root, latest, state, &hooks, &|_, _, _| {}, cancel)
    }

    fn dir_names(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    // ---------- compare ----------

    #[test]
    fn compare_covers_every_case() {
        let l = latest("2.0.1", "2025-09-09");
        assert_eq!(compare(None, &l), UpdateState::NotInstalled);
        assert_eq!(compare(Some(&manifest("2.0.1", "2025-09-09")), &l), UpdateState::UpToDate);
        assert_eq!(
            compare(Some(&manifest("2.0.0", "2025-09-09")), &l),
            UpdateState::Available { runtime: true, model: false, vad: false }
        );
        assert_eq!(
            compare(Some(&manifest("2.0.1", "2024-07-17")), &l),
            UpdateState::Available { runtime: false, model: true, vad: false }
        );
        let mut m = manifest("2.0.1", "2025-09-09");
        m.vad_digest = "sha256:other".to_string();
        assert_eq!(
            compare(Some(&m), &l),
            UpdateState::Available { runtime: false, model: false, vad: true }
        );
        // an installed version newer than the latest is never a downgrade
        assert_eq!(compare(Some(&manifest("2.0.5", "2026-01-01")), &l), UpdateState::UpToDate);
    }

    // ---------- digest ----------

    #[test]
    fn verify_digest_cases() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.bin");
        fs::write(&f, b"hello world").unwrap();
        let good = digest_of(b"hello world");
        assert!(verify_digest(&f, &good).is_ok());
        assert!(verify_digest(&f, &good.to_uppercase().replace("SHA256:", "sha256:")).is_ok());
        assert!(verify_digest(&f, &digest_of(b"other")).is_err());
        assert!(verify_digest(&f, &hex_of(b"hello world")).is_err(), "prefix is required");
        assert!(verify_digest(&f, "md5:abc").is_err());
        assert!(verify_digest(&dir.path().join("missing"), &good).is_err());
    }

    // ---------- arrange ----------

    #[test]
    fn arrange_runtime_takes_only_the_exe_and_dlls() {
        let fake = Fake::new();
        let dir = tempfile::tempdir().unwrap();
        let extracted = dir.path().join("x");
        fs::create_dir_all(&extracted).unwrap();
        fake.extract(Path::new("runtime.tar.bz2"), &extracted).unwrap();
        let dest = dir.path().join("dest");
        arrange_runtime(&extracted, &dest).unwrap();
        let names = dir_names(&dest.join("runtime"));
        assert_eq!(
            names,
            vec![
                "onnxruntime.dll".to_string(),
                "sherpa-onnx-c-api.dll".to_string(),
                "sherpa-onnx-vad-with-offline-asr.exe".to_string()
            ]
        );
    }

    #[test]
    fn arrange_runtime_errors_without_the_exe() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("x").join("pkg").join("bin");
        fs::create_dir_all(&top).unwrap();
        fs::write(top.join("onnxruntime.dll"), b"d").unwrap();
        assert!(arrange_runtime(&dir.path().join("x"), &dir.path().join("dest")).is_err());
    }

    #[test]
    fn arrange_model_requires_all_three_files() {
        let fake = Fake::new();
        let dir = tempfile::tempdir().unwrap();
        let extracted = dir.path().join("x");
        fs::create_dir_all(&extracted).unwrap();
        fake.extract(Path::new("model.tar.bz2"), &extracted).unwrap();
        let dest = dir.path().join("dest");
        arrange_model(&extracted, &dest).unwrap();
        assert_eq!(
            dir_names(&dest.join("model")),
            vec!["model.int8.onnx".to_string(), "selftest.wav".to_string(), "tokens.txt".to_string()]
        );

        let top = extracted.join("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09");
        fs::remove_file(top.join("tokens.txt")).unwrap();
        assert!(arrange_model(&extracted, &dir.path().join("dest2")).is_err());
    }

    // ---------- install transaction ----------

    #[test]
    fn fresh_install_creates_inst_1_and_activates_it() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new();
        let l = latest("2.0.1", "2025-09-09");
        let m = run_install(root.path(), &fake, &l, &UpdateState::NotInstalled, &AtomicBool::new(false)).unwrap();
        assert_eq!(m, manifest("2.0.1", "2025-09-09"));

        let active = active_dir(root.path()).unwrap();
        assert_eq!(active.file_name().unwrap(), "inst-1");
        assert_eq!(load_manifest(&active), Some(m));
        assert!(active.join("runtime").join("sherpa-onnx-vad-with-offline-asr.exe").exists());
        assert!(active.join("model").join("model.int8.onnx").exists());
        assert!(active.join("silero_vad.onnx").exists());
        assert!(active.join("work").is_dir());
        assert!(!dir_names(root.path()).iter().any(|n| n.ends_with(".tmp")));
        assert_eq!(fake.downloads.borrow().len(), 3);
    }

    #[test]
    fn partial_update_reuses_untouched_components() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new();
        let first = latest("2.0.1", "2024-07-17");
        run_install(root.path(), &fake, &first, &UpdateState::NotInstalled, &AtomicBool::new(false)).unwrap();
        let old = active_dir(root.path()).unwrap();

        let second = latest("2.0.1", "2025-09-09");
        let fake2 = Fake::new();
        let state = UpdateState::Available { runtime: false, model: true, vad: false };
        let m = run_install(root.path(), &fake2, &second, &state, &AtomicBool::new(false)).unwrap();

        assert_eq!(*fake2.downloads.borrow(), vec!["model.tar.bz2".to_string()]);
        let active = active_dir(root.path()).unwrap();
        assert_eq!(active.file_name().unwrap(), "inst-2");
        assert_eq!(m.model_date, "2025-09-09");
        assert!(active.join("runtime").join("sherpa-onnx-vad-with-offline-asr.exe").exists());
        assert!(active.join("silero_vad.onnx").exists());
        assert!(!old.exists(), "the replaced install is removed");
    }

    fn assert_old_install_intact(root: &Path, old: &Path, old_manifest: &Manifest) {
        assert_eq!(active_dir(root).as_deref(), Some(old));
        assert_eq!(load_manifest(old).as_ref(), Some(old_manifest));
        assert!(old.join("runtime").join("sherpa-onnx-vad-with-offline-asr.exe").exists());
        assert!(!dir_names(root).iter().any(|n| n.ends_with(".tmp")), "{:?}", dir_names(root));
    }

    fn installed_root() -> (tempfile::TempDir, PathBuf, Manifest) {
        let root = tempfile::tempdir().unwrap();
        let first = latest("2.0.0", "2024-07-17");
        let m = run_install(root.path(), &Fake::new(), &first, &UpdateState::NotInstalled, &AtomicBool::new(false)).unwrap();
        let old = active_dir(root.path()).unwrap();
        (root, old, m)
    }

    #[test]
    fn digest_mismatch_keeps_old_install() {
        let (root, old, old_m) = installed_root();
        let fake = Fake::new();
        fake.corrupt.set(true);
        let l = latest("2.0.1", "2025-09-09");
        let state = UpdateState::Available { runtime: true, model: true, vad: true };
        let err = run_install(root.path(), &fake, &l, &state, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("校验") || err.to_lowercase().contains("digest"), "{err}");
        assert_old_install_intact(root.path(), &old, &old_m);
    }

    #[test]
    fn selftest_failure_keeps_old_install() {
        let (root, old, old_m) = installed_root();
        let fake = Fake::new();
        fake.selftest_ok.set(false);
        let l = latest("2.0.1", "2025-09-09");
        let state = UpdateState::Available { runtime: true, model: true, vad: true };
        assert!(run_install(root.path(), &fake, &l, &state, &AtomicBool::new(false)).is_err());
        assert_old_install_intact(root.path(), &old, &old_m);
    }

    #[test]
    fn cancel_keeps_old_install() {
        static CANCEL: AtomicBool = AtomicBool::new(false);
        CANCEL.store(false, Ordering::Relaxed);
        let (root, old, old_m) = installed_root();
        let mut fake = Fake::new();
        fake.cancel_during_download = Some(&CANCEL);
        let l = latest("2.0.1", "2025-09-09");
        let state = UpdateState::Available { runtime: true, model: true, vad: true };
        let err = run_install(root.path(), &fake, &l, &state, &CANCEL).unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_old_install_intact(root.path(), &old, &old_m);
    }

    #[test]
    fn failed_first_install_leaves_nothing() {
        let root = tempfile::tempdir().unwrap();
        let fake = Fake::new();
        fake.selftest_ok.set(false);
        let l = latest("2.0.1", "2025-09-09");
        assert!(run_install(root.path(), &fake, &l, &UpdateState::NotInstalled, &AtomicBool::new(false)).is_err());
        assert!(!root.path().join("active.json").exists());
        assert!(dir_names(root.path()).is_empty(), "{:?}", dir_names(root.path()));
        assert!(active_dir(root.path()).is_none());
    }

    // ---------- housekeeping ----------

    #[test]
    fn cleanup_stale_removes_tmp_and_inactive_only() {
        let (root, old, _m) = installed_root();
        fs::create_dir_all(root.path().join("inst-7.tmp").join("x")).unwrap();
        fs::create_dir_all(root.path().join("inst-3")).unwrap();
        fs::write(root.path().join("generated.json"), b"[]").unwrap();
        cleanup_stale(root.path());
        let names = dir_names(root.path());
        assert!(names.contains(&old.file_name().unwrap().to_string_lossy().into_owned()));
        assert!(names.contains(&"active.json".to_string()));
        assert!(names.contains(&"generated.json".to_string()), "unrelated files stay");
        assert!(!names.contains(&"inst-7.tmp".to_string()));
        assert!(!names.contains(&"inst-3".to_string()));
    }

    #[test]
    fn active_dir_is_none_when_target_is_missing_or_has_no_manifest() {
        let root = tempfile::tempdir().unwrap();
        assert!(active_dir(root.path()).is_none());
        fs::write(root.path().join("active.json"), r#"{"dir":"inst-9"}"#).unwrap();
        assert!(active_dir(root.path()).is_none(), "target missing");
        fs::create_dir_all(root.path().join("inst-9")).unwrap();
        assert!(active_dir(root.path()).is_none(), "no manifest.json");
        fs::write(root.path().join("active.json"), "{not json").unwrap();
        assert!(active_dir(root.path()).is_none(), "corrupt pointer");
    }

    #[test]
    fn active_dir_rejects_a_pointer_that_escapes_the_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("manifest.json"), b"{}").unwrap();
        let evil = format!(r#"{{"dir":"../{}"}}"#, outside.path().file_name().unwrap().to_string_lossy());
        fs::write(root.path().join("active.json"), evil).unwrap();
        assert!(active_dir(root.path()).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn tar_extract_unpacks_in_a_chinese_spaced_directory() {
        let base = tempfile::tempdir().unwrap();
        let cwd = base.path().join("中文 目录");
        fs::create_dir_all(cwd.join("src").join("top")).unwrap();
        fs::write(cwd.join("src").join("top").join("a.txt"), "hello").unwrap();
        let tar = Path::new(&std::env::var("SystemRoot").unwrap()).join("System32").join("tar.exe");
        let made = std::process::Command::new(tar)
            .args(["-cjf", "pkg.tar.bz2", "-C", "src", "top"])
            .current_dir(&cwd)
            .status()
            .unwrap();
        assert!(made.success());
        fs::create_dir_all(cwd.join("out")).unwrap();

        tar_extract(&cwd, "pkg.tar.bz2", "out").unwrap();

        assert_eq!(fs::read_to_string(cwd.join("out").join("top").join("a.txt")).unwrap(), "hello");
    }

    #[test]
    fn tar_extract_reports_a_missing_archive() {
        let base = tempfile::tempdir().unwrap();
        fs::create_dir_all(base.path().join("out")).unwrap();
        assert!(tar_extract(base.path(), "nope.tar.bz2", "out").is_err());
    }

    #[test]
    fn uninstall_removes_everything_and_is_idempotent() {
        let (root, _old, _m) = installed_root();
        let path = root.path().to_path_buf();
        uninstall(&path).unwrap();
        assert!(!path.exists());
        uninstall(&path).unwrap();
    }
}
