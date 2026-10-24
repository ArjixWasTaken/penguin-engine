//! Command-line client of the `wallpaper` daemon's control socket.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use wpe_ipc::{Client, Fill, OutputProperties, Request, Response, Status};

#[derive(Parser)]
#[command(about = "Control the wallpaper daemon")]
struct Args {
    /// Print the daemon's raw JSON response.
    #[arg(long, global = true)]
    json: bool,
    /// Control socket, instead of $XDG_RUNTIME_DIR/wallpaper-engine-rs.sock.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show the outputs, their wallpapers and the settings.
    Status,
    /// Show a wallpaper: a directory, scene.pkg or scene JSON, or the id of
    /// an installed workshop wallpaper.
    Set {
        #[arg(long)]
        output: Option<String>,
        wallpaper: String,
    },
    /// List the wallpapers' user properties.
    Properties {
        #[arg(long)]
        output: Option<String>,
    },
    /// Change a user property; the value as JSON (`true`, `0.5`, `"1 0 0"`)
    /// or else a string.
    Property {
        #[arg(long)]
        output: Option<String>,
        name: String,
        value: String,
    },
    Pause {
        #[arg(long)]
        output: Option<String>,
    },
    Resume {
        #[arg(long)]
        output: Option<String>,
    },
    /// Sound and audio reactivity: on or off.
    Audio { state: OnOff },
    /// Volume in percent.
    Volume {
        #[arg(value_parser = clap::value_parser!(u32).range(0..=100))]
        volume: u32,
    },
    /// Frame rate cap (0 follows the display).
    Fps { fps: u32 },
    /// How wallpapers fill the outputs: crop, fit or stretch.
    Fill {
        #[arg(long)]
        output: Option<String>,
        fill: Fill,
    },
    /// Re-read the config file.
    Reload,
    /// Stop the daemon.
    Quit,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

/// A path the user named, absolute (the daemon runs elsewhere); a workshop
/// id or anything else that isn't a local file goes as is.
fn wallpaper_arg(spec: &str) -> String {
    let path = Path::new(spec);
    if path.exists()
        && let Ok(abs) = path.canonicalize()
    {
        return abs.display().to_string();
    }
    spec.to_owned()
}

fn request(command: Command) -> Request {
    match command {
        Command::Status => Request::Status,
        Command::Set { output, wallpaper } => Request::Set {
            output,
            wallpaper: wallpaper_arg(&wallpaper),
        },
        Command::Properties { output } => Request::Properties { output },
        Command::Property {
            output,
            name,
            value,
        } => Request::Property {
            output,
            name,
            value: serde_json::from_str(&value).unwrap_or(Value::String(value)),
        },
        Command::Pause { output } => Request::Pause { output },
        Command::Resume { output } => Request::Resume { output },
        Command::Audio { state } => Request::Audio {
            enabled: matches!(state, OnOff::On),
        },
        Command::Volume { volume } => Request::Volume { volume },
        Command::Fps { fps } => Request::Fps { fps },
        Command::Fill { output, fill } => Request::Fill { output, fill },
        Command::Reload => Request::ReloadSettings,
        Command::Quit => Request::Quit,
    }
}

fn print_status(status: &Status) {
    let s = &status.settings;
    let fps = match s.fps {
        0 => "display".to_owned(),
        n => n.to_string(),
    };
    println!(
        "settings: fps {fps}, audio {}, volume {}%, fill {}, layer {}, passthrough {}",
        if s.audio { "on" } else { "off" },
        s.volume,
        s.fill,
        s.layer,
        s.passthrough
    );
    if status.outputs.is_empty() {
        println!("no outputs");
    }
    for o in &status.outputs {
        println!(
            "{}: {}x{} @{:.2}, fill {}",
            o.name, o.width, o.height, o.scale, o.fill
        );
        match &o.wallpaper {
            Some(w) => {
                let id = w
                    .workshop_id
                    .as_deref()
                    .map(|id| format!(" [{id}]"))
                    .unwrap_or_default();
                let state = if o.paused {
                    "paused".to_owned()
                } else {
                    format!("{:.1} fps", o.fps)
                };
                println!("  {} \"{}\"{id}, {state}", w.kind, w.title);
                println!("  {}", w.path);
            }
            None => println!("  no wallpaper"),
        }
    }
}

fn print_properties(list: &[OutputProperties]) {
    for (i, output) in list.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}: {}", output.output, output.wallpaper);
        if output.properties.is_empty() {
            println!("  no properties");
        }
        for p in &output.properties {
            let mut extra = Vec::new();
            if let (Some(min), Some(max)) = (p.min, p.max) {
                extra.push(format!("{min}..{max}"));
            }
            if !p.options.is_empty() {
                let options: Vec<String> = p
                    .options
                    .iter()
                    .map(|o| match &o.label {
                        Some(label) => format!("{}={label}", o.value),
                        None => o.value.to_string(),
                    })
                    .collect();
                extra.push(options.join(" | "));
            }
            if p.value != p.default {
                extra.push(format!("default {}", p.default));
            }
            let extra = if extra.is_empty() {
                String::new()
            } else {
                format!("  ({})", extra.join("; "))
            };
            let label = p.text.as_deref().unwrap_or("");
            println!("  {} [{}] = {}{extra}", p.name, p.kind, p.value);
            if !label.is_empty() && label != p.name {
                println!("      {label}");
            }
        }
    }
}

