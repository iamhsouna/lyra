use crate::core::{self, Config, Field};
use crate::detect::{Paths, expand_tilde, home};
use crate::runner::Job;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const MAX_BODY: u64 = 1 << 20; // 1 MiB

struct State {
    paths: Paths,
    cfg: Config,
    job: Option<Arc<Job>>,
}

type Shared = Arc<Mutex<State>>;

pub fn serve(paths: Paths, host: &str, port: u16) -> anyhow::Result<()> {
    let state: Shared = Arc::new(Mutex::new(State {
        paths,
        cfg: Config::default(),
        job: None,
    }));
    let addr = format!("{host}:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr} failed: {e}"))?;
    println!("✦ Lyra web UI → http://{addr}");
    println!("  (Ctrl-C to stop)");

    for request in server.incoming_requests() {
        let state = state.clone();
        std::thread::spawn(move || {
            if let Err(e) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(request, state)))
            {
                eprintln!("lyra: request handler panicked: {e:?}");
            }
        });
    }
    Ok(())
}

fn handle(mut req: Request, state: Shared) {
    let url = req.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url, String::new()),
    };
    let method = req.method().clone();

    if method == Method::Options {
        let _ = req.respond(empty(StatusCode(204)));
        return;
    }

    match (method, path.as_str()) {
        (Method::Get, "/") | (Method::Get, "/index.html") => {
            let _ = req.respond(html_response());
        }
        (Method::Get, "/api/health") => {
            let _ = req.respond(json_response(json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
            })));
        }
        (Method::Get, "/api/state") => {
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Get, "/api/log") => {
            let since = query_param(&query, "since")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            let value = match &st.job {
                Some(j) => {
                    let (next, lines) = j.lines_since(since);
                    let status = j.status();
                    json!({
                        "job_id": j.id,
                        "next": next,
                        "lines": lines,
                        "running": status.is_running(),
                        "status": status.label(),
                        "success": status.is_success(),
                        "out": j.out_path(),
                        "wall_ms": j.wall_ms(),
                        "exists": j.output_exists(),
                    })
                }
                None => json!({
                    "job_id": Value::Null,
                    "next": 0,
                    "lines": [],
                    "running": false,
                    "status": "idle",
                    "success": false,
                    "out": "",
                    "wall_ms": 0.0,
                    "exists": false,
                }),
            };
            let _ = req.respond(json_response(value));
        }
        (Method::Post, "/api/field") => {
            let body = read_body(&mut req);
            let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(f) = Field::from_id(id) {
                let State { paths, cfg, .. } = &mut *st;
                match action {
                    "cycle-" => core::adjust(cfg, paths, f, -1),
                    "cycle+" => core::adjust(cfg, paths, f, 1),
                    "toggle" => core::toggle(cfg, f),
                    "reroll" => {
                        if f == Field::Seed {
                            cfg.seed = core::random_seed();
                        }
                    }
                    "set" => {
                        let v = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
                        core::set_text(cfg, f, v);
                    }
                    _ => {}
                }
                cfg.sanitize();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/select") => {
            let body = read_body(&mut req);
            let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let idx = body
                .get("index")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0) as usize;
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(f) = Field::from_id(id) {
                let State { paths, cfg, .. } = &mut *st;
                set_select(cfg, paths, f, idx);
                cfg.sanitize();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/generate") => {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            if st.job.as_ref().map(|j| j.running()).unwrap_or(false) {
                let _ = req.respond(
                    json_response(json!({"error": "a job is already running"}))
                        .with_status_code(409),
                );
                return;
            }
            st.cfg.sanitize();
            let errors = st.cfg.errors(&st.paths);
            if !errors.is_empty() {
                let _ = req.respond(
                    json_response(json!({"error": "cannot start", "errors": errors}))
                        .with_status_code(400),
                );
                return;
            }
            match Job::start(&st.paths, &st.cfg) {
                Ok(j) => {
                    st.job = Some(Arc::new(j));
                    let _ = req.respond(json_response(state_value(&st)));
                }
                Err(e) => {
                    let _ = req.respond(
                        json_response(json!({"error": format!("launch failed: {e}")}))
                            .with_status_code(500),
                    );
                }
            }
        }
        (Method::Post, "/api/abort") => {
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(j) = &st.job {
                j.abort();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/reset") => {
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            let out = st.cfg.out.clone();
            st.cfg = Config {
                out,
                ..Config::default()
            };
            st.cfg.sanitize();
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/save") => {
            let body = read_body(&mut req);
            let name = body
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            if !name.is_empty() && name.len() <= 120 {
                let mut p = core::load_presets();
                p.insert(name, st.cfg.clone());
                core::save_presets(&p);
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/load") => {
            let body = read_body(&mut req);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
            let p = core::load_presets();
            if let Some(c) = p.get(name) {
                st.cfg = c.clone();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/delete") => {
            let body = read_body(&mut req);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            let mut p = core::load_presets();
            p.remove(name);
            core::save_presets(&p);
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Get, "/api/download") => {
            let raw = query_param(&query, "path").unwrap_or_default();
            match open_wav(&percent_decode(&raw)) {
                Ok(file) => {
                    let name = file
                        .1
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "lyra.wav".into());
                    let resp = Response::from_file(file.0)
                        .with_header(header("Content-Type", "audio/wav"))
                        .with_header(header(
                            "Content-Disposition",
                            &format!("attachment; filename=\"{name}\""),
                        ));
                    let _ = req.respond(resp);
                }
                Err(code) => {
                    let msg = if code == 403 {
                        "forbidden"
                    } else {
                        "not found"
                    };
                    let _ = req.respond(
                        json_response(json!({"error": msg})).with_status_code(StatusCode(code)),
                    );
                }
            }
        }
        _ => {
            let _ = req.respond(json_response(json!({"error": "not found"})).with_status_code(404));
        }
    }
}

