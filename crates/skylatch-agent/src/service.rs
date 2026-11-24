//! The `SkylatchAgent` Windows service: SCM entry point, installation and removal.

use crate::cli::AgentArgs;
use clap::{Args, Parser, Subcommand};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::restrict_to_admins;

pub const SERVICE_NAME: &str = "SkylatchAgent";
pub const DISPLAY_NAME: &str = "Skylatch Host Agent";

#[derive(Args, Debug)]
pub struct Service {
    #[command(subcommand)]
    pub action: ServiceAction,
}

#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    /// Entry point for the service control manager; not for interactive use.
    Run(Box<AgentArgs>),
    /// Create or update the LocalSystem service (requires an elevated prompt).
    Install(Install),
    /// Stop and remove the service (requires an elevated prompt).
    Uninstall,
}

#[derive(Args, Debug)]
pub struct Install {
    /// Configuration the service reads (default: `%ProgramData%\Skylatch\agent.json`).
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Start the service, restarting it when it already runs.
    #[arg(long)]
    pub start: bool,
    /// Extra agent options for the service, after `--`, e.g. `-- --enable-direct`.
    #[arg(last = true, value_name = "AGENT_OPTIONS")]
    pub agent_args: Vec<String>,
}

#[derive(Parser)]
#[command(no_binary_name = true)]
struct RunCommand {
    #[command(flatten)]
    agent: AgentArgs,
}

impl Install {
    /// Arguments after the executable in the service command line. The agent options are
    /// validated now so that a typo fails the install instead of every service start.
    pub fn launch_arguments(&self, config: &Path) -> Result<Vec<OsString>, String> {
        let mut args: Vec<OsString> = vec!["--config".into(), config.into()];
        args.extend(self.agent_args.iter().map(OsString::from));
        RunCommand::try_parse_from(&args).map_err(|err| format!("invalid agent options: {err}"))?;
        args.splice(0..0, ["service".into(), "run".into()]);
        Ok(args)
    }
}

impl Service {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        #[cfg(windows)]
        {
            match self.action {
                ServiceAction::Run(agent) => windows::dispatch(*agent),
                ServiceAction::Install(install) => windows::install(&install),
                ServiceAction::Uninstall => windows::uninstall(),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = self.action;
            Err("the service subcommand is only available on Windows".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};

    fn parse(args: &[&str]) -> Result<ServiceAction, clap::Error> {
        let cli =
            Cli::try_parse_from(std::iter::once("skylatch-agent").chain(args.iter().copied()))?;
        match cli.command {
            Some(Command::Service(service)) => Ok(service.action),
            _ => panic!("not a service command"),
        }
    }

    #[test]
    fn service_subcommands_parse() {
        match parse(&[
            "service",
            "run",
            "--config",
            "C:/cfg/agent.json",
            "--enable-direct",
        ])
        .unwrap()
        {
            ServiceAction::Run(agent) => {
                assert_eq!(agent.config, Some(PathBuf::from("C:/cfg/agent.json")));
                assert!(agent.enable_direct);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            parse(&["service", "uninstall"]).unwrap(),
            ServiceAction::Uninstall
        ));
        match parse(&[
            "service",
            "install",
            "--start",
            "--",
            "--direct-port",
            "17901",
        ])
        .unwrap()
        {
            ServiceAction::Install(install) => {
                assert!(install.start);
                assert_eq!(install.config, None);
                assert_eq!(install.agent_args, ["--direct-port", "17901"]);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(parse(&["service"]).is_err());
        assert!(parse(&["service", "restart"]).is_err());
        assert!(
            Cli::try_parse_from(["skylatch-agent", "--enable-direct", "service", "uninstall"])
                .is_err()
        );
    }

    #[test]
    fn foreground_mode_is_unchanged() {
        let cli = Cli::try_parse_from(["skylatch-agent", "--engine-bin", "engine", "--fps", "60"])
            .unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.agent.engine_bin, Some(PathBuf::from("engine")));
        assert_eq!(cli.agent.engine_args(), ["--fps", "60"]);
        assert_eq!(cli.agent.config, None);
        assert!(Cli::try_parse_from(["skylatch-agent", "--fps", "60"]).is_err());
    }

    #[test]
    fn install_builds_the_service_command_line() {
        let install = Install {
            config: None,
            start: false,
            agent_args: vec!["--enable-direct".into(), "--room".into(), "Lab 2".into()],
        };
        let config = Path::new(r"C:\ProgramData\Skylatch\agent.json");
        assert_eq!(
            install.launch_arguments(config).unwrap(),
            [
                "service",
                "run",
                "--config",
                r"C:\ProgramData\Skylatch\agent.json",
                "--enable-direct",
                "--room",
                "Lab 2",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn install_rejects_invalid_agent_options() {
        let config = Path::new("agent.json");
        for bad in [
            vec!["--no-such-flag"],
            vec!["--config", "other.json"],
            vec!["--fps", "60"],
            vec!["join", "https://hub"],
        ] {
            let install = Install {
                config: None,
                start: false,
                agent_args: bad.iter().map(|s| s.to_string()).collect(),
            };
            assert!(install.launch_arguments(config).is_err(), "{bad:?}");
        }
    }
}
