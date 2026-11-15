//! `craft-mcp [--cache-dir DIR] [--config FILE]`: the read-only MCP server
//! over Craft Status's cache, on stdin and stdout. By default it reads the
//! app's cache directory and `~/.craft_status_config.toml`.

use std::path::PathBuf;

use craft_core::{Cache, Config};
use craft_mcp::{Server, Sources};

const USAGE: &str = "usage: craft-mcp [--cache-dir DIR] [--config FILE]";

fn main() {
    let mut dir: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--cache-dir" => &mut dir,
            "--config" => &mut config,
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => {
                eprintln!("craft-mcp: unknown argument {arg:?}\n{USAGE}");
                std::process::exit(2);
            }
        };
        match args.next() {
            Some(v) => *slot = Some(PathBuf::from(v)),
            None => {
                eprintln!("craft-mcp: {arg} needs a value\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let dir = match dir
        .map(Ok)
        .unwrap_or_else(|| Cache::default_location().map(|c| c.dir().to_path_buf()))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("craft-mcp: {e}");
            std::process::exit(1);
        }
    };
    let config = config.or_else(|| Config::path().ok());
    let server = Server::new(Sources { dir, config });
    if let Err(e) = server.serve(std::io::stdin().lock(), std::io::stdout().lock()) {
        eprintln!("craft-mcp: {e}");
        std::process::exit(1);
    }
}