/// Resolve a requested `.wav` path, rejecting anything outside `$HOME` and any
/// path that does not canonically resolve to a regular file.
fn open_wav(raw: &str) -> Result<(File, std::path::PathBuf), u16> {
    if raw.trim().is_empty() {
        return Err(404);
    }
    let path = expand_tilde(raw);
    let real = path.canonicalize().map_err(|_| 404u16)?;
    let home_real = home().canonicalize().map_err(|_| 404u16)?;
    if !real.starts_with(&home_real) {
        return Err(403);
    }
    if real.extension().and_then(|e| e.to_str()) != Some("wav") {
        return Err(403);
    }
    let file = File::open(&real).map_err(|_| 404u16)?;
    Ok((file, real))
}

fn set_select(c: &mut Config, paths: &Paths, f: Field, idx: usize) {
    let opts = core::options(paths, f);
    let n = opts.len();
    match f {
        Field::Genre => c.genre = idx.min(n.saturating_sub(1)),
        Field::Mood => c.mood = idx.min(core::MOODS.len().saturating_sub(1)),
        Field::Vocals => c.vocals = idx.min(core::VOCALS.len().saturating_sub(1)),
        Field::Backend | Field::Lm | Field::Flow | Field::Depth => {
            let v = if idx == 0 {
                String::new()
            } else {
                opts.get(idx - 1).cloned().unwrap_or_default()
            };
            match f {
                Field::Backend => c.backend = v,
                Field::Lm => c.lm_gguf = v,
                Field::Flow => c.flow_gguf = v,
                Field::Depth => c.depth_gguf = v,
                _ => unreachable!(),
            }
        }
        _ => {}
    }
}

