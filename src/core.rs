use crate::detect::{self, Paths, expand_tilde};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const GENRES: &[&str] = &[
    "bright pop rock with clean drums and crisp rhythm guitars",
    "mellow lo-fi hip hop with warm vinyl crackle and dusty drums",
    "epic cinematic orchestral score with soaring strings and big percussion",
    "retro synthwave with pulsing analog bass and gated 80s drums",
    "intimate acoustic ballad with fingerpicked guitar and soft piano",
    "high-energy EDM festival anthem with a hard synth drop",
    "smooth late-night jazz with brushed drums and a walking bass",
    "custom...",
];

pub const MOODS: &[&str] = &[
    "uplifting",
    "melancholic",
    "energetic",
    "calm",
    "dark",
    "romantic",
    "dreamy",
    "defiant",
];

pub const VOCALS: &[&str] = &[
    "a clear female vocal",
    "a warm male vocal",
    "a duet with male and female vocals",
    "no vocals (instrumental)",
];

pub const DEFAULT_LYRICS: &str = "[verse]\nCity lights are shining low\nI keep moving with the glow\n[chorus]\nTurn it up and let it fly\nSing the melody tonight";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    // prompt
    pub genre: usize,
    pub genre_custom: String,
    pub mood: usize,
    pub vocals: usize,
    pub caption_override: String,
    // lyrics
    pub lyrics: String,
    // generation
    pub duration_sec: f64,
    pub num_inference_steps: u32,
    pub guidance_scale: f64,
    pub ar_guidance_scale: f64,
    pub top_k: u32,
    pub seed: u64,
    // performance / memory
    pub flow_uncond_interval: u32,
    pub flow_uncond_warmup: u32,
    pub flow_chunk_hop_frames: u32,
    pub ensemble_takes: u32,
    pub ensemble_prefix_frames: u32,
    pub mem_saver: bool,
    pub pipeline_overlap: bool,
    pub graph_context_mb: u32,
    pub weight_context_mb: u32,
    // component selection ("" = auto)
    pub lm_gguf: String,
    pub flow_gguf: String,
    pub depth_gguf: String,
    // io
    pub backend: String,
    pub out: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            genre: 0,
            genre_custom: String::new(),
            mood: 0,
            vocals: 0,
            caption_override: String::new(),
            lyrics: DEFAULT_LYRICS.to_string(),
            duration_sec: 30.0,
            num_inference_steps: 30,
            guidance_scale: 1.7,
            ar_guidance_scale: 1.5,
            top_k: 50,
            seed: 0,
            flow_uncond_interval: 1,
            flow_uncond_warmup: 2,
            flow_chunk_hop_frames: 0,
            ensemble_takes: 1,
            ensemble_prefix_frames: 0,
            mem_saver: true,
            pipeline_overlap: false,
            graph_context_mb: 32,
            weight_context_mb: 32,
            lm_gguf: String::new(),
            flow_gguf: String::new(),
            depth_gguf: String::new(),
            backend: String::new(),
            out: default_out(),
        }
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn default_out() -> String {
    format!("~/Music/lyra-{}.wav", epoch_secs())
}

pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl Config {
    /// Clamped style index (tolerates hand-edited presets / API input).
    pub fn genre_index(&self) -> usize {
        self.genre.min(GENRES.len().saturating_sub(1))
    }

    pub fn mood_index(&self) -> usize {
        self.mood.min(MOODS.len().saturating_sub(1))
    }

    pub fn vocals_index(&self) -> usize {
        self.vocals.min(VOCALS.len().saturating_sub(1))
    }

    pub fn genre_text(&self) -> String {
        if self.is_custom_genre() {
            if self.genre_custom.trim().is_empty() {
                "an original song".to_string()
            } else {
                self.genre_custom.trim().to_string()
            }
        } else {
            GENRES[self.genre_index()].to_string()
        }
    }

    pub fn is_custom_genre(&self) -> bool {
        self.genre_index() + 1 == GENRES.len()
    }

    pub fn is_instrumental(&self) -> bool {
        self.vocals_index() + 1 == VOCALS.len()
    }

    pub fn caption(&self) -> String {
        if !self.caption_override.trim().is_empty() {
            return self.caption_override.trim().to_string();
        }
        let genre = capitalize_first(self.genre_text().trim());
        let mut c = format!(
            "{genre} — {} mood, {}, polished studio production.",
            MOODS[self.mood_index()],
            VOCALS[self.vocals_index()],
        );
        if self.is_instrumental() {
            c.push_str(" Focus on the melody and arrangement.");
        }
        c
    }

    /// Normalise indices and numeric ranges after loading from disk or an API.
    pub fn sanitize(&mut self) {
        self.genre = self.genre_index();
        self.mood = self.mood_index();
        self.vocals = self.vocals_index();
        self.duration_sec = clamp_f64(self.duration_sec, 1.0, 3600.0);
        self.num_inference_steps = self.num_inference_steps.clamp(1, 500);
        self.guidance_scale = clamp_f64(self.guidance_scale, 0.0, 20.0);
        self.ar_guidance_scale = clamp_f64(self.ar_guidance_scale, 0.0, 20.0);
        self.top_k = self.top_k.clamp(1, 2000);
        self.flow_uncond_interval = self.flow_uncond_interval.clamp(1, 20);
        self.flow_uncond_warmup = self.flow_uncond_warmup.min(20);
        self.ensemble_takes = self.ensemble_takes.clamp(1, 16);
        self.graph_context_mb = self.graph_context_mb.clamp(8, 4096);
        self.weight_context_mb = self.weight_context_mb.clamp(8, 4096);
        if !detect::BACKENDS.iter().any(|b| *b == self.backend) && !self.backend.is_empty() {
            self.backend.clear();
        }
    }

    pub fn lyrics_text(&self) -> String {
        let t = self.lyrics.trim();
        if self.is_instrumental() && !t.to_lowercase().contains("instrumental") {
            format!("[instrumental]\n{t}")
        } else if t.is_empty() {
            DEFAULT_LYRICS.to_string()
        } else {
            t.to_string()
        }
    }

    pub fn out_path(&self) -> String {
        expand_tilde(self.out.trim()).to_string_lossy().into_owned()
    }

    /// Fatal problems that make a run impossible (missing runtime/weights, bad
    /// values). Generation is refused while this is non-empty.
    pub fn errors(&self, paths: &Paths) -> Vec<String> {
        let mut e = Vec::new();
        if self.duration_sec < 1.0 || !self.duration_sec.is_finite() {
            e.push("duration must be at least 1 second".into());
        }
        if self.num_inference_steps == 0 {
            e.push("steps must be >= 1".into());
        }
        if self.out.trim().is_empty() {
            e.push("output path is empty".into());
        }
        e.extend(detect::problems(paths));
        e
    }

    /// Non-fatal advisories shown alongside the preview.
    pub fn warnings(&self, paths: &Paths) -> Vec<String> {
        let mut w = Vec::new();
        if self.pipeline_overlap && self.mem_saver {
            w.push("pipeline_overlap has no effect while mem_saver is on".into());
        }
        if self.is_instrumental() && self.lyrics.trim().is_empty() {
            w.push("instrumental selected — lyrics will be replaced with [instrumental]".into());
        }
        if self.duration_sec > 300.0 {
            w.push("very long duration — generation may take a long time".into());
        }
        w.extend(self.errors(paths));
        w
    }
}

fn clamp_f64(v: f64, lo: f64, hi: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { lo }
}

