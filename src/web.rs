use crate::core::{self, Config, Field};
use crate::detect::{expand_tilde, home, Paths};
use crate::runner::Job;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

struct State {
    paths: Paths,
    cfg: Config,
    job: Option<Arc<Job>>,
}

type Shared = Arc<Mutex<State>>;

pub fn serve(paths: Paths, host: &str, port: u16) -> anyhow::Result<()> {
    let cfg = Config::default();
    let state: Shared = Arc::new(Mutex::new(State { paths, cfg, job: None }));
    let addr = format!("{host}:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr} failed: {e}"))?;
    println!("✦ Lyra web UI → http://{addr}");
    println!("  (Ctrl-C to stop)");

    for request in server.incoming_requests() {
        let state = state.clone();
        std::thread::spawn(move || handle(request, state));
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
        let _ = req.respond(cors(Response::new_empty(StatusCode(204))));
        return;
    }

    match (method, path.as_str()) {
        (Method::Get, "/") | (Method::Get, "/index.html") => {
            let _ = req.respond(html_response());
        }
        (Method::Get, "/api/state") => {
            let st = state.lock().unwrap();
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Get, "/api/events") => {
            let job = state.lock().unwrap().job.clone();
            let Some(job) = job else {
                let _ = req.respond(json_response(json!({"error": "no job"})));
                return;
            };
            let headers = vec![
                header("Content-Type", "text/event-stream"),
                header("Cache-Control", "no-cache"),
                header("Access-Control-Allow-Origin", "*"),
            ];
            let resp = Response::new(StatusCode(200), headers, SseStream::new(job), None, None);
            let _ = req.respond(resp);
        }
        (Method::Post, "/api/field") => {
            let body = read_body(&mut req);
            let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let action = body.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let mut st = state.lock().unwrap();
            if let Some(f) = Field::from_id(id) {
                let State { paths, cfg, .. } = &mut *st;
                match action {
                    "cycle-" => core::adjust(cfg, paths, f, -1),
                    "cycle+" => core::adjust(cfg, paths, f, 1),
                    "toggle" => core::toggle(cfg, f),
                    "set" => {
                        let v = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
                        core::set_text(cfg, f, v);
                    }
                    _ => {}
                }
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/select") => {
            let body = read_body(&mut req);
            let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let idx = body.get("index").and_then(|v| v.as_i64()).unwrap_or(0) as usize;
            let mut st = state.lock().unwrap();
            if let Some(f) = Field::from_id(id) {
                let State { paths, cfg, .. } = &mut *st;
                set_select(cfg, paths, f, idx);
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/generate") => {
            let mut st = state.lock().unwrap();
            if st.job.as_ref().map(|j| j.running()).unwrap_or(false) {
                let _ = req.respond(
                    json_response(json!({"error": "already running"})).with_status_code(409),
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
            let st = state.lock().unwrap();
            if let Some(j) = &st.job {
                j.abort();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/save") => {
            let body = read_body(&mut req);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
            let st = state.lock().unwrap();
            if !name.is_empty() {
                let mut p = core::load_presets();
                p.insert(name, st.cfg.clone());
                core::save_presets(&p);
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/load") => {
            let body = read_body(&mut req);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let mut st = state.lock().unwrap();
            let p = core::load_presets();
            if let Some(c) = p.get(name) {
                st.cfg = c.clone();
            }
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Post, "/api/presets/delete") => {
            let body = read_body(&mut req);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let st = state.lock().unwrap();
            let mut p = core::load_presets();
            p.remove(name);
            core::save_presets(&p);
            let _ = req.respond(json_response(state_value(&st)));
        }
        (Method::Get, "/api/download") => {
            let raw = query_param(&query, "path").unwrap_or_default();
            let path = expand_tilde(&percent_decode(&raw));
            let home = home();
            if !path.starts_with(&home) || !path.to_string_lossy().ends_with(".wav") {
                let _ = req.respond(json_response(json!({"error": "forbidden"})).with_status_code(403));
                return;
            }
            match File::open(&path) {
                Ok(file) => {
                    let _ = req.respond(
                        Response::from_file(file).with_header(header("Content-Type", "audio/wav")),
                    );
                }
                Err(e) => {
                    let _ = req.respond(
                        json_response(json!({"error": e.to_string()})).with_status_code(404),
                    );
                }
            }
        }
        _ => {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

fn set_select(c: &mut Config, paths: &Paths, f: Field, idx: usize) {
    let opts = core::options(paths, f);
    let n = opts.len();
    match f {
        Field::Genre => c.genre = idx.min(n.saturating_sub(1)),
        Field::Mood => c.mood = idx.min(opts.len().saturating_sub(1)),
        Field::Vocals => c.vocals = idx.min(opts.len().saturating_sub(1)),
        Field::Backend => c.backend = if idx == 0 { String::new() } else { opts.get(idx - 1).cloned().unwrap_or_default() },
        Field::Lm => c.lm_gguf = if idx == 0 { String::new() } else { opts.get(idx - 1).cloned().unwrap_or_default() },
        Field::Flow => c.flow_gguf = if idx == 0 { String::new() } else { opts.get(idx - 1).cloned().unwrap_or_default() },
        Field::Depth => c.depth_gguf = if idx == 0 { String::new() } else { opts.get(idx - 1).cloned().unwrap_or_default() },
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
        .take(20)
        .map(|h| json!({"at": h.at, "out": h.out, "caption": h.caption, "success": h.success, "wall_ms": h.wall_ms}))
        .collect();

    json!({
        "fields": fields,
        "caption": st.cfg.caption(),
        "lyrics": st.cfg.lyrics_text(),
        "out_path": st.cfg.out_path(),
        "warnings": st.cfg.warnings(&st.paths),
        "command": core::shell_join(&core::build_argv(&st.paths, &st.cfg)),
        "paths": {
            "bin": st.paths.bin.to_string_lossy(),
            "model": st.paths.model_dir.to_string_lossy(),
            "backend": st.paths.backend,
        },
        "presets": presets,
        "history": history,
        "running": st.job.as_ref().map(|j| j.running()).unwrap_or(false),
        "status": st.job.as_ref().map(|j| j.status().label()).unwrap_or_else(|| "idle".into()),
    })
}

// --- SSE --------------------------------------------------------------------

struct SseStream {
    job: Arc<Job>,
    cursor: usize,
    pending: Vec<u8>,
    pos: usize,
    done_sent: bool,
}

impl SseStream {
    fn new(job: Arc<Job>) -> Self {
        SseStream { job, cursor: 0, pending: Vec::new(), pos: 0, done_sent: false }
    }
}

impl Read for SseStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.pos < self.pending.len() {
                let n = (self.pending.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
                self.pos += n;
                if self.pos == self.pending.len() {
                    self.pending.clear();
                    self.pos = 0;
                }
                return Ok(n);
            }
            let lines = self.job.lines();
            if self.cursor < lines.len() {
                let mut out = String::new();
                for l in &lines[self.cursor..] {
                    out.push_str("data: ");
                    out.push_str(&serde_json::to_string(l).unwrap_or_else(|_| "\"\"".into()));
                    out.push_str("\n\n");
                }
                self.cursor = lines.len();
                self.pending = out.into_bytes();
                continue;
            }
            let status = self.job.status();
            if !status.is_running() {
                if !self.done_sent {
                    self.done_sent = true;
                    let ev = format!(
                        "event: done\ndata: {}\n\n",
                        json!({
                            "status": status.label(),
                            "success": status == crate::runner::Status::Success,
                            "out": self.job.config.out_path(),
                            "wall_ms": self.job.wall_ms(),
                        })
                    );
                    self.pending = ev.into_bytes();
                    continue;
                }
                return Ok(0);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

// --- http helpers -----------------------------------------------------------

fn read_body(req: &mut Request) -> Value {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).unwrap()
}

fn json_response(v: Value) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
}

fn cors<R>(r: Response<R>) -> Response<R>
where
    R: Read + Send + 'static,
{
    r.with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "Content-Type"))
}

fn html_response() -> Response<Cursor<Vec<u8>>> {
    Response::from_data(INDEX_HTML.as_bytes().to_vec())
        .with_header(header("Content-Type", "text/html; charset=utf-8"))
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
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
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
<title>✦ Lyra — local song generation</title>
<style>
:root{--bg:#0d1117;--panel:#161b22;--line:#30363d;--txt:#e6edf3;--dim:#8b949e;--accent:#39d0d8;--mag:#d2a8ff;--ok:#3fb950;--err:#f85149}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--txt);font:14px/1.45 ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}
header{padding:10px 16px;border-bottom:1px solid var(--line);display:flex;gap:14px;align-items:center;flex-wrap:wrap}
h1{font-size:16px;margin:0;color:var(--mag)}
.tabs{display:flex;gap:6px;flex-wrap:wrap}
.tab{padding:4px 10px;border:1px solid var(--line);border-radius:6px;color:var(--dim);cursor:pointer}
.tab.on{background:var(--accent);color:#00151a;border-color:var(--accent);font-weight:700}
main{padding:16px;max-width:1100px;margin:0 auto}
.grid{display:grid;grid-template-columns:1fr 1fr;gap:16px}
@media(max-width:820px){.grid{grid-template-columns:1fr}}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:12px}
.panel h2{font-size:13px;margin:0 0 10px;color:var(--accent);text-transform:uppercase;letter-spacing:.05em}
.grp{margin:14px 0 4px;color:var(--mag);font-weight:700}
.row{display:grid;grid-template-columns:190px 1fr;gap:8px;align-items:center;padding:3px 0}
.row label{color:var(--dim)}
.row .hint{grid-column:2;color:var(--dim);font-size:12px}
input,select,textarea{background:#0b0f14;color:var(--txt);border:1px solid var(--line);border-radius:5px;padding:5px 7px;font:inherit;width:100%}
textarea{min-height:120px;resize:vertical}
button{background:#21262d;color:var(--txt);border:1px solid var(--line);border-radius:6px;padding:6px 12px;cursor:pointer;font:inherit}
button.pri{background:var(--accent);color:#00151a;border-color:var(--accent);font-weight:700}
button.danger{background:#3d1418;border-color:var(--err);color:#ffb3ae}
button:hover{filter:brightness(1.15)}
.mini{padding:2px 7px}
pre{background:#0b0f14;border:1px solid var(--line);border-radius:6px;padding:10px;white-space:pre-wrap;word-break:break-word;max-height:340px;overflow:auto;margin:0}
.warn{color:var(--err)}.ok{color:var(--ok)}.dim{color:var(--dim)}
.bar{position:sticky;bottom:0;background:var(--panel);border-top:1px solid var(--line);padding:10px 16px;display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.chips span{display:inline-block;border:1px solid var(--line);border-radius:999px;padding:1px 8px;margin:2px;color:var(--dim);cursor:pointer}
.chips span:hover{border-color:var(--accent);color:var(--txt)}
.spin{display:inline-block;animation:r 1s linear infinite}
@keyframes r{to{transform:rotate(360deg)}}
a{color:var(--accent)}
</style>
</head>
<body>
<header>
  <h1>✦ Lyra</h1>
  <div class="tabs" id="tabs"></div>
  <span class="dim" id="status">loading…</span>
</header>
<main id="main"></main>
<div class="bar">
  <button class="pri" onclick="generate()">▶ Generate</button>
  <button class="danger" onclick="abort()">■ Abort</button>
  <span class="dim" id="barinfo"></span>
</div>
<script>
let S=null, tab="song", editing=null;

const $=s=>document.querySelector(s);
const esc=s=>String(s).replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));

async function post(url,body){const r=await fetch(url,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body||{})});return r.json();}
async function refresh(){S=await (await fetch('/api/state')).json();render();}

function tabs(){return[['song','Song'],['advanced','Advanced'],['components','Components'],['run','Run'],['presets','Presets'],['help','Help']];}

function render(){
  $('#status').textContent = S.running ? '⚙ '+S.status : S.status;
  $('#tabs').innerHTML = tabs().map(([k,l])=>`<div class="tab ${k===tab?'on':''}" onclick="tab='${k}';render()">${l}</div>`).join('');
  const m=$('#main');
  if(tab==='run'){m.innerHTML=runView();hookRun();return;}
  if(tab==='presets'){m.innerHTML=presetView();hookPresets();return;}
  if(tab==='help'){m.innerHTML=helpView();return;}
  m.innerHTML=`<div class="grid"><div class="panel"><h2>${tab}</h2>${formView()}</div><div class="panel"><h2>preview</h2>${previewView()}</div></div>`;
  hookForm();
}

function fieldsFor(g){return S.fields.filter(f=>f.group===g);}

function formView(){
  let last='';
  return fieldsFor(tab).map(f=>{
    let head='';
    if(f.label.split(' ')[0]!==last){}
    let ctl='';
    if(f.kind==='select'){
      const auto=f.auto?`<option value="-1" ${f.index===0?'selected':''}>(auto)</option>`:'';
      const opts=f.options.map((o,i)=>`<option value="${i}" ${f.index===(f.auto?i+1:i)?'selected':''}>${esc(o)}</option>`).join('');
      ctl=`<select onchange="setSelect('${f.id}',this.value)">${auto}${opts}</select>`;
    }else if(f.kind==='bool'){
      ctl=`<input type="checkbox" style="width:auto" ${f.value==='true'?'checked':''} onchange="act('${f.id}','toggle')"/>`;
    }else if(f.kind==='text'){
      ctl=`<input type="text" value="${esc(f.value)}" onchange="setVal('${f.id}',this.value)"/>`;
    }else if(f.kind==='multiline'){
      ctl=`<textarea onchange="setVal('${f.id}',this.value)">${esc(f.value)}</textarea>`;
    }else{
      ctl=`<div style="display:flex;gap:6px"><button class="mini" onclick="act('${f.id}','cycle-')">−</button>
           <input type="text" value="${esc(f.value)}" onchange="setVal('${f.id}',this.value)"/>
           <button class="mini" onclick="act('${f.id}','cycle+')">+</button></div>`;
    }
    return `<div class="row"><label>${esc(f.label)}</label><div>${ctl}</div><div class="hint">${esc(f.help)}</div></div>`;
  }).join('');
}

function previewView(){
  const w=S.warnings.length?`<p class="warn">⚠ ${S.warnings.map(esc).join('<br>⚠ ')}</p>`:'<p class="ok">✓ setup looks good</p>';
  return `<b class="dim">caption</b><p>${esc(S.caption)}</p>
  <b class="dim">lyrics</b><pre>${esc(S.lyrics)}</pre>
  <b class="dim">output</b><p>${esc(S.out_path)}</p>${w}
  <details><summary class="dim">resolved command</summary><pre>${esc(S.command)}</pre></details>`;
}

function runView(){
  const dl=`<p><a href="/api/download?path=${encodeURIComponent(S.out_path)}" target="_blank">⬇ download ${esc(S.out_path.split('/').pop())}</a></p>`;
  const hist=S.history.length?`<div class="panel" style="margin-top:14px"><h2>recent</h2>${S.history.map(h=>`<div class="dim">${new Date(h.at*1000).toLocaleTimeString()} · ${h.success?'✓':'✗'} ${h.wall_ms?Math.round(h.wall_ms/1000)+'s':''} · ${esc(h.out)}</div>`).join('')}</div>`:'';
  return `<div class="grid"><div class="panel"><h2>log</h2><pre id="log">${esc(window._log||'')}</pre></div>
  <div class="panel"><h2>status</h2><p class="${S.running?'':'ok'}" id="runstat">${esc(S.status)} ${S.running?'<span class="spin">◐</span>':''}</p>${S.running?'':dl}${S.warnings.length?'<p class="warn">⚠ '+S.warnings.map(esc).join('<br>⚠ ')+'</p>':''}</div></div>${hist}`;
}

function presetView(){
  const chips=S.presets.length?S.presets.map(n=>`<span onclick="loadPreset('${esc(n)}')">${esc(n)}</span>`).join(''):'<span class="dim">none yet</span>';
  const del=id=>`<select id="${id}">${S.presets.map(n=>`<option>${esc(n)}</option>`).join('')}</select>`;
  return `<div class="grid"><div class="panel"><h2>save current</h2>
   <div class="row"><label>name</label><input id="pname" type="text" placeholder="my pop song"/></div>
   <button class="pri" onclick="savePreset()">💾 Save</button>
   <h2 style="margin-top:16px">load / delete</h2><div class="chips">${chips}</div>
   ${S.presets.length?`<div style="margin-top:10px">${del('pdel')} <button class="danger" onclick="delPreset()">Delete</button></div>`:''}
  </div><div class="panel"><h2>history</h2>${S.history.map(h=>`<div class="dim">${new Date(h.at*1000).toLocaleTimeString()} · ${h.success?'✓':'✗'} · ${esc(h.out)}</div>`).join('')||'<span class="dim">no runs yet</span>'}</div></div>`;
}

function helpView(){
  return `<div class="panel"><h2>how Lyra works</h2>
  <p><b>Song</b> sets the prompt: style, mood, vocals, optional caption override, lyrics, length, steps and output path.</p>
  <p><b>Advanced</b> exposes the model controls: flow AR guidance, top-k, seed, the flow/perf toggles and memory options.</p>
  <p><b>Components</b> pick the backend and which LM / flow / depth GGUF to load (options come from your model dir).</p>
  <p><b>Run</b> shows warnings and streams the audio.cpp log; <b>Generate</b> starts and <b>Abort</b> stops it.</p>
  <p><b>Presets</b> save/load full configs to <code>~/.config/lyra/presets.json</code>.</p>
  <p class="dim">Everything here mirrors the TUI (<code>lyra</code>); the web server is the same binary via <code>lyra web</code>.</p>
  </div>`;
}

// --- actions ---
async function act(id,action){S=await post('/api/field',{id,action});render();}
async function setVal(id,value){S=await post('/api/field',{id,action:'set',value});render();}
async function setSelect(id,value){let index=parseInt(value);if(isNaN(index)||index<0)index=0;S=await post('/api/select',{id,index});render();}
async function generate(){S=await post('/api/generate');window._log='';render();}
async function abort(){S=await post('/api/abort');render();}
async function savePreset(){const n=$('#pname').value.trim();if(!n)return;S=await post('/api/presets/save',{name:n});render();}
async function loadPreset(name){S=await post('/api/presets/load',{name});render();}
async function delPreset(){const sel=$('#pdel');if(!sel)return;S=await post('/api/presets/delete',{name:sel.value});render();}

function hookForm(){}
function hookRun(){}
function hookPresets(){}

// --- live log (SSE) ---
function connect(){
  const es=new EventSource('/api/events');
  es.onmessage=e=>{try{const l=JSON.parse(e.data);window._log=(window._log||'')+l+'\n';const p=document.getElementById('log');if(p){p.textContent=window._log;p.scrollTop=p.scrollHeight;}}catch(_){}};
  es.addEventListener('done',async e=>{try{const d=JSON.parse(e.data);window._log=(window._log||'')+'\n— '+d.status+' ('+Math.round(d.wall_ms/1000)+'s)\n';}catch(_){}
    es.close();window._log=window._log||'';S=await (await fetch('/api/state')).json();render();setTimeout(connect,1000);});
  es.onerror=()=>{es.close();setTimeout(connect,2000);};
}
document.addEventListener('DOMContentLoaded',()=>{refresh();connect();});
setInterval(async()=>{if(tab==='run'||S&&S.running){const j=await (await fetch('/api/state')).json();const was=S&&S.running;S=j;if(tab==='run')render();}},1500);
</script>
</body>
</html>
"##;
