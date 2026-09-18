mod core;
mod detect;
mod runner;
mod tui;
mod web;

use anyhow::Result;
use crossterm::event::{self, Event};
use std::path::PathBuf;
use std::time::Duration;
use tui::TuiApp;

const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "\
Lyra — interactive local song generation with MiniMax-Music3 (audio.cpp)

USAGE:
    lyra [OPTIONS]              launch the TUI
    lyra web [OPTIONS]          launch the web UI (same features)
    lyra tui [OPTIONS]          launch the TUI explicitly

OPTIONS:
    --bin <path>       audiocpp_cli binary (default: auto-detect under ~/audio.cpp/build)
    --model <dir>      MiniMax-Music3-GGUF directory (default: ~/models/MiniMax-Music3-GGUF)
    --backend <name>   cuda | metal | vulkan | hip | cpu (default: inferred from the binary)
    --host <addr>      web bind host (default: 127.0.0.1)
    --port <port>      web bind port (default: 8282)
    --dry-run          print the audiocpp_cli command and exit
    --print-config     print resolved paths/components, warnings, and exit
    -V, --version      print the version and exit
    -h, --help         show this help

TUI keys: Tab switches tabs · ↑/↓ move · ←/→ adjust · Enter edit · g generate · ? help
Web UI:   http://127.0.0.1:8282
";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut web_mode = false;
    let mut bin: Option<PathBuf> = None;
    let mut model: Option<PathBuf> = None;
    let mut backend: Option<String> = None;
    let mut host = "127.0.0.1".to_string();
    let mut port: u16 = 8282;
    let mut dry_run = false;
    let mut print_config = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg, None),
        };
        let next = |i: &mut usize| -> Option<String> {
            if let Some(v) = &inline {
                Some(v.clone())
            } else {
                *i += 1;
                args.get(*i).cloned()
            }
        };
        match flag {
            "web" => web_mode = true,
            "tui" => web_mode = false,
            "--bin" => bin = next(&mut i).map(PathBuf::from),
            "--model" => model = next(&mut i).map(PathBuf::from),
            "--backend" => backend = next(&mut i),
            "--host" => {
                if let Some(v) = next(&mut i) {
                    host = v;
                }
            }
            "--port" => match next(&mut i).and_then(|s| s.parse::<u16>().ok()) {
                Some(v) => port = v,
                None => {
                    eprintln!("error: --port requires a number between 0 and 65535");
                    std::process::exit(2);
                }
            },
            "--dry-run" => dry_run = true,
            "--print-config" => print_config = true,
            "-V" | "--version" => {
                println!("lyra {VERSION}");
                return Ok(());
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => {
                eprintln!("error: unknown argument: {other}\n");
                eprint!("{HELP}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let paths = detect::detect(bin, model, backend);

    if print_config {
        println!("binary:  {}", paths.bin.display());
        println!("model:   {}", paths.model_dir.display());
        println!("backend: {}", paths.backend);
        println!("lm:      {:?}", paths.language_model);
        println!("flow:    {:?}", paths.flow_transformer);
        println!("depth:   {:?}", paths.depth_decoder);
        for w in core::Config::default().warnings(&paths) {
            println!("warning: {w}");
        }
        return Ok(());
    }

    if dry_run {
        let cfg = core::Config::default();
        println!("{}", core::shell_join(&core::build_argv(&paths, &cfg)));
        return Ok(());
    }

    if web_mode {
        if host != "127.0.0.1" && host != "localhost" && host != "::1" {
            eprintln!(
                "⚠  binding {host}: the web UI has no authentication — keep it on a trusted network"
            );
        }
        return web::serve(paths, &host, port);
    }
    run_tui(TuiApp::new(paths))
}

fn run_tui(mut app: TuiApp) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::from_millis(120))?
                && let Event::Key(key) = event::read()?
            {
                app.on_key(key);
            }
            app.on_tick();
            if app.should_quit {
                break;
            }
        }
        Ok(())
    })();
    ratatui::restore();
    if let Some(job) = &app.job
        && job.running()
    {
        job.abort();
        std::thread::sleep(Duration::from_millis(250));
    }
    result
}
