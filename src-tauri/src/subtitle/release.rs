use serde::Deserialize;

/// One downloadable file from a GitHub release. `digest` keeps its `sha256:` prefix.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub digest: String,
    pub url: String,
}

#[derive(Deserialize)]
struct RawAsset {
    name: String,
    #[serde(default)]
    size: u64,
    digest: Option<String>,
    browser_download_url: Option<String>,
}

#[derive(Deserialize)]
struct RawRelease {
    assets: Vec<RawAsset>,
}

/// Parses a release's JSON. Files without a digest are dropped: without one the download
/// cannot be verified, so it must never be picked.
pub fn parse_assets(json: &str) -> Result<Vec<Asset>, String> {
    let release: RawRelease = serde_json::from_str(json).map_err(|e| e.to_string())?;
    Ok(release
        .assets
        .into_iter()
        .filter_map(|a| {
            Some(Asset {
                name: a.name,
                size: a.size,
                digest: a.digest.filter(|d| !d.is_empty())?,
                url: a.browser_download_url?,
            })
        })
        .collect())
}

const RUNTIME_PREFIX: &str = "sherpa-onnx-v";
const RUNTIME_SUFFIX: &str = "-win-x64-shared-MD-Release-no-tts.tar.bz2";
const MODEL_PREFIX: &str = "sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-";
const MODEL_SUFFIX: &str = ".tar.bz2";
const VAD_NAME: &str = "silero_vad.onnx";

pub fn pick_runtime(assets: &[Asset]) -> Option<Asset> {
    assets
        .iter()
        .find(|a| a.name.starts_with(RUNTIME_PREFIX) && a.name.ends_with(RUNTIME_SUFFIX))
        .cloned()
}

/// `YYYY-MM-DD`: ten characters, dashes at 4 and 7, digits elsewhere.
fn is_iso_date(s: &str) -> bool {
    s.len() == 10
        && s.char_indices().all(|(i, c)| match i {
            4 | 7 => c == '-',
            _ => c.is_ascii_digit(),
        })
}

/// The SenseVoice int8 package with the latest date, and that date.
pub fn pick_model(assets: &[Asset]) -> Option<(String, Asset)> {
    assets
        .iter()
        .filter_map(|a| {
            let date = a.name.strip_prefix(MODEL_PREFIX)?.strip_suffix(MODEL_SUFFIX)?;
            is_iso_date(date).then(|| (date.to_string(), a.clone()))
        })
        .max_by(|x, y| x.0.cmp(&y.0))
}

pub fn pick_vad(assets: &[Asset]) -> Option<Asset> {
    assets.iter().find(|a| a.name == VAD_NAME).cloned()
}

pub fn runtime_version(name: &str) -> Option<String> {
    let v = name.strip_prefix(RUNTIME_PREFIX)?.strip_suffix(RUNTIME_SUFFIX)?;
    (!v.is_empty()).then(|| v.to_string())
}

fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|part| part.parse().ok()).collect()
}

/// True when `candidate` is a strictly newer dotted version than `installed`; missing
/// trailing parts count as 0. Anything unparseable is never "newer".
pub fn version_newer(installed: &str, candidate: &str) -> bool {
    let (Some(a), Some(b)) = (parse_version(installed), parse_version(candidate)) else {
        return false;
    };
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return y > x;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            size: 1,
            digest: format!("sha256:{name}"),
            url: format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/x/{name}"),
        }
    }

    #[test]
    fn parse_drops_assets_without_digest() {
        let json = r#"{"assets":[
            {"name":"a.bin","size":10,"digest":"sha256:aa","browser_download_url":"https://x/a.bin"},
            {"name":"b.bin","size":20,"digest":null,"browser_download_url":"https://x/b.bin"},
            {"name":"c.bin","size":30,"browser_download_url":"https://x/c.bin"}
        ]}"#;
        let assets = parse_assets(json).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "a.bin");
        assert_eq!(assets[0].size, 10);
        assert_eq!(assets[0].digest, "sha256:aa");
        assert_eq!(assets[0].url, "https://x/a.bin");
    }

    #[test]
    fn parse_rejects_garbage_and_missing_assets() {
        assert!(parse_assets("not json").is_err());
        assert!(parse_assets(r#"{"tag_name":"v1"}"#).is_err());
    }

    #[test]
    fn pick_runtime_ignores_lib_debug_static_and_cuda_variants() {
        let assets = vec![
            asset("sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts-lib.tar.bz2"),
            asset("sherpa-onnx-v2.0.1-win-x64-shared-MD-Debug-no-tts.tar.bz2"),
            asset("sherpa-onnx-v2.0.1-win-x64-static-MD-Release-no-tts.tar.bz2"),
            asset("sherpa-onnx-v2.0.1-cuda-12.x-cudnn-9.x-onnxruntime1.28.2-win-x64-cuda.tar.bz2"),
            asset("sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts.tar.bz2"),
        ];
        assert_eq!(
            pick_runtime(&assets).unwrap().name,
            "sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts.tar.bz2"
        );
        assert!(pick_runtime(&assets[..4]).is_none());
    }

    #[test]
    fn pick_model_takes_latest_date() {
        let assets = vec![
            asset("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2"),
            asset("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.bz2"),
        ];
        let (date, a) = pick_model(&assets).unwrap();
        assert_eq!(date, "2025-09-09");
        assert!(a.name.ends_with("int8-2025-09-09.tar.bz2"));
    }

    #[test]
    fn pick_model_ignores_lookalikes() {
        let assets = vec![
            asset("sherpa-onnx-sense-voice-funasr-nano-int8-2025-12-17.tar.bz2"),
            asset("sherpa-onnx-qnn-5-seconds-sense-voice-zh-en-ja-ko-yue-2025-09-09-int8-linux-x64.tar.bz2"),
            asset("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-2025-09-09.tar.bz2"),
            asset("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-9-9.tar.bz2"),
            asset("sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09.tar.gz"),
        ];
        assert!(pick_model(&assets).is_none());
    }

    #[test]
    fn pick_vad_requires_exact_name() {
        let assets = vec![asset("silero_vad_v5.onnx"), asset("silero_vad.int8.onnx")];
        assert!(pick_vad(&assets).is_none());
        let with = vec![asset("silero_vad_v5.onnx"), asset("silero_vad.onnx")];
        assert_eq!(pick_vad(&with).unwrap().name, "silero_vad.onnx");
    }

    #[test]
    fn version_compare() {
        assert!(version_newer("2.0.0", "2.0.1"));
        assert!(version_newer("2.0.9", "2.0.10"));
        assert!(version_newer("1.13.8", "2.0.0"));
        assert!(!version_newer("2.0.0", "2.0"));
        assert!(!version_newer("2.0.1", "2.0.0"));
        assert!(!version_newer("2.0.1", "2.0.1"));
        assert!(!version_newer("x.y", "2.0.1"));
        assert!(!version_newer("2.0.1", "x.y"));
    }

    #[test]
    fn runtime_version_extraction() {
        assert_eq!(
            runtime_version("sherpa-onnx-v2.0.1-win-x64-shared-MD-Release-no-tts.tar.bz2"),
            Some("2.0.1".to_string())
        );
        assert_eq!(runtime_version("something-else.tar.bz2"), None);
    }
}
