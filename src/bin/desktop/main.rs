#![cfg_attr(windows, windows_subsystem = "windows")]

mod activity;
mod app;
mod charts;
mod clip;
mod connection;
mod detail;
mod feed;
mod form;
mod format;
mod health;
mod list;
mod logs;
mod message;
mod output;
mod schedules;
mod sidebar;
mod theme;
mod tree;
mod widgets;

use app::App;
use clap::Parser;
use iced::{Size, window};
use k3up::{
    autostart::{bundled_agent, desktop_agent},
    platform, services,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "K3 Up desktop control console")]
struct Options {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Start the agent and exit. Used by the Windows login entry.
    #[arg(long, hide = true)]
    start_agent: bool,
    /// Register the agent to start at login, start it, and exit. Used by installers.
    #[arg(long, hide = true)]
    register_agent: bool,
}

fn main() -> iced::Result {
    let options = Options::parse();
    let managed = options.data_dir.is_none();
    let data = options.data_dir.unwrap_or_else(platform::default_data_dir);
    let services_mode = services::machine_mode_on() || services::active(&data);
    let agent = desktop_agent(bundled_agent(), &data, services_mode);
    if options.start_agent || options.register_agent {
        if let Some(login) = agent {
            // A reinstall must not undo a login item the user turned off.
            if options.register_agent
                && !login.opted_out()
                && let Err(error) = login.register()
            {
                eprintln!("{error:#}");
            }
            if let Err(error) = login.start() {
                eprintln!("{error:#}");
            }
        }
        return Ok(());
    }
    // Only the default agent is set up automatically; an explicit --data-dir is left alone.
    let login = agent.filter(|_| managed);
    let icon = window::icon::from_file_data(
        include_bytes!("../../../assets/branding/k3-up-app-icon.png"),
        None,
    )
    .ok();
    iced::application("K3 Up", App::update, App::view)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(1240.0, 860.0),
            min_size: Some(Size::new(1000.0, 680.0)),
            icon,
            ..Default::default()
        })
        .run_with(move || App::new(data, login))
}
