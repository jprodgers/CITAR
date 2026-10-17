//! The command line (design/phase4-helper.md P4.3.2). Every flag of protocol 1's helper
//! (`citar/worker/__main__.py` `main`, lines 156-186) with its meaning, so a 0.1.5 service line or
//! shortcut keeps working: `--server --token --config --base-url --provider --api-key-env
//! --max-concurrent --name --model --quiet --insecure --no-hardware --hardware -v`. Long names may
//! be shortened to any unambiguous prefix and a flag given twice keeps its last value, as
//! argparse allowed. The flags work before or after the command; `run` is the default command.
//!
//! New in protocol 2: `--home`, `--no-llm`, `--cpu`, `--ca-file`, `--no-power`, `--updates`,
//! `--service`, `--self-test`, and the `setup`, `scan [FILE]`, `update check|apply|rollback` (4-13)
//! and (internal) `job-host` commands, and the hidden `--supervised` (a console run's supervisor
//! gives it to the helper it runs after an update, P4.3.8). They make a few prefixes ambiguous that
//! were not (`--serv`, `--no`, `--c`, `--se`), which are refused as argparse refuses an
//! ambiguous one. `service` arrives with package 4-14. A `test-update-key` build also takes
//! `--update-key FILE`.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// The command line.
#[derive(Clone, Debug, Default, Parser)]
#[command(
    name = "citar-helper",
    about = "Lend this machine's model endpoint, and if you choose its spare CPU, to a CITAR server.",
    after_help = "The connection is outbound: nothing on this machine needs to be reachable from \
                  the internet.",
    disable_version_flag = true,
    infer_long_args = true,
    args_override_self = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
    #[command(flatten)]
    pub opts: Options,
}

/// What to do.
#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Connect to the server and serve it (the default).
    Run,
    /// Ask for the server, a new token and whether to lend CPU, and save them in helper.json.
    Setup,
    /// Scan this machine's hardware and print the report (or write it to FILE), to import on the
    /// Servers page: the standalone collector. Nothing is sent anywhere.
    Scan {
        /// Where to write the report (default: print it).
        file: Option<PathBuf>,
    },
    /// The job host (internal: the helper runs it as its child, P4.4.3): reads job frames as JSON
    /// lines on its standard input and plays them under the limits it is given.
    #[command(name = "job-host", hide = true)]
    JobHost,
    /// Self-update: what this helper knows of updates, install the one its server relayed, or put
    /// the binary an update replaced back.
    Update {
        #[command(subcommand)]
        action: UpdateAction,
    },
}

/// What `update` does.
#[derive(Clone, Debug, PartialEq, Eq, Subcommand)]
pub enum UpdateAction {
    /// Print this helper's version, the update keys it trusts, its policy, and what its home
    /// holds of updates: the last manifest its server relayed (and whether it is newer), a
    /// download waiting, an update in progress or rolled back.
    Check,
    /// Install the update the server last relayed, now: checked, downloaded, checked again and
    /// self-tested as an automatic update is, then put in place (the binary it replaces kept as
    /// its name with .old added, beside it). Restart the helper (or its service) to run it.
    Apply,
    /// Put the binary the last update replaced (its name with .old added) back in place; the
    /// replaced one takes that name, so the update can be put back the same way. Its release is
    /// held back: this helper does not install it again by itself until helper/update/failed.json
    /// is deleted from its home (or update apply is run).
    Rollback,
}