fn run(args: Args) -> Result<Response> {
    let mut client = match &args.socket {
        Some(path) => Client::connect(path),
        None => Client::connect_default(),
    }
    .context("cannot reach the wallpaper daemon (is it running?)")?;
    let response = client
        .request(&request(args.command))
        .context("talking to the wallpaper daemon")?;
    Ok(response)
}

fn main() -> ExitCode {
    let args = Args::parse();
    let json = args.json;
    let response = match run(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wallpaperctl: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        print!("{}", wpe_ipc::encode_line(&response));
    } else {
        if let Some(status) = &response.status {
            print_status(status);
        }
        if let Some(properties) = &response.properties {
            print_properties(properties);
        }
    }
    match response.into_result() {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            if !json {
                eprintln!("wallpaperctl: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Request {
        let mut argv = vec!["wallpaperctl"];
        argv.extend_from_slice(args);
        request(Args::try_parse_from(argv).unwrap().command)
    }

    #[test]
    fn commands_make_requests() {
        assert_eq!(parse(&["status"]), Request::Status);
        assert_eq!(
            parse(&["set", "--output", "DP-1", "3566329932"]),
            Request::Set {
                output: Some("DP-1".into()),
                wallpaper: "3566329932".into()
            }
        );
        assert_eq!(
            parse(&["property", "speed", "0.5"]),
            Request::Property {
                output: None,
                name: "speed".into(),
                value: serde_json::json!(0.5)
            }
        );
        assert_eq!(
            parse(&["property", "label", "hello"]),
            Request::Property {
                output: None,
                name: "label".into(),
                value: serde_json::json!("hello")
            }
        );
        assert_eq!(parse(&["audio", "off"]), Request::Audio { enabled: false });
        assert_eq!(
            parse(&["fill", "fit", "--json"]),
            Request::Fill {
                output: None,
                fill: Fill::Fit
            }
        );
        assert_eq!(parse(&["reload"]), Request::ReloadSettings);
        assert!(Args::try_parse_from(["wallpaperctl", "volume", "101"]).is_err());
        assert!(Args::try_parse_from(["wallpaperctl", "fill", "zoom"]).is_err());
    }

    #[test]
    fn local_paths_become_absolute() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            wallpaper_arg("."),
            here.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(wallpaper_arg("3566329932"), "3566329932");
    }
}
