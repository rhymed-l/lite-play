use crate::memory::sibling_with_suffix;
use crate::subtitle::install::Latest;
use crate::subtitle::release::{parse_assets, pick_model, pick_runtime, pick_vad, runtime_version};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const DOWNLOAD_PREFIX: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/";
const RUNTIME_API: &str = "https://api.github.com/repos/k2-fsa/sherpa-onnx/releases/latest";
const MODELS_API: &str = "https://api.github.com/repos/k2-fsa/sherpa-onnx/releases/tags/asr-models";
const USER_AGENT: &str = "lite-play";
/// A release listing is a few hundred KB; this only guards against a runaway response.
const MAX_JSON_BYTES: u64 = 8 * 1024 * 1024;

/// Only files the official project publishes may be downloaded. The check applies to the
/// address taken from the release listing; ureq then follows GitHub's own redirect to its
/// download CDN, and the SHA-256 check after the download is what protects the content.
pub fn is_allowed_download_url(url: &str) -> bool {
    url.starts_with(DOWNLOAD_PREFIX)
}

/// Picks the runtime, model and VAD file out of the two release listings.
pub fn latest_from_json(runtime_release: &str, models_release: &str) -> Result<Latest, String> {
    let runtime_assets = parse_assets(runtime_release).map_err(|e| format!("无法解析发布信息：{e}"))?;
    let model_assets = parse_assets(models_release).map_err(|e| format!("无法解析发布信息：{e}"))?;

    let runtime = pick_runtime(&runtime_assets).ok_or("官方发布里没有找到识别程序")?;
    let runtime_version = runtime_version(&runtime.name).ok_or("识别程序的版本号无法识别")?;
    let (model_date, model) = pick_model(&model_assets).ok_or("官方发布里没有找到 SenseVoice 模型")?;
    let vad = pick_vad(&model_assets).ok_or("官方发布里没有找到 VAD 文件")?;

    for asset in [&runtime, &model, &vad] {
        if !is_allowed_download_url(&asset.url) {
            return Err(format!("下载地址不在官方发布范围内：{}", asset.name));
        }
    }
    Ok(Latest { runtime, runtime_version, model, model_date, vad })
}

/// How long a transfer may go without receiving a single byte before it is given up on.
const IDLE_LIMIT: Duration = Duration::from_secs(30);

/// Release listings are small, so the whole exchange is capped. Downloads are not: a 165 MB
/// model legitimately takes minutes, and `timeout_recv_body` would cut it off after that long
/// in total. They are only abandoned when no data arrives for `idle`.
fn agent(idle: Duration, cap_total: bool) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(idle));
    if cap_total {
        config = config.timeout_global(Some(Duration::from_secs(60)));
    }
    config.build().into()
}

fn get_json(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut response = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(403 | 429)) => return Err("GitHub 限流，稍后再试".to_string()),
        Err(e) => return Err(format!("无法访问 GitHub：{e}")),
    };
    response
        .body_mut()
        .with_config()
        .limit(MAX_JSON_BYTES)
        .read_to_string()
        .map_err(|e| e.to_string())
}

// DUAL-LOCAL EXCEPTION
// Boundary: User Client -> GitHub (api.github.com, public release metadata)
// Data Scope: a read-only GET for two release listings; no audio, no file names, no user data
// Necessity: the add-on is not bundled, so the newest file names and digests must be looked up
// Retention & Deletion: nothing is stored beyond the returned listing, which is dropped after use
// Zero-Trust Access: unauthenticated public read; every download is verified against its SHA-256
/// Looks up the newest runtime, model and VAD file on GitHub.
pub fn fetch_latest() -> Result<Latest, String> {
    let agent = agent(IDLE_LIMIT, true);
    let runtime = get_json(&agent, RUNTIME_API)?;
    let models = get_json(&agent, MODELS_API)?;
    latest_from_json(&runtime, &models)
}

// DUAL-LOCAL EXCEPTION
// Boundary: User Client -> GitHub (github.com release downloads and its download CDN)
// Data Scope: a read-only GET for one public file; nothing from this machine is sent
// Necessity: the speech recognizer and its model are downloaded on demand, not bundled
// Retention & Deletion: files live in subtitle-addon/ and are removed by "uninstall add-on"
// Zero-Trust Access: only release URLs under k2-fsa/sherpa-onnx; SHA-256 checked after download
/// Streams `url` into `dest` through a `.part` file, reporting (done, total) bytes.
pub fn download(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
) -> Result<(), String> {
    download_inner(url, dest, progress, cancel, true)
}

fn download_inner(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
    enforce_allow_list: bool,
) -> Result<(), String> {
    download_with_idle_limit(url, dest, progress, cancel, enforce_allow_list, IDLE_LIMIT)
}