/// argv for `audiocpp_cli`; `[0]` is the binary.
pub fn build_argv(paths: &Paths, c: &Config) -> Vec<String> {
    let mut a: Vec<String> = vec![paths.bin.to_string_lossy().into_owned()];
    for s in ["--task", "gen", "--family", "minimax_music3", "--model"] {
        a.push(s.into());
    }
    a.push(paths.model_dir.to_string_lossy().into_owned());

    a.push("--backend".into());
    a.push(if c.backend.is_empty() {
        paths.backend.clone()
    } else {
        c.backend.clone()
    });

    let lm = pick(c.lm_gguf.clone(), paths.language_model.clone());
    let flow = pick(c.flow_gguf.clone(), paths.flow_transformer.clone());
    let depth = pick(c.depth_gguf.clone(), paths.depth_decoder.clone());
    if let Some(v) = lm {
        session(&mut a, "language_model_gguf", &v);
    }
    if let Some(v) = depth {
        session(&mut a, "rvq_depth_decoder_gguf", &v);
    }
    if let Some(v) = flow {
        session(&mut a, "flow_transformer_gguf", &v);
    }
    session(&mut a, "mem_saver", bool_str(c.mem_saver));
    session(&mut a, "pipeline_overlap", bool_str(c.pipeline_overlap));
    session(&mut a, "graph_context_mb", &c.graph_context_mb.to_string());
    session(
        &mut a,
        "weight_context_mb",
        &c.weight_context_mb.to_string(),
    );

    a.push("--text".into());
    a.push(c.caption());
    req(&mut a, "lyrics", &c.lyrics_text());
    req(&mut a, "duration_sec", &fmt_f64(c.duration_sec));
    req(
        &mut a,
        "num_inference_steps",
        &c.num_inference_steps.to_string(),
    );
    req(&mut a, "guidance_scale", &fmt_f64(c.guidance_scale));
    req(&mut a, "ar_guidance_scale", &fmt_f64(c.ar_guidance_scale));
    req(&mut a, "top_k", &c.top_k.to_string());
    req(&mut a, "seed", &c.seed.to_string());
    req(
        &mut a,
        "flow_uncond_interval",
        &c.flow_uncond_interval.to_string(),
    );
    req(
        &mut a,
        "flow_uncond_warmup",
        &c.flow_uncond_warmup.to_string(),
    );
    req(
        &mut a,
        "flow_chunk_hop_frames",
        &c.flow_chunk_hop_frames.to_string(),
    );
    req(&mut a, "ensemble_takes", &c.ensemble_takes.to_string());
    req(
        &mut a,
        "ensemble_prefix_frames",
        &c.ensemble_prefix_frames.to_string(),
    );

    a.push("--out".into());
    a.push(c.out_path());
    a.push("--metrics".into());
    a
}