/// The flags, each also accepted after the command.
#[derive(Clone, Debug, Default, Args)]
pub struct Options {
    /// The helper's home: helper.json and its own files (default: CITAR_DATA_DIR, else the
    /// per-user CITAR folder).
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,
    /// The CITAR server's address, as a browser opens it.
    #[arg(
        long = "server",
        global = true,
        value_name = "URL",
        help = "The CITAR server, e.g. https://citar.example.com"
    )]
    pub server_url: Option<String>,
    /// The worker token, issued on the Servers page.
    #[arg(long, global = true)]
    pub token: Option<String>,
    /// A JSON file with these settings (keeps the token out of your shell history); replaces
    /// helper.json.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// The local model endpoint (default: LM Studio on this machine).
    #[arg(
        long = "base-url",
        global = true,
        value_name = "URL",
        help = "The local model endpoint (default http://localhost:1234/v1)"
    )]
    pub base_url: Option<String>,
    /// What serves the model locally.
    #[arg(long, global = true, value_parser = ["lmstudio", "ollama", "openai_compatible", "openai"])]
    pub provider: Option<String>,
    /// The environment variable holding the API key, if this helper fronts a paid API. The key
    /// is read here and never sent to the server.
    #[arg(long = "api-key-env", global = true, value_name = "NAME")]
    pub api_key_env: Option<String>,
    /// How many requests this machine takes at once (default 1, at most 16).
    #[arg(long = "max-concurrent", global = true, value_name = "N")]
    pub max_concurrent: Option<i64>,
    /// What to call this machine on the server.
    #[arg(long, global = true)]
    pub name: Option<String>,
    /// Serve only this model (repeatable; default is everything the endpoint lists).
    #[arg(long = "model", global = true, action = ArgAction::Append)]
    pub models: Vec<String>,
    /// Local quiet hours, e.g. "22:00-06:00" or "Sat 00:00-09:00", enforced here so the server
    /// cannot override them. Repeatable.
    #[arg(long, global = true, value_name = "WINDOW", action = ArgAction::Append)]
    pub quiet: Vec<String>,
    /// Skip TLS verification: honoured only for loopback and private-network servers.
    #[arg(long, global = true)]
    pub insecure: bool,
    /// Trust this CA certificate (PEM) too: the right answer to a self-signed server.
    #[arg(long = "ca-file", global = true, value_name = "FILE")]
    pub ca_file: Option<PathBuf>,
    /// Do not report this machine's hardware to the server.
    #[arg(long = "no-hardware", global = true)]
    pub no_hardware: bool,
    /// Report the hardware in FILE (a collector's output) instead of scanning this machine; read
    /// again at every connection.
    #[arg(long = "hardware", global = true, value_name = "FILE")]
    pub hardware_file: Option<String>,
    /// Serve no models: lend CPU only.
    #[arg(long = "no-llm", global = true)]
    pub no_llm: bool,
    /// Lend this machine's spare CPU for this run (helper.json's cpu.enabled keeps it on).
    #[arg(long, global = true)]
    pub cpu: bool,
    /// Report no power samples.
    #[arg(long = "no-power", global = true)]
    pub no_power: bool,
    /// Updates: install them (auto), only report them (notify), or ignore them (off).
    #[arg(long, global = true, value_parser = ["auto", "notify", "off"])]
    pub updates: Option<String>,
    /// Run as a service: a permanent refusal idles and retries hourly instead of exiting.
    #[arg(long, global = true)]
    pub service: bool,
    /// Run under the console run's supervisor (internal, P4.3.8): an update's hand-off exits 75
    /// and the supervisor starts the new binary.
    #[arg(long, global = true, hide = true)]
    pub supervised: bool,
    /// Trust this update key too (a test build only): a file of base64 public keys, one a line.
    #[cfg(feature = "test-update-key")]
    #[arg(long = "update-key", global = true, value_name = "FILE")]
    pub update_key: Option<PathBuf>,
    /// Log more.
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Print the version, the build, the target, the protocol and the machine key's fingerprint.
    #[arg(short = 'V', long, global = true)]
    pub version: bool,
    /// Check that this machine computes jobs as every other does (the canary and the self-check,
    /// the job host's own), print the build, and exit 0 when it does.
    #[arg(long = "self-test", global = true)]
    pub self_test: bool,
    /// Cheat on purpose (a test build only): result:N, result-at:N,M,..., digest:R, sig, cpu:F,
    /// abandon:N, hold:S. Repeatable.
    #[cfg(feature = "test-faults")]
    #[arg(long = "test-fault", global = true, value_name = "MODE", action = ArgAction::Append)]
    pub test_faults: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("citar-helper").chain(args.iter().copied()))
            .expect("parses")
    }

    #[test]
    fn protocol_one_command_lines_parse_with_their_meaning() {
        let c = parse(&[
            "--server",
            "https://s.example",
            "--token=T",
            "--max-concurrent",
            "2",
            "--model",
            "a",
            "--model",
            "b",
            "--quiet",
            "22:00-06:00",
            "--quiet",
            "Sat 00:00-09:00",
            "--insecure",
            "--no-hardware",
            "-v",
        ]);
        assert_eq!(c.command, None);
        assert_eq!(c.opts.server_url.as_deref(), Some("https://s.example"));
        assert_eq!(c.opts.token.as_deref(), Some("T"));
        assert_eq!(c.opts.max_concurrent, Some(2));
        assert_eq!(c.opts.models, ["a", "b"]);
        assert_eq!(c.opts.quiet, ["22:00-06:00", "Sat 00:00-09:00"]);
        assert!(c.opts.insecure && c.opts.no_hardware && c.opts.verbose);
    }

    #[test]
    fn prefixes_repeats_and_flags_after_the_command_work_as_argparse_did() {
        let c = parse(&["--serve", "x", "--server", "y", "run", "--tok", "t", "--home", "/h"]);
        assert_eq!(c.command, Some(Command::Run));
        assert_eq!(c.opts.server_url.as_deref(), Some("y"));
        assert_eq!(c.opts.token.as_deref(), Some("t"));
        assert_eq!(c.opts.home, Some(PathBuf::from("/h")));
        assert_eq!(parse(&["setup"]).command, Some(Command::Setup));
        assert_eq!(parse(&["scan"]).command, Some(Command::Scan { file: None }));
        assert_eq!(
            parse(&["scan", "hw.json", "--no-llm"]).command,
            Some(Command::Scan { file: Some(PathBuf::from("hw.json")) })
        );
        assert!(parse(&["-V"]).opts.version);
        assert!(parse(&["--self-test"]).opts.self_test);
        assert_eq!(parse(&["job-host"]).command, Some(Command::JobHost));
        assert_eq!(
            parse(&["update", "rollback", "--home", "/h"]).command,
            Some(Command::Update { action: UpdateAction::Rollback })
        );
        assert_eq!(
            parse(&["update", "check"]).command,
            Some(Command::Update { action: UpdateAction::Check })
        );
        assert!(parse(&["run", "--supervised"]).opts.supervised);
        for bad in [
            &["--provider", "vllm"][..],
            &["--max-concurrent", "two"],
            &["--no"],
            &["bogus"],
            &["update"],
            &["update", "now"],
        ] {
            let args = std::iter::once("citar-helper").chain(bad.iter().copied());
            assert!(Cli::try_parse_from(args).is_err(), "{bad:?}");
        }
    }
}