fn download_with_idle_limit(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
    enforce_allow_list: bool,
    idle: Duration,
) -> Result<(), String> {
    if enforce_allow_list && !is_allowed_download_url(url) {
        return Err(format!("下载地址不在官方发布范围内：{url}"));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".to_string());
    }
    let part = sibling_with_suffix(dest, ".part");
    let result = stream_to(url, &part, progress, cancel, idle);
    match result {
        Ok(()) => fs::rename(&part, dest).map_err(|e| {
            let _ = fs::remove_file(&part);
            e.to_string()
        }),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn stream_to(
    url: &str,
    part: &Path,
    progress: &dyn Fn(u64, u64),
    cancel: &AtomicBool,
    idle: Duration,
) -> Result<(), String> {
    let agent = agent(idle, false);
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| format!("下载失败：{e}"))?;
    let total = response.body().content_length().unwrap_or(0);
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("下载中断：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        progress(done, total);
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_string());
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    if total > 0 && done != total {
        return Err(format!("下载不完整：{done}/{total} 字节"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicBool;

    const RUNTIME_NAME: &str = "sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts.tar.bz2";
    const MODEL_2024: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2";
    const MODEL_2025: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2";
    const BASE: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";

    fn asset_json(name: &str, digest: Option<&str>, url: &str) -> String {
        let digest = digest.map_or("null".to_string(), |d| format!("\"{d}\""));
        format!(r#"{{"name":"{name}","size":5,"digest":{digest},"browser_download_url":"{url}"}}"#)
    }

    fn release(assets: &[String]) -> String {
        format!(r#"{{"assets":[{}]}}"#, assets.join(","))
    }

    fn runtime_release() -> String {
        release(&[
            asset_json("sherpa-onnx-v2.0.1-win-x64-shared-MD-Debug-no-tts.tar.bz2", Some("sha256:aa"), &format!("{BASE}/v2.0.1/debug.tar.bz2")),
            asset_json(RUNTIME_NAME, Some("sha256:bb"), &format!("{BASE}/v2.0.1/{RUNTIME_NAME}")),
        ])
    }

    fn models_release() -> String {
        release(&[
            asset_json(MODEL_2024, Some("sha256:cc"), &format!("{BASE}/asr-models/{MODEL_2024}")),
            asset_json(MODEL_2025, Some("sha256:dd"), &format!("{BASE}/asr-models/{MODEL_2025}")),
            asset_json("silero_vad_v5.onnx", None, &format!("{BASE}/asr-models/silero_vad_v5.onnx")),
            asset_json("silero_vad.onnx", Some("sha256:ee"), &format!("{BASE}/asr-models/silero_vad.onnx")),
        ])
    }

    #[test]
    fn allowed_urls() {
        assert!(is_allowed_download_url(&format!("{BASE}/v2.0.1/x.tar.bz2")));
        assert!(!is_allowed_download_url("http://github.com/k2-fsa/sherpa-onnx/releases/download/x"));
        assert!(!is_allowed_download_url("https://github.com/other/repo/releases/download/x"));
        assert!(!is_allowed_download_url(
            "https://evil.example/https://github.com/k2-fsa/sherpa-onnx/releases/download/"
        ));
        assert!(!is_allowed_download_url("https://github.com.evil.example/k2-fsa/sherpa-onnx/releases/download/x"));
        assert!(!is_allowed_download_url(""));
    }

    #[test]
    fn latest_from_json_picks_all_three() {
        let l = latest_from_json(&runtime_release(), &models_release()).unwrap();
        assert_eq!(l.runtime.name, RUNTIME_NAME);
        assert_eq!(l.runtime_version, "2.0.1");
        assert_eq!(l.model.name, MODEL_2025);
        assert_eq!(l.model_date, "2025-09-09");
        assert_eq!(l.vad.name, "silero_vad.onnx");
        assert_eq!(l.vad.digest, "sha256:ee");
    }

    #[test]
    fn newest_model_without_digest_is_skipped() {
        let models = release(&[
            asset_json(MODEL_2024, Some("sha256:cc"), &format!("{BASE}/asr-models/{MODEL_2024}")),
            asset_json(MODEL_2025, None, &format!("{BASE}/asr-models/{MODEL_2025}")),
            asset_json("silero_vad.onnx", Some("sha256:ee"), &format!("{BASE}/asr-models/silero_vad.onnx")),
        ]);
        let l = latest_from_json(&runtime_release(), &models).unwrap();
        assert_eq!(l.model_date, "2024-07-17", "an unverifiable file is never picked");
    }

    #[test]
    fn latest_from_json_reports_what_is_missing() {
        let no_model = release(&[asset_json(
            "silero_vad.onnx",
            Some("sha256:ee"),
            &format!("{BASE}/asr-models/silero_vad.onnx"),
        )]);
        assert!(latest_from_json(&runtime_release(), &no_model).unwrap_err().contains("模型"));

        let no_vad = release(&[asset_json(MODEL_2025, Some("sha256:dd"), &format!("{BASE}/asr-models/{MODEL_2025}"))]);
        assert!(latest_from_json(&runtime_release(), &no_vad).unwrap_err().contains("VAD"));

        let no_runtime = release(&[]);
        assert!(latest_from_json(&no_runtime, &models_release()).unwrap_err().contains("识别程序"));
    }

    #[test]
    fn latest_from_json_rejects_foreign_download_url() {
        let runtime = release(&[asset_json(RUNTIME_NAME, Some("sha256:bb"), "https://evil.example/x.tar.bz2")]);
        let err = latest_from_json(&runtime, &models_release()).unwrap_err();
        assert!(err.contains("下载地址"), "{err}");
    }

    #[test]
    fn latest_from_json_rejects_garbage() {
        assert!(latest_from_json("nope", &models_release()).is_err());
        assert!(latest_from_json(&runtime_release(), "nope").is_err());
    }

    /// Serves one fixed 200 response on a local port, then stops.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = stream.read(&mut req);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        });
        format!("http://127.0.0.1:{port}/file.bin")
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn download_streams_to_part_then_renames() {
        let body: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve_once(body.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let last = Cell::new((0u64, 0u64));
        download_inner(&url, &dest, &|done, total| last.set((done, total)), &AtomicBool::new(false), false)
            .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert_eq!(last.get(), (200_000, 200_000));
        assert_eq!(names(dir.path()), vec!["file.bin".to_string()], "no .part left behind");
    }

    /// Sends `chunks` pieces of `chunk_len` bytes, pausing `pause_ms` between pieces.
    fn serve_slowly(chunks: usize, chunk_len: usize, pause_ms: u64) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let _ = stream.read(&mut req);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                chunks * chunk_len
            );
            let _ = stream.write_all(head.as_bytes());
            for _ in 0..chunks {
                let _ = stream.write_all(&vec![9u8; chunk_len]);
                let _ = stream.flush();
                std::thread::sleep(Duration::from_millis(pause_ms));
            }
        });
        format!("http://127.0.0.1:{port}/slow.bin")
    }

    #[test]
    fn a_download_longer_than_the_idle_limit_still_completes_while_data_keeps_flowing() {
        // 8 pieces, 150 ms apart: about 1.2 s in total. With an idle limit of 400 ms every read
        // succeeds, so the download must not be cut off by the total time.
        let url = serve_slowly(8, 1000, 150);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("slow.bin");
        let started = std::time::Instant::now();
        download_with_idle_limit(&url, &dest, &|_, _| {}, &AtomicBool::new(false), false, Duration::from_millis(400))
            .unwrap();
        assert!(started.elapsed() > Duration::from_millis(900), "the test must really be slow");
        assert_eq!(fs::metadata(&dest).unwrap().len(), 8000);
    }

    #[test]
    fn a_stalled_download_fails_after_the_idle_limit() {
        // one piece, then the server goes quiet for 3 s
        let url = serve_slowly(2, 1000, 3000);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("slow.bin");
        let started = std::time::Instant::now();
        let err = download_with_idle_limit(&url, &dest, &|_, _| {}, &AtomicBool::new(false), false, Duration::from_millis(400))
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(2500), "gave up after the idle limit, not after the server");
        assert!(err.contains("下载中断") || err.contains("下载失败"), "{err}");
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn download_cancel_removes_part_file() {
        let url = serve_once(vec![7u8; 4 * 1024 * 1024]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let cancel = AtomicBool::new(false);
        let err = download_inner(
            &url,
            &dest,
            &|_, _| cancel.store(true, std::sync::atomic::Ordering::Relaxed),
            &cancel,
            false,
        )
        .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn download_refuses_a_foreign_host_without_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let err = download("http://127.0.0.1:1/file.bin", &dest, &|_, _| {}, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("下载地址"), "{err}");
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn download_http_error_leaves_nothing() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        });
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("file.bin");
        let url = format!("http://127.0.0.1:{port}/missing");
        assert!(download_inner(&url, &dest, &|_, _| {}, &AtomicBool::new(false), false).is_err());
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    #[ignore = "needs network"]
    fn live_download_of_the_vad_file_matches_its_official_digest() {
        let latest = fetch_latest().expect("fetch_latest");
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("silero_vad.onnx");
        let last = Cell::new((0u64, 0u64));
        download(&latest.vad.url, &dest, &|d, t| last.set((d, t)), &AtomicBool::new(false))
            .expect("download");
        crate::subtitle::install::verify_digest(&dest, &latest.vad.digest).expect("digest");
        assert_eq!(fs::metadata(&dest).unwrap().len(), latest.vad.size);
        eprintln!("downloaded {} bytes, progress {:?}", latest.vad.size, last.get());
    }

    #[test]
    #[ignore = "needs network"]
    fn live_latest() {
        let l = fetch_latest().expect("fetch_latest");
        assert!(l.model_date.as_str() >= "2025-09-09", "{}", l.model_date);
        assert!(l.runtime.digest.starts_with("sha256:"));
    }
}