fn state_value(st: &State) -> Value {
    let fields: Vec<Value> = core::ALL_FIELDS
        .iter()
        .map(|f| {
            let opts = core::options(&st.paths, *f);
            let is_select = f.kind() == core::Kind::Select;
            json!({
                "id": f.id(),
                "label": f.label(),
                "group": f.group().as_str(),
                "kind": f.kind().as_str(),
                "help": f.help(),
                "value": core::value_string(&st.cfg, *f),
                "index": if is_select { core::select_index(&st.cfg, &st.paths, *f) } else { 0 },
                "options": opts,
                "auto": matches!(f, Field::Backend | Field::Lm | Field::Flow | Field::Depth),
            })
        })
        .collect();

    let presets: Vec<String> = core::load_presets().keys().cloned().collect();
    let history: Vec<Value> = core::load_history()
        .iter()
        .rev()
        .take(25)
        .map(|h| {
            json!({
                "at": h.at, "out": h.out, "caption": h.caption,
                "success": h.success, "wall_ms": h.wall_ms,
            })
        })
        .collect();

    let (running, status, elapsed, job_id, output_exists) = match &st.job {
        Some(j) => (
            j.running(),
            j.status().label(),
            j.elapsed_secs(),
            Some(j.id),
            j.output_exists(),
        ),
        None => (false, "idle".to_string(), 0.0, None, false),
    };

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "fields": fields,
        "caption": st.cfg.caption(),
        "lyrics": st.cfg.lyrics_text(),
        "out_path": st.cfg.out_path(),
        "warnings": st.cfg.warnings(&st.paths),
        "errors": st.cfg.errors(&st.paths),
        "command": core::shell_join(&core::build_argv(&st.paths, &st.cfg)),
        "paths": {
            "bin": st.paths.bin.to_string_lossy(),
            "model": st.paths.model_dir.to_string_lossy(),
            "backend": st.paths.backend,
        },
        "config_dir": core::config_dir().to_string_lossy(),
        "presets": presets,
        "history": history,
        "running": running,
        "status": status,
        "elapsed": elapsed,
        "job_id": job_id,
        "output_exists": output_exists,
    })
}

// --- http helpers -----------------------------------------------------------

