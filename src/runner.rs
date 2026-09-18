use crate::core::{self, Config, HistoryEntry, build_argv};
use crate::detect::{Paths, expand_tilde};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const LOG_LIMIT: usize = 20_000;
const LOG_TRIM: usize = 5_000;

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Status {
    Running,
    Success,
    Failed(i32),
    Aborted,
}

impl Status {
    pub fn is_running(&self) -> bool {
        matches!(self, Status::Running)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Status::Success)
    }

    pub fn label(&self) -> String {
        match self {
            Status::Running => "running".into(),
            Status::Success => "success".into(),
            Status::Failed(0) => "failed".into(),
            Status::Failed(c) => format!("failed (exit {c})"),
            Status::Aborted => "aborted".into(),
        }
    }
}

pub struct Job {
    pub id: u64,
    pub config: Config,
    log: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<Status>>,
    child: Arc<Mutex<Option<Child>>>,
    started: Instant,
    recorded: Arc<AtomicBool>,
    out_path: String,
}

impl Job {
    /// Spawn `audiocpp_cli` for `config`. A background thread watches the child,
    /// keeps `status` current, and writes exactly one history row when it ends.
    pub fn start(paths: &Paths, config: &Config) -> std::io::Result<Job> {
        let argv = build_argv(paths, config);
        let out = expand_tilde(&config.out);
        if let Some(parent) = out.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let status = Arc::new(Mutex::new(Status::Running));
        let recorded = Arc::new(AtomicBool::new(false));
        let started = Instant::now();

        if let Some(o) = child.stdout.take() {
            spawn_reader(o, log.clone());
        }
        if let Some(e) = child.stderr.take() {
            spawn_reader(e, log.clone());
        }
        let child = Arc::new(Mutex::new(Some(child)));

        {
            let c = child.clone();
            let s = status.clone();
            let cfg = config.clone();
            let rec = recorded.clone();
            std::thread::spawn(move || {
                loop {
                    let done = {
                        let mut guard = c.lock().unwrap();
                        match guard.as_mut() {
                            Some(ch) => match ch.try_wait() {
                                Ok(Some(st)) => {
                                    let mut g = s.lock().unwrap();
                                    if *g == Status::Running {
                                        *g = if st.success() {
                                            Status::Success
                                        } else {
                                            Status::Failed(st.code().unwrap_or(-1))
                                        };
                                    }
                                    true
                                }
                                Ok(None) => false,
                                Err(_) => {
                                    let mut g = s.lock().unwrap();
                                    if *g == Status::Running {
                                        *g = Status::Failed(-1);
                                    }
                                    true
                                }
                            },
                            None => true,
                        }
                    };
                    if done {
                        let end = s.lock().unwrap().clone();
                        record_history(&cfg, &end, started.elapsed().as_secs_f64() * 1000.0, &rec);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
            });
        }

        Ok(Job {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            config: config.clone(),
            log,
            status,
            child,
            started,
            recorded,
            out_path: out.to_string_lossy().into_owned(),
        })
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    pub fn running(&self) -> bool {
        self.status.lock().unwrap().is_running()
    }

    pub fn lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Return lines after `cursor`; if the buffer has rolled past `cursor`, the
    /// whole buffer is returned instead (so a client never misses new output).
    pub fn lines_since(&self, cursor: usize) -> (usize, Vec<String>) {
        let g = self.log.lock().unwrap();
        let start = if cursor > g.len() { 0 } else { cursor };
        (g.len(), g[start..].to_vec())
    }

    pub fn abort(&self) {
        let mut s = self.status.lock().unwrap();
        if s.is_running() {
            *s = Status::Aborted;
        }
        drop(s);
        if let Some(ch) = self.child.lock().unwrap().as_mut() {
            let _ = ch.kill();
        }
    }

    pub fn wall_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    pub fn out_path(&self) -> &str {
        &self.out_path
    }

    pub fn output_exists(&self) -> bool {
        std::path::Path::new(&self.out_path).is_file()
    }

    /// Persist one history row. The background watcher normally does this; the
    /// call is idempotent so UIs may also call it defensively.
    pub fn record(&self) {
        let status = self.status();
        record_history(&self.config, &status, self.wall_ms(), &self.recorded);
    }
}

fn record_history(config: &Config, status: &Status, wall_ms: f64, recorded: &AtomicBool) {
    if recorded.swap(true, Ordering::SeqCst) {
        return;
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    core::append_history(HistoryEntry {
        at,
        out: config.out.clone(),
        caption: config.caption(),
        success: status.is_success(),
        wall_ms,
    });
}

fn spawn_reader<R: std::io::Read + Send + 'static>(r: R, log: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(r);
        for line in reader.lines().map_while(Result::ok) {
            let mut g = log.lock().unwrap();
            g.push(line);
            if g.len() > LOG_LIMIT {
                g.drain(0..LOG_TRIM);
            }
        }
    });
}
