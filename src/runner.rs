use crate::core::{self, build_argv, Config, HistoryEntry};
use crate::detect::{expand_tilde, Paths};
use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, PartialEq)]
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
    pub fn label(&self) -> String {
        match self {
            Status::Running => "running".into(),
            Status::Success => "success".into(),
            Status::Failed(c) => format!("failed (exit {c})"),
            Status::Aborted => "aborted".into(),
        }
    }
}

pub struct Job {
    pub config: Config,
    log: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<Status>>,
    child: Arc<Mutex<Option<Child>>>,
    started: Instant,
    recorded: Mutex<bool>,
}

impl Job {
    pub fn start(paths: &Paths, config: &Config) -> std::io::Result<Job> {
        let argv = build_argv(paths, config);
        let out = expand_tilde(&config.out);
        if let Some(p) = out.parent() {
            let _ = std::fs::create_dir_all(p);
        }
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let status = Arc::new(Mutex::new(Status::Running));
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
            std::thread::spawn(move || loop {
                {
                    let mut guard = c.lock().unwrap();
                    match guard.as_mut() {
                        Some(ch) => match ch.try_wait() {
                            Ok(Some(st)) => {
                                let code = st.code().unwrap_or(-1);
                                let mut g = s.lock().unwrap();
                                if *g == Status::Running {
                                    *g = if st.success() {
                                        Status::Success
                                    } else {
                                        Status::Failed(code)
                                    };
                                }
                                break;
                            }
                            Ok(None) => {}
                            Err(_) => {
                                *s.lock().unwrap() = Status::Failed(-1);
                                break;
                            }
                        },
                        None => break,
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            });
        }

        Ok(Job {
            config: config.clone(),
            log,
            status,
            child,
            started: Instant::now(),
            recorded: Mutex::new(false),
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

    pub fn abort(&self) {
        if let Some(ch) = self.child.lock().unwrap().as_mut() {
            let _ = ch.kill();
        }
        let mut s = self.status.lock().unwrap();
        if s.is_running() {
            *s = Status::Aborted;
        }
    }

    pub fn wall_ms(&self) -> f64 {
        self.started.elapsed().as_secs_f64() * 1000.0
    }

    pub fn elapsed_secs(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }

    /// Persist one history row after the job reaches a terminal state.
    pub fn record(&self, success: bool) {
        let mut r = self.recorded.lock().unwrap();
        if *r {
            return;
        }
        *r = true;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        core::append_history(HistoryEntry {
            at,
            out: self.config.out.clone(),
            caption: self.config.caption(),
            success,
            wall_ms: self.wall_ms(),
        });
    }
}

fn spawn_reader<R: std::io::Read + Send + 'static>(r: R, log: Arc<Mutex<Vec<String>>>) {
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(r);
        for line in reader.lines().map_while(Result::ok) {
            let mut g = log.lock().unwrap();
            g.push(line);
            if g.len() > 20_000 {
                g.drain(0..5_000);
            }
        }
    });
}
