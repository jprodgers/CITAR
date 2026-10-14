//! The acknowledgement of the site's settings (design/phase4-helper.md P4.2.3, PROTOCOL.md
//! "Settings"): what the helper will actually do, each figure the smaller of the site's and the
//! local ceiling, a site `0` meaning the ceiling, and `capped` naming where the ceiling won.
//! Local always wins (plan C3), and the effective figures are never above the site's.

use crate::config::Ceiling;
use crate::protocol::{
    Capped, Effective, EffectiveLlm, Hardness, Kind, Limits, Settings, SettingsAck, SiteCpu,
};

/// What the machine itself allows: its model side and its CPU ceiling.
#[derive(Clone, Debug, PartialEq)]
pub struct Local {
    /// The helper serves models (it has an endpoint, `--no-llm` not given, and its proxy works).
    pub serves_llm: bool,
    /// Its own concurrent requests: the configured `max_concurrent`, 0 with `--no-llm`.
    pub slots: u32,
    /// The machine's own CPU switch.
    pub cpu_enabled: bool,
    pub ceiling: Ceiling,
    /// How hard this OS holds each limit.
    pub limits: Limits,
}

/// The limits a helper reports until its job host says what holds (4-08): soft, on every OS, so
/// that nothing is ever claimed hard before it is.
pub const UNTIL_THE_JOB_HOST: Limits = Limits { cpu: Hardness::Soft, memory: Hardness::Soft };

/// A site figure against a local ceiling: the site's `0` is the ceiling; otherwise the smaller,
/// `capped` when the ceiling is.
fn figure(site: u64, ceiling: u64, name: Capped, capped: &mut Vec<Capped>) -> u64 {
    if site == 0 {
        ceiling
    } else if ceiling < site {
        capped.push(name);
        ceiling
    } else {
        site
    }
}

fn narrow(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The acknowledgement of `site` by a machine allowing `local`.
#[must_use]
pub fn ack(site: &Settings, local: &Local) -> SettingsAck {
    let mut capped = Vec::new();
    let llm_enabled = site.llm.enabled && local.serves_llm;
    if site.llm.enabled && !local.serves_llm {
        capped.push(Capped::LlmEnabled);
    }
    let slots =
        figure(u64::from(site.llm.slots), u64::from(local.slots), Capped::LlmSlots, &mut capped);
    let cpu_enabled = site.cpu.enabled && local.cpu_enabled;
    if site.cpu.enabled && !local.cpu_enabled {
        capped.push(Capped::CpuEnabled);
    }
    let c = &local.ceiling;
    let threads =
        figure(u64::from(site.cpu.threads), u64::from(c.threads), Capped::CpuThreads, &mut capped);
    let percent = figure(
        u64::from(site.cpu.cpu_percent),
        u64::from(c.cpu_percent),
        Capped::CpuPercent,
        &mut capped,
    );
    let memory = figure(site.cpu.memory_mb, c.memory_mb, Capped::CpuMemory, &mut capped);
    let both: Vec<Kind> = [Kind::LabGame, Kind::RankingGame, Kind::ReferenceGame]
        .into_iter()
        .filter(|k| site.cpu.work.contains(k) && c.work.contains(k))
        .collect();
    if site.cpu.work.iter().any(|k| !c.work.contains(k)) {
        capped.push(Capped::CpuWork);
    }
    capped.sort();
    SettingsAck {
        effective: Effective {
            llm: EffectiveLlm { enabled: llm_enabled, slots: narrow(slots) },
            cpu: SiteCpu {
                enabled: cpu_enabled,
                threads: narrow(threads),
                cpu_percent: narrow(percent),
                memory_mb: memory,
                work: both,
                yield_to_llm: site.cpu.yield_to_llm,
            },
        },
        capped,
        limits: local.limits.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SiteLlm;

    fn site(llm: (bool, u32), cpu: (bool, u32, u32, u64, &[Kind])) -> Settings {
        Settings {
            llm: SiteLlm { enabled: llm.0, slots: llm.1, allow_swaps: false },
            cpu: SiteCpu {
                enabled: cpu.0,
                threads: cpu.1,
                cpu_percent: cpu.2,
                memory_mb: cpu.3,
                work: cpu.4.to_vec(),
                yield_to_llm: true,
            },
        }
    }

    fn local(serves: bool, slots: u32, cpu: bool) -> Local {
        Local {
            serves_llm: serves,
            slots,
            cpu_enabled: cpu,
            ceiling: Ceiling {
                threads: 4,
                cpu_percent: 50,
                memory_mb: 2048,
                work: vec![Kind::LabGame, Kind::ReferenceGame],
            },
            limits: UNTIL_THE_JOB_HOST,
        }
    }

    #[test]
    fn the_settings_capped_transcript_by_hand() {
        let s = site((false, 4), (true, 8, 25, 0, &[Kind::LabGame, Kind::RankingGame]));
        let a = ack(&s, &local(false, 2, true));
        assert_eq!(a.effective.llm, EffectiveLlm { enabled: false, slots: 2 });
        let c = &a.effective.cpu;
        assert_eq!((c.enabled, c.threads, c.cpu_percent, c.memory_mb), (true, 4, 25, 2048));
        assert_eq!(c.work, [Kind::LabGame]);
        assert_eq!(a.capped, [Capped::LlmSlots, Capped::CpuThreads, Capped::CpuWork]);
    }

    #[test]
    fn switches_off_on_the_machine_are_capped_and_never_claimed() {
        let s = site((true, 4), (true, 8, 25, 0, &[Kind::LabGame]));
        let a = ack(&s, &local(false, 0, false));
        assert!(!a.effective.llm.enabled && !a.effective.cpu.enabled);
        assert_eq!(a.effective.llm.slots, 0);
        assert_eq!(
            a.capped,
            [Capped::LlmEnabled, Capped::LlmSlots, Capped::CpuEnabled, Capped::CpuThreads]
        );
        let off = site((false, 1), (false, 1, 10, 512, &[Kind::LabGame]));
        assert!(ack(&off, &local(true, 1, true)).capped.is_empty());
    }
}
