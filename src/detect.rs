use std::path::{Path, PathBuf};

pub const BACKENDS: &[&str] = &["cuda", "metal", "vulkan", "hip", "cpu"];

/// GGUF component files in `dir` starting with `prefix`, ordered by quality.
pub fn component_files(dir: &Path, prefix: &str) -> Vec<String> {
    let rank = |n: &str| -> u8 {
        match () {
            _ if n.contains("q4_k") => 0,
            _ if n.contains("q8_0") => 1,
            _ if n.contains("q4_0") => 2,
            _ if n.contains("bf16") => 3,
            _ => 4,
        }
    };
    let mut v: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with(prefix) && n.ends_with(".gguf") {
                v.push(n);
            }
        }
    }
    v.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    v
}

/// Resolved paths/components for a local audio.cpp + MiniMax-Music3 install.
#[derive(Clone)]
pub struct Paths {
    pub bin: PathBuf,
    pub model_dir: PathBuf,
    pub backend: String,
    pub language_model: Option<String>,
    pub flow_transformer: Option<String>,
    pub depth_decoder: Option<String>,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else if p == "~" {
        home()
    } else {
        PathBuf::from(p)
    }
}

fn detect_bin(override_bin: Option<PathBuf>) -> PathBuf {
    if let Some(b) = override_bin {
        return b;
    }
    // Prefer the fastest backend present, mirroring the updater's build dirs.
    let builds = [
        "linux-cuda-release",
        "macos-metal-release",
        "linux-vulkan-release",
        "linux-hip-release",
        "linux-cpu-release",
        "macos-cpu-release",
    ];
    for b in builds {
        let p = home()
            .join("audio.cpp/build")
            .join(b)
            .join("bin/audiocpp_cli");
        if p.exists() {
            return p;
        }
    }
    home().join("audio.cpp/build/linux-cuda-release/bin/audiocpp_cli")
}

fn backend_from_bin(bin: &Path) -> String {
    let s = bin.to_string_lossy();
    if s.contains("cuda") {
        "cuda"
    } else if s.contains("metal") {
        "metal"
    } else if s.contains("vulkan") {
        "vulkan"
    } else if s.contains("hip") {
        "hip"
    } else {
        "cpu"
    }
    .to_string()
}

fn detect_model_dir(override_dir: Option<PathBuf>) -> PathBuf {
    if let Some(d) = override_dir {
        return d;
    }
    let direct = home().join("models/MiniMax-Music3-GGUF");
    if direct.is_dir() {
        return direct;
    }
    // Fall back to any *MiniMax-Music* directory under ~/models.
    if let Ok(entries) = std::fs::read_dir(home().join("models")) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains("minimax") && name.contains("music") && e.path().is_dir() {
                return e.path();
            }
        }
    }
    direct
}

fn pick(dir: &Path, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|c| dir.join(c).exists())
        .map(|c| c.to_string())
}

pub fn detect(
    override_bin: Option<PathBuf>,
    override_model: Option<PathBuf>,
    override_backend: Option<String>,
) -> Paths {
    let bin = detect_bin(override_bin);
    let backend = override_backend.unwrap_or_else(|| backend_from_bin(&bin));
    let model_dir = detect_model_dir(override_model);
    let language_model = pick(
        &model_dir,
        &[
            "language_model_q4_k.gguf",
            "language_model_q4_0.gguf",
            "language_model_q8_0.gguf",
            "language_model_bf16.gguf",
        ],
    );
    let flow_transformer = pick(
        &model_dir,
        &[
            "transformer_q4_k.gguf",
            "transformer_q4_0.gguf",
            "transformer_q8_0.gguf",
            "transformer_bf16.gguf",
        ],
    );
    let depth_decoder = pick(
        &model_dir,
        &[
            "rvq_depth_decoder_q8_0.gguf",
            "rvq_depth_decoder_q4_k.gguf",
            "rvq_depth_decoder_bf16.gguf",
        ],
    );
    Paths {
        bin,
        model_dir,
        backend,
        language_model,
        flow_transformer,
        depth_decoder,
    }
}

/// Human-readable sanity check used by the TUI sidebar and `--dry-run`.
pub fn problems(paths: &Paths) -> Vec<String> {
    let mut out = Vec::new();
    if !paths.bin.exists() {
        out.push(format!("audiocpp_cli not found at {}", paths.bin.display()));
    }
    if !paths.model_dir.is_dir() {
        out.push(format!("model dir missing: {}", paths.model_dir.display()));
    } else {
        for (prefix, name) in [
            ("language_model_", "language model"),
            ("transformer_", "flow transformer"),
            ("rvq_depth_decoder_", "depth decoder"),
        ] {
            if component_files(&paths.model_dir, prefix).is_empty() {
                out.push(format!("no {name} GGUF in model dir"));
            }
        }
        for f in ["condition_encoder.gguf", "vocoder.gguf", "config.json"] {
            if !paths.model_dir.join(f).exists() {
                out.push(format!("missing {f}"));
            }
        }
    }
    out
}
