mod adapters;
mod config;
mod domain;
mod present;
mod services;
mod ui;

use anyhow::Result;
use clap::Parser;

use adapters::{FsRegistryRepository, FsStateStore, TmuxHost};
use config::{Command, Config, Format};
use services::{list_sessions, add_bookmark, rm_bookmark, go_to_session, RegistryRepository};
use ui::TerminalState;

use crate::services::SessionHost;

fn main() -> Result<()> {
    let config = Config::parse();
    let repo = FsRegistryRepository::new(config.registry_path());

    match &config.command {
        Some(Command::List) => {
            for agent in repo.load()?.agents() {
                println!("{}", agent.name);
            }
            Ok(())
        }
        Some(Command::Status { format }) => {
            let store = FsStateStore::new(config.state_path());
            let host = TmuxHost;
            let sessions = list_sessions(&repo, &store, &host)?;
            let current = host.current();
            let output = match format {
                Format::Plain => present::plain(&sessions),
                Format::Tmux => present::tmux(&sessions, current.as_ref().map(|a| a.name.as_str())),
            };
            println!("{output}");
            Ok(())
        }
        Some(Command::Add) => {
            let host = TmuxHost;
            add_bookmark(&repo, &host)?;
            Ok(())
        }
        Some(Command::Rm { target }) => {
            rm_bookmark(&repo, target.to_string())?;
            Ok(())
        }
        Some(Command::Go { target }) => {
            let host = TmuxHost;
            if let Some(agent) = go_to_session(&repo, &host, target.to_string())? {
                println!("{}={}", agent.name, agent.path);
            }
            Ok(())
        }
        None => {
            let mut terminal_state = TerminalState::new()?;
            ui::run(&mut terminal_state, &config)
        }
    }
}
