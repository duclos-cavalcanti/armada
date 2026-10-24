use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(name = "crew", version, about)]
pub struct Config {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Crew directory holding `registry` and `state/` [default: ~/.config/crew].
    #[arg(long, env = "CREW_DIRECTORY", global = true)]
    pub crew_dir: Option<PathBuf>,

    /// Poll interval, milliseconds (TUI).
    #[arg(long, default_value_t = 250)]
    pub tick_ms: u64,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List registered sessions, one per line.
    List,
    /// Show sessions with their agent states.
    Status {
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Plain)]
        format: Format,
    },
    /// Add current session to be bookmarked
    Add,
    /// Remove current session from bookmarks
    Rm {
        /// Remove target.
        target: String,
    },
    Go {
        /// Session target.
        target: String,
    }
}

#[derive(ValueEnum, Clone, Debug)]
pub enum Format {
    Plain,
    Tmux,
}

impl Config {
    /// The crew root, defaulting to ~/.config/crew. `registry` and `state/`
    /// are derived from it, mirroring the bash tool's DIR-relative layout.
    pub fn crew_dir(&self) -> PathBuf {
        self.crew_dir.clone().unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config/crew")
        })
    }

    pub fn registry_path(&self) -> PathBuf {
        self.crew_dir().join("registry")
    }

    pub fn state_path(&self) -> PathBuf {
        self.crew_dir().join("state")
    }
}