fn pick(chosen: String, fallback: Option<String>) -> Option<String> {
    if !chosen.is_empty() {
        Some(chosen)
    } else {
        fallback
    }
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn fmt_f64(v: f64) -> String {
    if (v.fract()).abs() < f64::EPSILON {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn session(a: &mut Vec<String>, key: &str, val: &str) {
    a.push("--session-option".into());
    a.push(format!("minimax_music3.{key}={val}"));
}

fn req(a: &mut Vec<String>, key: &str, val: &str) {
    a.push("--request-option".into());
    a.push(format!("{key}={val}"));
}

pub fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "-_./=~".contains(ch))
            {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// --- field model (shared by TUI + web) --------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Song,
    Advanced,
    Components,
}

impl Group {
    pub fn as_str(self) -> &'static str {
        match self {
            Group::Song => "song",
            Group::Advanced => "advanced",
            Group::Components => "components",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Select,
    Bool,
    Int,
    Float,
    Text,
    Multiline,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Select => "select",
            Kind::Bool => "bool",
            Kind::Int => "int",
            Kind::Float => "float",
            Kind::Text => "text",
            Kind::Multiline => "multiline",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Genre,
    Mood,
    Vocals,
    Caption,
    Lyrics,
    Duration,
    Steps,
    Out,
    Guidance,
    ArGuidance,
    TopK,
    Seed,
    FlowInterval,
    FlowWarmup,
    FlowHop,
    EnsembleTakes,
    EnsemblePrefix,
    MemSaver,
    PipelineOverlap,
    GraphCtx,
    WeightCtx,
    Backend,
    Lm,
    Flow,
    Depth,
}

pub const SONG_FIELDS: &[Field] = &[
    Field::Genre,
    Field::Mood,
    Field::Vocals,
    Field::Caption,
    Field::Lyrics,
    Field::Duration,
    Field::Steps,
    Field::Out,
];

pub const ADVANCED_FIELDS: &[Field] = &[
    Field::Guidance,
    Field::ArGuidance,
    Field::TopK,
    Field::Seed,
    Field::FlowInterval,
    Field::FlowWarmup,
    Field::FlowHop,
    Field::EnsembleTakes,
    Field::EnsemblePrefix,
    Field::MemSaver,
    Field::PipelineOverlap,
    Field::GraphCtx,
    Field::WeightCtx,
];

pub const COMPONENT_FIELDS: &[Field] = &[Field::Backend, Field::Lm, Field::Flow, Field::Depth];

impl Field {
    pub fn id(self) -> &'static str {
        match self {
            Field::Genre => "genre",
            Field::Mood => "mood",
            Field::Vocals => "vocals",
            Field::Caption => "caption",
            Field::Lyrics => "lyrics",
            Field::Duration => "duration_sec",
            Field::Steps => "num_inference_steps",
            Field::Out => "out",
            Field::Guidance => "guidance_scale",
            Field::ArGuidance => "ar_guidance_scale",
            Field::TopK => "top_k",
            Field::Seed => "seed",
            Field::FlowInterval => "flow_uncond_interval",
            Field::FlowWarmup => "flow_uncond_warmup",
            Field::FlowHop => "flow_chunk_hop_frames",
            Field::EnsembleTakes => "ensemble_takes",
            Field::EnsemblePrefix => "ensemble_prefix_frames",
            Field::MemSaver => "mem_saver",
            Field::PipelineOverlap => "pipeline_overlap",
            Field::GraphCtx => "graph_context_mb",
            Field::WeightCtx => "weight_context_mb",
            Field::Backend => "backend",
            Field::Lm => "language_model_gguf",
            Field::Flow => "flow_transformer_gguf",
            Field::Depth => "rvq_depth_decoder_gguf",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::Genre => "Style",
            Field::Mood => "Mood",
            Field::Vocals => "Vocals",
            Field::Caption => "Caption override",
            Field::Lyrics => "Lyrics",
            Field::Duration => "Length (s)",
            Field::Steps => "Steps",
            Field::Out => "Output file",
            Field::Guidance => "Flow guidance",
            Field::ArGuidance => "AR guidance",
            Field::TopK => "Top-k",
            Field::Seed => "Seed",
            Field::FlowInterval => "Flow uncond interval",
            Field::FlowWarmup => "Flow uncond warmup",
            Field::FlowHop => "Flow chunk hop",
            Field::EnsembleTakes => "Ensemble takes",
            Field::EnsemblePrefix => "Ensemble prefix frames",
            Field::MemSaver => "Mem saver",
            Field::PipelineOverlap => "Pipeline overlap",
            Field::GraphCtx => "Graph ctx (MiB)",
            Field::WeightCtx => "Weight ctx (MiB)",
            Field::Backend => "Backend",
            Field::Lm => "Language model",
            Field::Flow => "Flow transformer",
            Field::Depth => "Depth decoder",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Field::Genre => "caption style; last item lets you type your own",
            Field::Mood => "mood word added to the caption",
            Field::Vocals => "vocal description; last item is instrumental",
            Field::Caption => "if set, overrides the composed caption entirely",
            Field::Lyrics => "song words; [verse]/[chorus]/[bridge]/[outro] tags allowed",
            Field::Duration => "AR frame budget in seconds — a cap, not a hard length",
            Field::Steps => "flow-matching Euler steps per chunk (lower = faster)",
            Field::Out => "where the .wav is written (~ = home)",
            Field::Guidance => "flow CFG scale; 0 disables CFG",
            Field::ArGuidance => "semantic/depth AR CFG scale; 0 disables",
            Field::TopK => "top-k sampling for semantic and residual codes",
            Field::Seed => "reroll the song by changing this",
            Field::FlowInterval => "evaluate flow uncond branch every N steps (2-3 = faster)",
            Field::FlowWarmup => "steps per chunk that always evaluate both CFG branches",
            Field::FlowHop => "flow chunk hop in AR frames (0 = model default; 150 = faster)",
            Field::EnsembleTakes => "generate K takes sharing one AR pass",
            Field::EnsemblePrefix => "shared AR prefix frames across takes (intro lock)",
            Field::MemSaver => "load big stages only when needed (lower peak VRAM)",
            Field::PipelineOverlap => "overlap AR with denoise/vocode (needs mem_saver off)",
            Field::GraphCtx => "runtime graph arena size",
            Field::WeightCtx => "weight context size",
            Field::Backend => "cuda / metal / vulkan / hip / cpu",
            Field::Lm => "language-model component GGUF",
            Field::Flow => "flow-transformer component GGUF",
            Field::Depth => "RVQ depth-decoder component GGUF",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Field::Genre
            | Field::Mood
            | Field::Vocals
            | Field::Backend
            | Field::Lm
            | Field::Flow
            | Field::Depth => Kind::Select,
            Field::MemSaver | Field::PipelineOverlap => Kind::Bool,
            Field::Duration | Field::Guidance | Field::ArGuidance => Kind::Float,
            Field::Caption | Field::Out => Kind::Text,
            Field::Lyrics => Kind::Multiline,
            _ => Kind::Int,
        }
    }

    pub fn group(self) -> Group {
        if SONG_FIELDS.contains(&self) {
            Group::Song
        } else if ADVANCED_FIELDS.contains(&self) {
            Group::Advanced
        } else {
            Group::Components
        }
    }

    pub fn from_id(id: &str) -> Option<Field> {
        ALL_FIELDS.iter().copied().find(|f| f.id() == id)
    }
}

pub const ALL_FIELDS: &[Field] = &[
    Field::Genre,
    Field::Mood,
    Field::Vocals,
    Field::Caption,
    Field::Lyrics,
    Field::Duration,
    Field::Steps,
    Field::Out,
    Field::Guidance,
    Field::ArGuidance,
    Field::TopK,
    Field::Seed,
    Field::FlowInterval,
    Field::FlowWarmup,
    Field::FlowHop,
    Field::EnsembleTakes,
    Field::EnsemblePrefix,
    Field::MemSaver,
    Field::PipelineOverlap,
    Field::GraphCtx,
    Field::WeightCtx,
    Field::Backend,
    Field::Lm,
    Field::Flow,
    Field::Depth,
];

/// Options for a Select field (dynamic for component files).
pub fn options(paths: &Paths, f: Field) -> Vec<String> {
    let slice = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
    match f {
        Field::Genre => slice(GENRES),
        Field::Mood => slice(MOODS),
        Field::Vocals => slice(VOCALS),
        Field::Backend => detect::BACKENDS.iter().map(|s| s.to_string()).collect(),
        Field::Lm => detect::component_files(&paths.model_dir, "language_model_"),
        Field::Flow => detect::component_files(&paths.model_dir, "transformer_"),
        Field::Depth => detect::component_files(&paths.model_dir, "rvq_depth_decoder_"),
        _ => Vec::new(),
    }
}

/// Current display string for a field.
pub fn value_string(c: &Config, f: Field) -> String {
    match f {
        Field::Genre => {
            if c.is_custom_genre() && !c.genre_custom.trim().is_empty() {
                c.genre_custom.clone()
            } else {
                c.genre_text()
            }
        }
        Field::Mood => MOODS[c.mood_index()].to_string(),
        Field::Vocals => VOCALS[c.vocals_index()].to_string(),
        Field::Caption => c.caption_override.clone(),
        Field::Lyrics => c.lyrics.clone(),
        Field::Duration => fmt_f64(c.duration_sec),
        Field::Steps => c.num_inference_steps.to_string(),
        Field::Out => c.out.clone(),
        Field::Guidance => fmt_f64(c.guidance_scale),
        Field::ArGuidance => fmt_f64(c.ar_guidance_scale),
        Field::TopK => c.top_k.to_string(),
        Field::Seed => c.seed.to_string(),
        Field::FlowInterval => c.flow_uncond_interval.to_string(),
        Field::FlowWarmup => c.flow_uncond_warmup.to_string(),
        Field::FlowHop => c.flow_chunk_hop_frames.to_string(),
        Field::EnsembleTakes => c.ensemble_takes.to_string(),
        Field::EnsemblePrefix => c.ensemble_prefix_frames.to_string(),
        Field::MemSaver => bool_str(c.mem_saver).to_string(),
        Field::PipelineOverlap => bool_str(c.pipeline_overlap).to_string(),
        Field::GraphCtx => c.graph_context_mb.to_string(),
        Field::WeightCtx => c.weight_context_mb.to_string(),
        Field::Backend => {
            if c.backend.is_empty() {
                "(auto)".to_string()
            } else {
                c.backend.clone()
            }
        }
        Field::Lm => {
            if c.lm_gguf.is_empty() {
                "(auto)".into()
            } else {
                c.lm_gguf.clone()
            }
        }
        Field::Flow => {
            if c.flow_gguf.is_empty() {
                "(auto)".into()
            } else {
                c.flow_gguf.clone()
            }
        }
        Field::Depth => {
            if c.depth_gguf.is_empty() {
                "(auto)".into()
            } else {
                c.depth_gguf.clone()
            }
        }
    }
}

/// Index of the selected option, when the field is a Select.
pub fn select_index(c: &Config, paths: &Paths, f: Field) -> usize {
    let opts = options(paths, f);
    match f {
        Field::Genre => c.genre.min(opts.len().saturating_sub(1)),
        Field::Mood => c.mood,
        Field::Vocals => c.vocals,
        Field::Backend => opts
            .iter()
            .position(|o| o == &c.backend)
            .map(|i| i + 1)
            .unwrap_or(0),
        Field::Lm => opts
            .iter()
            .position(|o| o == &c.lm_gguf)
            .map(|i| i + 1)
            .unwrap_or(0),
        Field::Flow => opts
            .iter()
            .position(|o| o == &c.flow_gguf)
            .map(|i| i + 1)
            .unwrap_or(0),
        Field::Depth => opts
            .iter()
            .position(|o| o == &c.depth_gguf)
            .map(|i| i + 1)
            .unwrap_or(0),
        _ => 0,
    }
}

/// Cycle a Select field. Backend/component selections include an "(auto)" slot.
pub fn cycle(c: &mut Config, paths: &Paths, f: Field, dir: i64) {
    let opts = options(paths, f);
    let auto = matches!(f, Field::Backend | Field::Lm | Field::Flow | Field::Depth);
    let n = opts.len() as i64 + if auto { 1 } else { 0 };
    if n == 0 {
        return;
    }
    let cur = select_index(c, paths, f) as i64;
    let next = (cur + dir).rem_euclid(n);
    match f {
        Field::Genre => c.genre = next.min(opts.len().saturating_sub(1) as i64) as usize,
        Field::Mood => c.mood = next as usize,
        Field::Vocals => c.vocals = next as usize,
        Field::Backend => {
            c.backend = if next == 0 {
                String::new()
            } else {
                opts[(next - 1) as usize].clone()
            }
        }
        Field::Lm => {
            c.lm_gguf = if next == 0 {
                String::new()
            } else {
                opts[(next - 1) as usize].clone()
            }
        }
        Field::Flow => {
            c.flow_gguf = if next == 0 {
                String::new()
            } else {
                opts[(next - 1) as usize].clone()
            }
        }
        Field::Depth => {
            c.depth_gguf = if next == 0 {
                String::new()
            } else {
                opts[(next - 1) as usize].clone()
            }
        }
        _ => {}
    }
}

pub fn adjust(c: &mut Config, paths: &Paths, f: Field, delta: i64) {
    let d = delta as f64;
    match f {
        Field::Duration => c.duration_sec = (c.duration_sec + d * 5.0).max(1.0),
        Field::Steps => {
            c.num_inference_steps = (c.num_inference_steps as i64 + delta).clamp(1, 500) as u32
        }
        Field::Guidance => c.guidance_scale = (c.guidance_scale + d * 0.1).clamp(0.0, 20.0),
        Field::ArGuidance => c.ar_guidance_scale = (c.ar_guidance_scale + d * 0.1).clamp(0.0, 20.0),
        Field::TopK => c.top_k = (c.top_k as i64 + delta).clamp(1, 2000) as u32,
        Field::Seed => c.seed = (c.seed as i64).saturating_add(delta).max(0) as u64,
        Field::FlowInterval => {
            c.flow_uncond_interval = (c.flow_uncond_interval as i64 + delta).clamp(1, 20) as u32
        }
        Field::FlowWarmup => {
            c.flow_uncond_warmup = (c.flow_uncond_warmup as i64 + delta).clamp(0, 20) as u32
        }
        Field::FlowHop => {
            c.flow_chunk_hop_frames = (c.flow_chunk_hop_frames as i64 + delta * 25).max(0) as u32
        }
        Field::EnsembleTakes => {
            c.ensemble_takes = (c.ensemble_takes as i64 + delta).clamp(1, 16) as u32
        }
        Field::EnsemblePrefix => {
            c.ensemble_prefix_frames = (c.ensemble_prefix_frames as i64 + delta * 25).max(0) as u32
        }
        Field::GraphCtx => {
            c.graph_context_mb = (c.graph_context_mb as i64 + delta * 8).clamp(8, 4096) as u32
        }
        Field::WeightCtx => {
            c.weight_context_mb = (c.weight_context_mb as i64 + delta * 8).clamp(8, 4096) as u32
        }
        _ => cycle(c, paths, f, delta),
    }
}

pub fn toggle(c: &mut Config, f: Field) {
    match f {
        Field::MemSaver => {
            c.mem_saver = !c.mem_saver;
            if c.mem_saver {
                c.pipeline_overlap = false;
            }
        }
        Field::PipelineOverlap => c.pipeline_overlap = !c.pipeline_overlap && !c.mem_saver,
        _ => {}
    }
}

/// Set a field from free text (used by editors and the web API).
pub fn set_text(c: &mut Config, f: Field, s: &str) {
    let t = s.trim();
    match f {
        Field::Genre => {
            c.genre = GENRES.len().saturating_sub(1);
            c.genre_custom = s.to_string();
        }
        Field::Caption => c.caption_override = s.to_string(),
        Field::Lyrics => c.lyrics = s.to_string(),
        Field::Out => c.out = s.to_string(),
        Field::Duration => {
            if let Ok(v) = t.parse::<f64>() {
                c.duration_sec = clamp_f64(v, 1.0, 3600.0);
            }
        }
        Field::Steps => {
            if let Ok(v) = t.parse::<i64>() {
                c.num_inference_steps = v.clamp(1, 500) as u32;
            }
        }
        Field::Guidance => {
            if let Ok(v) = t.parse::<f64>() {
                c.guidance_scale = clamp_f64(v, 0.0, 20.0);
            }
        }
        Field::ArGuidance => {
            if let Ok(v) = t.parse::<f64>() {
                c.ar_guidance_scale = clamp_f64(v, 0.0, 20.0);
            }
        }
        Field::TopK => {
            if let Ok(v) = t.parse::<i64>() {
                c.top_k = v.clamp(1, 2000) as u32;
            }
        }
        Field::Seed => {
            if let Ok(v) = t.parse::<u64>() {
                c.seed = v;
            }
        }
        Field::FlowInterval => {
            if let Ok(v) = t.parse::<i64>() {
                c.flow_uncond_interval = v.clamp(1, 20) as u32;
            }
        }
        Field::FlowWarmup => {
            if let Ok(v) = t.parse::<i64>() {
                c.flow_uncond_warmup = v.clamp(0, 20) as u32;
            }
        }
        Field::FlowHop => {
            if let Ok(v) = t.parse::<u32>() {
                c.flow_chunk_hop_frames = v;
            }
        }
        Field::EnsembleTakes => {
            if let Ok(v) = t.parse::<i64>() {
                c.ensemble_takes = v.clamp(1, 16) as u32;
            }
        }
        Field::EnsemblePrefix => {
            if let Ok(v) = t.parse::<u32>() {
                c.ensemble_prefix_frames = v;
            }
        }
        Field::GraphCtx => {
            if let Ok(v) = t.parse::<i64>() {
                c.graph_context_mb = v.clamp(8, 4096) as u32;
            }
        }
        Field::WeightCtx => {
            if let Ok(v) = t.parse::<i64>() {
                c.weight_context_mb = v.clamp(8, 4096) as u32;
            }
        }
        _ => {}
    }
}

/// Random 64-bit seed for the "reroll" action.
pub fn random_seed() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    // xorshift mix
    let mut x = t ^ 0x9e37_79b9_7f4a_7c15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

// --- presets / history ------------------------------------------------------

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| detect::home().join(".config"))
        .join("lyra")
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Option<T> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

fn write_json<T: Serialize>(path: &std::path::Path, v: &T) {
    let Some(parent) = path.parent() else { return };
    let _ = std::fs::create_dir_all(parent);
    let Ok(s) = serde_json::to_string_pretty(v) else {
        return;
    };
    // Write-then-rename so a crash can never leave a half-written state file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, s).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

pub fn load_presets() -> BTreeMap<String, Config> {
    let mut p: BTreeMap<String, Config> =
        read_json(&config_dir().join("presets.json")).unwrap_or_default();
    for cfg in p.values_mut() {
        cfg.sanitize();
    }
    p
}

pub fn save_presets(p: &BTreeMap<String, Config>) {
    write_json(&config_dir().join("presets.json"), p);
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: u64,
    pub out: String,
    pub caption: String,
    pub success: bool,
    pub wall_ms: f64,
}

pub fn load_history() -> Vec<HistoryEntry> {
    read_json(&config_dir().join("history.json")).unwrap_or_default()
}

pub fn append_history(e: HistoryEntry) {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut h = load_history();
    h.push(e);
    let keep = h.len().saturating_sub(100);
    h.drain(0..keep);
    write_json(&config_dir().join("history.json"), &h);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Paths;

    fn paths() -> Paths {
        Paths {
            bin: PathBuf::from("/nonexistent/audiocpp_cli"),
            model_dir: PathBuf::from("/nonexistent/model"),
            backend: "cuda".into(),
            language_model: Some("language_model_q4_k.gguf".into()),
            flow_transformer: Some("transformer_q4_k.gguf".into()),
            depth_decoder: Some("rvq_depth_decoder_q8_0.gguf".into()),
        }
    }

    #[test]
    fn sanitize_clamps_out_of_range() {
        let mut c = Config {
            genre: 999,
            mood: 999,
            vocals: 999,
            duration_sec: -5.0,
            num_inference_steps: 0,
            top_k: 0,
            ensemble_takes: 99,
            graph_context_mb: 1,
            backend: "bogus".into(),
            ..Config::default()
        };
        c.sanitize();
        assert!(c.genre < GENRES.len());
        assert!(c.mood < MOODS.len());
        assert!(c.vocals < VOCALS.len());
        assert_eq!(c.duration_sec, 1.0);
        assert_eq!(c.num_inference_steps, 1);
        assert_eq!(c.top_k, 1);
        assert_eq!(c.ensemble_takes, 16);
        assert_eq!(c.graph_context_mb, 8);
        assert!(c.backend.is_empty());
        // Must not panic:
        let _ = c.caption();
        let _ = c.warnings(&paths());
    }

    #[test]
    fn set_text_clamps_numeric_values() {
        let mut c = Config::default();
        set_text(&mut c, Field::Steps, "100000");
        assert_eq!(c.num_inference_steps, 500);
        set_text(&mut c, Field::Guidance, "-3");
        assert_eq!(c.guidance_scale, 0.0);
        set_text(&mut c, Field::Duration, "not-a-number");
        assert_eq!(c.duration_sec, 30.0); // unchanged
    }

    #[test]
    fn backend_cycle_round_trips_through_auto() {
        let p = paths();
        let mut c = Config::default();
        assert_eq!(select_index(&c, &p, Field::Backend), 0);
        cycle(&mut c, &p, Field::Backend, 1);
        assert_eq!(c.backend, crate::detect::BACKENDS[0]);
        assert_eq!(select_index(&c, &p, Field::Backend), 1);
        // cycling all the way around returns to auto
        for _ in 0..crate::detect::BACKENDS.len() {
            cycle(&mut c, &p, Field::Backend, 1);
        }
        assert!(c.backend.is_empty());
    }

    #[test]
    fn argv_has_core_flags_and_components() {
        let p = paths();
        let c = Config::default();
        let argv = build_argv(&p, &c);
        assert!(argv.contains(&"--task".to_string()));
        assert!(argv.contains(&"minimax_music3".to_string()));
        assert!(argv.iter().any(|a| a.contains("language_model_gguf")));
        assert!(argv.iter().any(|a| a == "--metrics"));
        let joined = argv.join(" ");
        assert!(joined.contains("--request-option duration_sec=30"));
    }
}
