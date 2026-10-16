//! The helper's job hosts: its own binary as a child, `citar-helper job-host` (design/phase4-helper.md
//! P4.3.6, P4.4.3), started by citar-jobs' parent side ([`citar_jobs::host::spawn`]: on Linux
//! under a `systemd --user` scope where the user's manager makes one) under the effective limits,
//! its `ready` held to this binary's jobs build.
//!
//! The parent side is blocking (pipes and a thread per stream), so each host gets a thread of its
//! own that owns it (`bridge`): it passes the helper's frames to the host and the host's to the
//! helper's job side ([`crate::jobs`]) over channels, and when the host's output ends it classifies
//! the death and fails its runs (`memory` or `host`, with each run's last reported peak). Dropping
//! the job side's handle ends the host: its standard input closes, it ends its runs and exits
//! within two seconds, and is killed past three.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use citar_jobs::host::parent::{Event, Host, Program, SpawnError, Wrapper, spawn};
use citar_jobs::host::{FromHost, ToHost};
use citar_jobs::limits::Requested;
use tokio::sync::mpsc;

use crate::jobs::{HostEvent, HostStart, Hosts, StartFailure};
use crate::llm::BoxFuture;
use crate::log::{Log, clip};

/// How long a host may take to be ready: its limits, the canary and the self-check.
pub const READY_WAIT: Duration = Duration::from_secs(60);
/// How long the bridge waits for the host's next frame before it looks for the helper's.
const POLL: Duration = Duration::from_millis(20);
/// How long a host that died has to exit before it is ended, while its death is classified.
const DEATH_WAIT: Duration = Duration::from_secs(5);

/// Job hosts as children of this process: `program job-host`.
pub struct ChildHosts {
    pub program: PathBuf,
    pub log: Log,
}

impl ChildHosts {
    /// This binary's own job hosts.
    ///
    /// # Errors
    /// The binary's own path cannot be read.
    pub fn this_binary(log: Log) -> Result<ChildHosts, String> {
        let program =
            std::env::current_exe().map_err(|e| format!("this program's own path: {e}"))?;
        Ok(ChildHosts { program, log })
    }
}

/// The wrapper this machine starts hosts under, found once (a trial scope takes a moment).
fn wrapper() -> Wrapper {
    static FOUND: OnceLock<Wrapper> = OnceLock::new();
    *FOUND.get_or_init(Wrapper::detect)
}

impl Hosts for ChildHosts {
    fn self_test(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async {
            let rules = citar_engine::rules::Ruleset::shared();
            let done = tokio::task::spawn_blocking(move || {
                citar_jobs::host::self_test(rules, citar_jobs::canary::Inject::default())
            })
            .await;
            match done {
                Ok(r) => r.map(|_| ()),
                Err(e) => Err(format!("the self-test's thread: {e}")),
            }
        })
    }

    fn start(
        &self,
        limits: Requested,
        jobs_build: String,
    ) -> BoxFuture<'static, Result<HostStart, StartFailure>> {
        let program = Program {
            program: self.program.clone(),
            args: vec![OsString::from("job-host")],
            env: Vec::new(),
        };
        let log = self.log.clone();
        Box::pin(async move {
            let started = tokio::task::spawn_blocking(move || {
                spawn(&program, wrapper(), &limits, &jobs_build, READY_WAIT)
            })
            .await;
            let host = match started {
                Ok(Ok(h)) => h,
                Ok(Err(e)) => {
                    let limits = e.is_limits();
                    return Err(StartFailure { limits, message: describe(&e) });
                }
                Err(e) => {
                    return Err(StartFailure {
                        limits: false,
                        message: format!("the job host's start: {e}"),
                    });
                }
            };
            let ready = host.ready().clone();
            log.debug(format!("job host started: pid {}", host.pid()));
            let (cmds_tx, cmds_rx) = mpsc::unbounded_channel();
            let (events_tx, events_rx) = mpsc::unbounded_channel();
            let thread = std::thread::Builder::new()
                .name("citar-job-host".to_owned())
                .spawn(move || bridge(host, cmds_rx, &events_tx, &log));
            if let Err(e) = thread {
                return Err(StartFailure {
                    limits: false,
                    message: format!("no thread for the job host: {e}"),
                });
            }
            Ok(HostStart { ready, cmds: cmds_tx, events: events_rx })
        })
    }
}

fn describe(e: &SpawnError) -> String {
    clip(&e.to_string(), 600)
}

/// The host's thread: the helper's frames to it, its frames to the helper, until it ends or the
/// helper lets it go.
fn bridge(
    mut host: Host,
    mut cmds: mpsc::UnboundedReceiver<ToHost>,
    events: &mpsc::UnboundedSender<HostEvent>,
    log: &Log,
) {
    loop {
        loop {
            match cmds.try_recv() {
                Ok(cmd) => {
                    // A host that is gone shows below, as its end.
                    let _sent = host.send(&cmd);
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // The job side let it go: it ends its runs and exits.
                    for line in host.take_log() {
                        log.debug(format!("job host: {}", clip(&line, 400)));
                    }
                    let _exit = host.close();
                    return;
                }
            }
        }
        match host.next(POLL) {
            Some(Event::Frame(f)) => {
                if let FromHost::Heartbeat(_) = f {
                    continue;
                }
                if events.send(HostEvent::Frame(f)).is_err() {
                    let _exit = host.close();
                    return;
                }
            }
            Some(Event::Garbled(line)) => {
                log.debug(format!("job host said: {}", clip(&line, 300)));
            }
            Some(Event::Gone) => {
                let peaks: Vec<(String, Option<f64>)> =
                    host.in_flight().iter().map(|r| (r.clone(), host.peak_mb(r))).collect();
                let death = host.death(DEATH_WAIT);
                for line in host.take_log() {
                    log.debug(format!("job host: {}", clip(&line, 400)));
                }
                let failures = host
                    .failures(&death)
                    .into_iter()
                    .filter_map(|f| match f {
                        FromHost::JobFailed(f) => {
                            let peak =
                                peaks.iter().find(|(r, _)| *r == f.run).and_then(|(_, p)| *p);
                            Some((f, peak))
                        }
                        _ => None,
                    })
                    .collect();
                let _told = events.send(HostEvent::Died { death: death.to_string(), failures });
                return;
            }
            None => {
                for line in host.take_log() {
                    log.debug(format!("job host: {}", clip(&line, 400)));
                }
            }
        }
    }
}