fn read_body(req: &mut Request) -> Value {
    let mut s = String::new();
    let _ = req.as_reader().take(MAX_BODY).read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn base_headers() -> Vec<Header> {
    vec![
        header("X-Content-Type-Options", "nosniff"),
        header("Referrer-Policy", "no-referrer"),
        header("Cache-Control", "no-store"),
    ]
}

fn json_response(v: Value) -> Response<Cursor<Vec<u8>>> {
    let mut r = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_header(header("Content-Type", "application/json"));
    for h in base_headers() {
        r = r.with_header(h);
    }
    r
}

fn empty(status: StatusCode) -> Response<Cursor<Vec<u8>>> {
    let mut r = Response::from_data(Vec::<u8>::new()).with_status_code(status);
    for h in base_headers() {
        r = r.with_header(h);
    }
    r
}

fn html_response() -> Response<Cursor<Vec<u8>>> {
    let mut r = Response::from_data(INDEX_HTML.as_bytes().to_vec())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
        .with_header(header(
            "Content-Security-Policy",
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
             connect-src 'self'; img-src 'self' data:; media-src 'self'",
        ));
    for h in base_headers() {
        r = r.with_header(h);
    }
    r
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<meta name="color-scheme" content="dark"/>
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Ctext y='13' font-size='13'%3E%E2%9C%A6%3C/text%3E%3C/svg%3E"/>
<title>✦ Lyra — local song generation</title>
<style>
:root{--bg:#0d1117;--panel:#161b22;--panel2:#1c2430;--line:#30363d;--txt:#e6edf3;--dim:#8b949e;
--accent:#39d0d8;--mag:#d2a8ff;--ok:#3fb950;--err:#f85149;--warn:#d29922}
*{box-sizing:border-box}
html,body{height:100%}
body{margin:0;background:var(--bg);color:var(--txt);font:14px/1.5 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;display:flex;flex-direction:column}
header{padding:10px 16px;border-bottom:1px solid var(--line);display:flex;gap:12px;align-items:center;flex-wrap:wrap;position:sticky;top:0;background:var(--bg);z-index:5}
h1{font-size:16px;margin:0;color:var(--mag);white-space:nowrap}
.tabs{display:flex;gap:6px;flex-wrap:wrap}
.tab{padding:4px 10px;border:1px solid var(--line);border-radius:6px;color:var(--dim);cursor:pointer;user-select:none}
.tab:hover{border-color:var(--accent);color:var(--txt)}
.tab.on{background:var(--accent);color:#00151a;border-color:var(--accent);font-weight:700}
.badge{margin-left:auto;padding:3px 10px;border-radius:999px;border:1px solid var(--line);color:var(--dim);font-size:12px;display:flex;align-items:center;gap:6px;white-space:nowrap}
.badge.run{color:var(--accent);border-color:var(--accent)}
.badge.ok{color:var(--ok);border-color:var(--ok)}
.badge.err{color:var(--err);border-color:var(--err)}
main{padding:16px;max-width:1180px;margin:0 auto;width:100%;flex:1 1 auto}
.grid{display:grid;grid-template-columns:1fr 1fr;gap:16px}
@media(max-width:860px){.grid{grid-template-columns:1fr}}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:12px}
.panel h2{font-size:12px;margin:0 0 10px;color:var(--accent);text-transform:uppercase;letter-spacing:.08em}
.grp{margin:16px 0 6px;color:var(--mag);font-weight:700;font-size:12px;text-transform:uppercase;letter-spacing:.06em}
.grp:first-child{margin-top:0}
.row{display:grid;grid-template-columns:180px 1fr;gap:10px;align-items:center;padding:3px 0}
@media(max-width:560px){.row{grid-template-columns:1fr}}
.row>label{color:var(--dim);font-size:13px;overflow-wrap:anywhere}
.row .hint{grid-column:2;color:var(--dim);font-size:12px;line-height:1.3}
@media(max-width:560px){.row .hint{grid-column:1}}
input,select,textarea{background:#0b0f14;color:var(--txt);border:1px solid var(--line);border-radius:5px;padding:6px 8px;font:inherit;width:100%}
input:focus,select:focus,textarea:focus{outline:none;border-color:var(--accent)}
textarea{min-height:130px;resize:vertical}
.num{display:flex;gap:6px;align-items:center}
.num button{flex:0 0 auto}
button{background:#21262d;color:var(--txt);border:1px solid var(--line);border-radius:6px;padding:6px 12px;cursor:pointer;font:inherit}
button:hover{filter:brightness(1.15)}
button:disabled{opacity:.45;cursor:not-allowed;filter:none}
button.pri{background:var(--accent);color:#00151a;border-color:var(--accent);font-weight:700}
button.danger{background:#3d1418;border-color:var(--err);color:#ffb3ae}
button.mini{padding:2px 9px;line-height:1.4}
pre{background:#0b0f14;border:1px solid var(--line);border-radius:6px;padding:10px;white-space:pre-wrap;word-break:break-word;max-height:360px;overflow:auto;margin:0}
#log{max-height:460px}
.warn{color:var(--warn)}.err{color:var(--err)}.ok{color:var(--ok)}.dim{color:var(--dim)}
.bar{position:sticky;bottom:0;background:var(--panel);border-top:1px solid var(--line);padding:10px 16px;display:flex;gap:10px;align-items:center;flex-wrap:wrap;z-index:5}
.chip{display:inline-flex;align-items:center;gap:6px;border:1px solid var(--line);border-radius:999px;padding:2px 10px;margin:2px;color:var(--dim)}
.chip button{border:none;background:none;color:var(--err);padding:0 2px;font-size:13px}
a{color:var(--accent)}
.toast{position:fixed;right:16px;bottom:70px;background:var(--panel2);border:1px solid var(--line);border-radius:8px;padding:10px 14px;max-width:360px;box-shadow:0 6px 24px #0008;z-index:20;opacity:0;transform:translateY(8px);transition:.18s}
.toast.show{opacity:1;transform:none}
.spin{display:inline-block;animation:r 1s linear infinite}
@keyframes r{to{transform:rotate(360deg)}}
details summary{cursor:pointer}
.mono{font-size:12px}
.foot{color:var(--dim);font-size:12px;margin-top:14px}
</style>
</head>
<body>
<header>
  <h1>✦ Lyra</h1>
  <div class="tabs" id="tabs" role="tablist"></div>
  <span class="badge" id="status">loading…</span>
</header>
<main id="main"></main>
<div class="bar">
  <button class="pri" id="genbtn" data-act="generate">▶ Generate</button>
  <button class="danger" id="abortbtn" data-act="abort">■ Abort</button>
  <button data-act="reroll" title="randomize the seed">🎲 Seed</button>
  <button data-act="reset" title="restore defaults">↺ Reset</button>
  <span class="dim" id="barinfo"></span>
</div>
<div class="toast" id="toast"></div>
<script>
const TABS=[["song","Song"],["advanced","Advanced"],["components","Components"],["run","Run"],["presets","Presets"],["help","Help"]];
let S=null,tab="song",logLines=[],autoScroll=true,busy=false,logCursor=0,logJobId=null,logTimer=null,sawRunning=false;
const $=s=>document.querySelector(s);
const esc=s=>String(s==null?"":s).replace(/[&<>"']/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[c]));
const getJSON=async u=>{const r=await fetch(u,{cache:"no-store"});if(!r.ok)throw new Error("HTTP "+r.status);return r.json();};
const post=async(u,b)=>{const r=await fetch(u,{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify(b||{})});const d=await r.json().catch(()=>({}));if(!r.ok)throw new Error(d.error||("HTTP "+r.status));return d;};
const fmtSec=s=>{s=Math.max(0,Math.round(s));const m=Math.floor(s/60);return m?`${m}m${String(s%60).padStart(2,"0")}s`:`${s}s`;};

function toast(msg,kind){const t=$("#toast");t.textContent=msg;t.className="toast show "+(kind||"");clearTimeout(t._h);t._h=setTimeout(()=>t.className="toast "+(kind||""),3200);}

function render(){renderHeader();renderTabs();renderMain();}

function renderHeader(){
  const el=$("#status");
  if(!S){el.textContent="connecting…";el.className="badge";return;}
  if(S.running){el.className="badge run";el.innerHTML=`<span class="spin">◐</span> generating · ${fmtSec(S.elapsed)}`;}
  else if(S.errors&&S.errors.length){el.className="badge err";el.textContent="setup incomplete";}
  else{el.className="badge";el.textContent=S.status||"idle";}
  $("#genbtn").disabled=!!S.running;
  $("#abortbtn").disabled=!S.running;
  $("#barinfo").textContent=S.running?"streaming audiocpp_cli output…":(S.out_path||"");
}

function renderTabs(){
  $("#tabs").innerHTML=TABS.map(([k,l])=>`<div class="tab ${k===tab?"on":""}" role="tab" data-tab="${k}">${l}</div>`).join("");
}

function fieldsFor(g){return S?S.fields.filter(f=>f.group===g):[];}

function renderMain(){
  if(!S){$("#main").innerHTML=`<div class="panel"><h2>connect</h2><p class="dim">Talking to the local Lyra server…</p></div>`;return;}
  if(tab==="run"){$("#main").innerHTML=runView();scrollLog();return;}
  if(tab==="presets"){$("#main").innerHTML=presetView();return;}
  if(tab==="help"){$("#main").innerHTML=helpView();return;}
  $("#main").innerHTML=`<div class="grid"><div class="panel"><h2>${esc(tab)}</h2>${formView()}</div><div class="panel"><h2>preview</h2>${previewView()}</div></div>`;
}

function fieldControl(f){
  if(f.kind==="select"){
    const auto=f.auto?`<option value="0" ${f.index===0?"selected":""}>(auto — ${esc(S.paths.backend||"detected")})</option>`:"";
    const opts=f.options.map((o,i)=>{const v=f.auto?i+1:i;return `<option value="${v}" ${f.index===v?"selected":""}>${esc(o)}</option>`;}).join("");
    return `<select data-field="${f.id}" data-kind="select">${auto}${opts}</select>`;
  }
  if(f.kind==="bool"){
    const on=f.value==="true";
    return `<label class="dim" style="display:flex;gap:8px;align-items:center;cursor:pointer"><input type="checkbox" data-field="${f.id}" data-kind="bool" style="width:auto" ${on?"checked":""}/> ${on?"on":"off"}</label>`;
  }
  if(f.kind==="multiline"){
    return `<textarea data-field="${f.id}" data-kind="text">${esc(f.value)}</textarea>`;
  }
  if(f.kind==="text"){
    return `<input type="text" data-field="${f.id}" data-kind="text" value="${esc(f.value)}"/>`;
  }
  const reroll=f.id==="seed"?`<button class="mini" data-act="reroll" title="randomize">🎲</button>`:"";
  return `<div class="num"><button class="mini" data-act="cycle" data-field="${f.id}" data-dir="-1">−</button>
    <input type="text" inputmode="decimal" data-field="${f.id}" data-kind="text" value="${esc(f.value)}"/>
    <button class="mini" data-act="cycle" data-field="${f.id}" data-dir="1">+</button>${reroll}</div>`;
}

function formView(){
  let out="",last="";
  for(const f of fieldsFor(tab)){
    if(f.group!==last){last=f.group;out+=`<div class="grp">${esc(f.group)}</div>`;}
    out+=`<div class="row"><label for="f-${f.id}">${esc(f.label)}</label><div>${fieldControl(f)}</div><div class="hint">${esc(f.help)}</div></div>`;
  }
  return out||`<p class="dim">no fields</p>`;
}

function previewView(){
  const errs=(S.errors||[]).map(e=>`<div class="err">✗ ${esc(e)}</div>`).join("");
  const warns=(S.warnings||[]).filter(w=>!(S.errors||[]).includes(w)).map(w=>`<div class="warn">⚠ ${esc(w)}</div>`).join("");
  return `<b class="dim">caption</b><p>${esc(S.caption)}</p>
  <b class="dim">lyrics</b><pre>${esc(S.lyrics)}</pre>
  <b class="dim">output</b><p>${esc(S.out_path)}</p>
  ${errs}${warns}${!errs&&!warns?`<p class="ok">✓ setup looks good</p>`:""}
  <details><summary class="dim">resolved command</summary><pre>${esc(S.command)}</pre>
  <button class="mini" data-act="copy" data-copy="${esc(S.command)}">copy</button></details>`;
}

function runView(){
  const hist=historyView();
  const dl=S.output_exists?`<p><a href="/api/download?path=${encodeURIComponent(S.out_path)}">⬇ download ${esc(S.out_path.split("/").pop())}</a></p>`:"";
  const st=S.running?`<span class="spin">◐</span> ${esc(S.status)} · ${fmtSec(S.elapsed)}`:`<span class="${S.status==="success"?"ok":(S.status==="idle"?"dim":"err")}">${esc(S.status)}</span>`;
  return `<div class="grid"><div class="panel"><h2>live log</h2>
    <label class="dim" style="display:flex;gap:6px;align-items:center;margin-bottom:6px"><input type="checkbox" id="autoscroll" ${autoScroll?"checked":""} style="width:auto"/> follow output</label>
    <pre id="log"></pre></div>
    <div class="panel"><h2>status</h2><p id="runstat">${st}</p>${dl}${errorsHtml()}
    <button data-act="reroll">🎲 reroll seed</button></div></div>${hist}`;
}

function errorsHtml(){
  const e=(S.errors||[]).map(x=>`<div class="err">✗ ${esc(x)}</div>`).join("");
  const w=(S.warnings||[]).filter(x=>!(S.errors||[]).includes(x)).map(x=>`<div class="warn">⚠ ${esc(x)}</div>`).join("");
  return e+w;
}

function historyView(){
  const rows=(S.history||[]).map(h=>{
    const t=new Date(h.at*1000).toLocaleString();
    const tag=h.success?`<span class="ok">✓</span>`:`<span class="err">✗</span>`;
    const link=h.success&&h.out.endsWith(".wav")?` <a href="/api/download?path=${encodeURIComponent(h.out)}" class="dim">download</a>`:"";
    return `<div class="dim mono">${tag} ${esc(t)} · ${h.wall_ms?fmtSec(h.wall_ms/1000):""} · ${esc(h.out)}${link}</div>`;
  }).join("");
  return `<div class="panel" style="margin-top:14px"><h2>recent runs</h2>${rows||`<span class="dim">no runs yet</span>`}</div>`;
}

function presetView(){
  const chips=(S.presets||[]).length?S.presets.map(n=>`<span class="chip">${esc(n)} <button data-act="del-preset" data-name="${esc(n)}" title="delete">✕</button></span>`).join(""):`<span class="dim">none yet</span>`;
  return `<div class="grid"><div class="panel"><h2>presets</h2>
    <div class="row"><label>save current as</label><div class="num"><input id="pname" type="text" placeholder="my pop song"/><button class="pri" data-act="save-preset">Save</button></div></div>
    <div style="margin-top:12px">${chips}</div>
    <p class="dim mono">stored in ${esc(S.config_dir)}/presets.json</p></div>
    <div class="panel"><h2>history</h2>${historyViewInner()}</div></div>`;
}

function historyViewInner(){
  const rows=(S.history||[]).map(h=>`<div class="dim mono">${h.success?`<span class="ok">✓</span>`:`<span class="err">✗</span>`} ${new Date(h.at*1000).toLocaleString()} · ${esc(h.out)}</div>`).join("");
  return rows||`<span class="dim">no runs yet</span>`;
}

function helpView(){
  return `<div class="panel"><h2>how Lyra works</h2>
  <p><b>Song</b> sets the prompt: style, mood, vocals, optional caption override, lyrics, length, steps and output path.</p>
  <p><b>Advanced</b> exposes the model controls: flow/AR guidance, top-k, seed, the flow/perf toggles and memory options.</p>
  <p><b>Components</b> picks the backend and which LM / flow / depth GGUF to load (options come from your model dir).</p>
  <p><b>Run</b> streams the audiocpp_cli log live; <b>Generate</b> starts and <b>Abort</b> stops it. Finished files can be downloaded.</p>
  <p><b>Presets</b> save/load full configs to <code>${esc(S.config_dir)}</code>.</p>
  <p class="dim">Keyboard: <code>1</code>-<code>6</code> switch tabs · <code>Ctrl+Enter</code> generate · <code>Esc</code> abort.</p>
  <div class="foot">Lyra ${esc(S.version)} · same core as the TUI (<code>lyra</code>)</div></div>`;
}

function appendLog(line){
  logLines.push(line);
  if(logLines.length>6000)logLines.splice(0,2000);
  const p=$("#log");
  if(p){p.textContent=logLines.join("\n");if(autoScroll)p.scrollTop=p.scrollHeight;}
}
function scrollLog(){const p=$("#log");if(p){p.textContent=logLines.join("\n");p.scrollTop=p.scrollHeight;}}

function setState(s){S=s;render();}

// --- interactions (event delegation) ---
document.addEventListener("click",async e=>{
  const tabEl=e.target.closest("[data-tab]");
  if(tabEl){tab=tabEl.dataset.tab;render();return;}
  const btn=e.target.closest("[data-act]");
  if(!btn)return;
  const act=btn.dataset.act;
  try{
    if(act==="cycle"){setState(await post("/api/field",{id:btn.dataset.field,action:btn.dataset.dir==="-1"?"cycle-":"cycle+"}));}
    else if(act==="reroll"){setState(await post("/api/field",{id:"seed",action:"reroll"}));toast("new seed");}
    else if(act==="reset"){if(confirm("Reset all settings to defaults?")){setState(await post("/api/reset"));toast("reset to defaults");}}
    else if(act==="generate"){await generate();}
    else if(act==="abort"){setState(await post("/api/abort"));}
    else if(act==="save-preset"){const n=($("#pname")||{}).value?.trim();if(!n)return toast("enter a preset name","err");setState(await post("/api/presets/save",{name:n}));toast("preset '"+n+"' saved");}
    else if(act==="del-preset"){setState(await post("/api/presets/delete",{name:btn.dataset.name}));toast("preset deleted");}
    else if(act==="copy"){await navigator.clipboard.writeText(btn.dataset.copy||S.command);toast("command copied");}
  }catch(err){toast(String(err.message||err),"err");}
});

document.addEventListener("change",async e=>{
  const el=e.target.closest("[data-field]");
  if(!el)return;
  const id=el.dataset.field,kind=el.dataset.kind;
  try{
    if(kind==="select"){setState(await post("/api/select",{id,index:Number(el.value)}));}
    else if(kind==="bool"){setState(await post("/api/field",{id,action:"toggle"}));}
    else{setState(await post("/api/field",{id,action:"set",value:el.value}));}
  }catch(err){toast(String(err.message||err),"err");}
});

document.addEventListener("change",e=>{
  if(e.target&&e.target.id==="autoscroll"){autoScroll=e.target.checked;if(autoScroll)scrollLog();}
});

document.addEventListener("keydown",async e=>{
  if(e.target&&/INPUT|TEXTAREA|SELECT/.test(e.target.tagName))return;
  if(e.key==="Enter"&&(e.ctrlKey||e.metaKey)){e.preventDefault();await generate();return;}
  if(e.key==="Escape"&&S&&S.running){try{setState(await post("/api/abort"));}catch(_){}return;}
  const n=parseInt(e.key,10);
  if(n>=1&&n<=TABS.length){tab=TABS[n-1][0];render();}
});

async function generate(){
  if(busy)return;busy=true;
  try{
    await post("/api/generate");
    logLines=[];logCursor=0;logJobId=null;autoScroll=true;tab="run";
    setState(await getJSON("/api/state"));
    toast("generation started");
    startLogPoll();
  }catch(err){toast(String(err.message||err),"err");}
  finally{busy=false;}
}

// --- live log (incremental polling; robust over tiny_http) ---
function startLogPoll(){
  clearTimeout(logTimer);
  logTimer=setTimeout(pollLog,250);
}

async function pollLog(){
  logTimer=null;
  try{
    let d=await getJSON("/api/log?since="+logCursor);
    if(d.job_id!==logJobId){
      logJobId=d.job_id;logCursor=0;logLines=[];
      d=await getJSON("/api/log?since=0");
    }
    for(const l of (d.lines||[]))appendLog(l);
    logCursor=d.next||0;
    S=await getJSON("/api/state");
    renderHeader();
    if(tab==="run"){
      const p=$("#runstat");
      if(p)p.innerHTML=S.running?`<span class="spin">◐</span> ${esc(S.status)} · ${fmtSec(S.elapsed)}`:`<span class="${S.status==="success"?"ok":"err"}">${esc(S.status)}</span>`;
    }
    if(d.running){sawRunning=true;logTimer=setTimeout(pollLog,450);}
    else{
      if(sawRunning){
        appendLog("— "+(d.status||"done")+" · "+fmtSec((d.wall_ms||0)/1000));
        toast(d.success?"song saved 🎵":"job "+(d.status||"finished"),d.success?"ok":"err");
      }
      sawRunning=false;
      setState(S);
    }
  }catch(err){logTimer=setTimeout(pollLog,1500);}
}

document.addEventListener("DOMContentLoaded",async()=>{
  try{
    const s=await getJSON("/api/state");setState(s);
    if(s.job_id!=null)startLogPoll();
  }catch(err){toast("cannot reach Lyra server","err");}
  setInterval(async()=>{if(S&&S.running&&!logTimer){try{S=await getJSON("/api/state");renderHeader();}catch(_){}}},1500);
});
</script>
</body>
</html>
"##;
