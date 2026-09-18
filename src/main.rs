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

const HELP: &str = "\
Lyra — interactive local song generation with MiniMax-Music3 (audio.cpp)

USAGE:
    lyra [OPTIONS]              launch the TUI
    lyra web [OPTIONS]          launch the web UI (same features)

OPTIONS:
    --bin <path>       audiocpp_cli binary (default: auto-detect under ~/audio.cpp/build)
    --model <dir>      MiniMax-Music3-GGUF directory (default: ~/models/MiniMax-Music3-GGUF)
    --backend <name>   cuda | metal | vulkan | hip | cpu (default: inferred from the binary)
    --host <addr>      web bind host (default: 127.0.0.1)
    --port <port>      web bind port (default: 8282)
    --dry-run          print the audiocpp_cli command and exit
    --print-config     print resolved paths/components, warnings, and exit
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
    if args.first().map(|a| a == "web").unwrap_or(false) {
        web_mode = true;
        i = 1;
    }
    while i < args.len() {
        match args[i].as_str() {
            "--bin" => {
                i += 1;
                bin = args.get(i).map(PathBuf::from);
            }
            "--model" => {
                i += 1;
                model = args.get(i).map(PathBuf::from);
            }
            "--backend" => {
                i += 1;
                backend = args.get(i).cloned();
            }
            "--host" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    host = v.clone();
                }
            }
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i).and_then(|s| s.parse().ok()) {
                    port = v;
                }
            }
            "--dry-run" => dry_run = true,
            "--print-config" => print_config = true,
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(());
            }
            other => {
                eprintln!("unknown argument: {other}\n");
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
        return web::serve(paths, &host, port);
    }
    run_tui(TuiApp::new(paths))
}

fn run_tui(mut app: TuiApp) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|frame| app.draw(frame))?;
            if event::poll(Duration::from_millis(120))? {
                if let Event::Key(key) = event::read()? {
                    app.on_key(key);
                }
            }
            app.on_tick();
            if app.should_quit {
                break;
            }
        }
        Ok(())
    })();
    ratatui::restore();
    result
}
