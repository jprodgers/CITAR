//! Every frame of protocol 2 as a serde type, with its fields described for the checks
//! (PROTOCOL.md, "Frames, field by field"). Each object is written once, by `object!`, which
//! makes both the struct and its [`Field`] list, so the two cannot drift apart; a closed set of
//! strings is written once by `wire_enum!`.
//!
//! Replaces `citar/worker/protocol.py`'s dataclasses (protocol 1), which 4-05 deletes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::check::{Field, INT, NUM, TEXT, Ty, int, opt, req, string};

/// A closed set of strings on the wire, as a Rust enum, with the list the checks read.
macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $( $(#[$vm:meta])* $variant:ident = $wire:literal ),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum $name {
            $( $(#[$vm])* #[serde(rename = $wire)] $variant, )*
        }

        impl $name {
            /// Every value, as the wire writes it.
            pub const ALL: &'static [&'static str] = &[$($wire),*];

            /// The value as the wire writes it.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $wire, )* }
            }
        }
    };
}

/// An object of the protocol: the struct, and its fields for the checks (`$kind` is `Struct`,
/// whose unknown fields a reader drops, or `Closed`, which refuses them).
macro_rules! object {
    (
        $(#[$m:meta])*
        $kind:ident $name:ident {
            $( $(#[$fm:meta])* $field:ident : $fty:ty = $spec:expr ),* $(,)?
        }
    ) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        $(#[$m])*
        pub struct $name {
            $( $(#[$fm])* pub $field: $fty, )*
        }

        impl $name {
            /// Its fields, as PROTOCOL.md's table gives them.
            pub const FIELDS: &'static [Field] = &[$( $spec.named(stringify!($field)) ),*];
            /// What the checks hold it to.
            pub const TY: Ty = Ty::$kind(Self::FIELDS);
        }
    };
}

// ------------------------------------------------------------------ shared types

const HEX12: Ty = Ty::Hex(12);
const HEX64: Ty = Ty::Hex(64);
const RUNS: Ty = Ty::List { of: &Ty::RunId, min: 0, max: 1024 };
const KIND: Ty = Ty::OneOf(Kind::ALL);
const KINDS: Ty = Ty::List { of: &KIND, min: 0, max: 3 };
const BOT: Ty = string(1, 64);
const BOTS: Ty = Ty::List { of: &BOT, min: 0, max: 64 };
const MODELS: Ty = Ty::List { of: &Model::TY, min: 0, max: 500 };
const SLOTS: Ty = int(0, 16);
const THREADS: Ty = int(0, 1024);
const PERCENT: Ty = int(0, 100);
const MEMORY_MB: Ty = int(0, 16_777_216);
const FRACTION: Ty = Ty::Num { lo: 0.0, hi: 1.0 };
const ANY_STRING: Ty = string(0, usize::MAX);
const SHORT: Ty = string(0, 64);
const REASON: Ty = string(0, 200);

wire_enum! {
    /// A kind of CPU job (P4.4.1).
    Kind {
        LabGame = "lab_game",
        RankingGame = "ranking_game",
        ReferenceGame = "reference_game",
    }
}

wire_enum! {
    /// How hard the OS holds a limit (P4.3.6).
    Hardness { Hard = "hard", Soft = "soft" }
}

wire_enum! {
    /// The kind of model endpoint a helper fronts (P4.3.4).
    Provider {
        LmStudio = "lmstudio",
        Ollama = "ollama",
        OpenAiCompatible = "openai_compatible",
        OpenAi = "openai",
    }
}

object! {
    /// One model an endpoint serves.
    Struct Model {
        key: String = req(string(1, 200)),
        label: String = req(string(0, 200)),
        context: Option<u64> = req(Ty::Nullable(&INT)),
        loaded: bool = req(Ty::Bool),
    }
}

object! {
    /// How hard this OS holds the CPU and the memory limit.
    Struct Limits {
        cpu: Hardness = req(Ty::OneOf(Hardness::ALL)),
        memory: Hardness = req(Ty::OneOf(Hardness::ALL)),
    }
}

object! {
    /// The site's model work for the machine.
    Struct SiteLlm {
        enabled: bool = req(Ty::Bool),
        slots: u32 = req(SLOTS),
        allow_swaps: bool = req(Ty::Bool),
    }
}

object! {
    /// CPU work: what the site asks (in `settings`), or what the helper will do (in
    /// `settings_ack`). A `0` figure from the site means the helper's ceiling.
    Struct SiteCpu {
        enabled: bool = req(Ty::Bool),
        threads: u32 = req(THREADS),
        cpu_percent: u32 = req(PERCENT),
        memory_mb: u64 = req(MEMORY_MB),
        work: Vec<Kind> = req(KINDS),
        yield_to_llm: bool = req(Ty::Bool),
    }
}

object! {
    /// What the site asks of the machine: the `settings` frame, and `welcome.settings`.
    Struct Settings {
        llm: SiteLlm = req(SiteLlm::TY),
        cpu: SiteCpu = req(SiteCpu::TY),
    }
}

object! {
    /// A signed update manifest the server relays: the `update` frame, and `welcome.update`.
    Struct UpdateOffer {
        /// The manifest file's text exactly as published: the signature is over these bytes.
        manifest: String = req(string(1, 65_536)),
        signature: String = req(Ty::Base64(64)),
    }
}

// ------------------------------------------------------------------ the handshake

object! {
    /// The helper's build.
    Struct HelperInfo {
        version: String = req(string(1, 64)),
        build: String = req(HEX12),
        /// `None` for a build without jobs, which is never offered one.
        jobs_build: Option<String> = req(Ty::Nullable(&HEX12)),
        target: String = req(string(1, 64)),
        label: String = req(string(0, 200)),
    }
}

object! {
    /// The machine a helper runs on.
    Struct MachineInfo {
        hostname: String = req(string(0, 255)),
        name: String = req(string(0, 120)),
        os: String = req(string(1, 32)),
        arch: String = req(string(1, 32)),
        public_key: String = req(Ty::Base64(32)),
    }
}

object! {
    /// The helper's model side.
    Struct LlmInfo {
        enabled: bool = req(Ty::Bool),
        provider: Option<Provider> = req(Ty::Nullable(&Ty::OneOf(Provider::ALL))),
        slots: u32 = req(SLOTS),
        stream: bool = req(Ty::Bool),
        models: Vec<Model> = req(MODELS),
    }
}

object! {
    /// The machine's local CPU ceiling (local always wins, plan C3).
    Struct Ceiling {
        threads: u32 = req(THREADS),
        cpu_percent: u32 = req(PERCENT),
        memory_mb: u64 = req(MEMORY_MB),
        work: Vec<Kind> = req(KINDS),
    }
}

object! {
    /// The helper's CPU side.
    Struct CpuInfo {
        /// The machine's own switch: off unless its owner turned it on there.
        enabled: bool = req(Ty::Bool),
        ceiling: Ceiling = req(Ceiling::TY),
        limits: Limits = req(Limits::TY),
        verifiable: bool = req(Ty::Bool),
        bots: Vec<String> = req(BOTS),
        ruleset: String = req(HEX64),
        /// The memory each run may use on its job host, in MB, as the host last said in its
        /// `ready` (4-10); left out while no host has said.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory_per_run_mb: Option<u64> = opt(MEMORY_MB),
    }
}

wire_enum! {
    /// Where a hello's hardware report came from.
    HelloHardwareSource { Scan = "scan", File = "file", Nothing = "none" }
}

object! {
    /// The hardware report a hello carries.
    Struct Report {
        hardware: Option<Map<String, Value>> = req(Ty::Nullable(&Ty::Object)),
        hardware_source: HelloHardwareSource = req(Ty::OneOf(HelloHardwareSource::ALL)),
    }
}

wire_enum! {
    /// The helper's local update policy.
    UpdatePolicy { Auto = "auto", Notify = "notify", Off = "off" }
}

object! {
    /// The helper's first frame.
    Struct Hello {
        protocol: u32 = req(Ty::Exactly(2)),
        /// The worker token; never logged.
        token: String = req(string(1, 512)),
        helper: HelperInfo = req(HelperInfo::TY),
        machine: MachineInfo = req(MachineInfo::TY),
        llm: LlmInfo = req(LlmInfo::TY),
        cpu: CpuInfo = req(CpuInfo::TY),
        report: Report = req(Report::TY),
        running: Vec<String> = req(RUNS),
        results: Vec<String> = req(RUNS),
        updates: UpdatePolicy = req(Ty::OneOf(UpdatePolicy::ALL)),
        quiet_until: Option<f64> = req(Ty::Nullable(&NUM)),
    }
}

object! {
    /// The machine row a token belongs to.
    Struct WelcomeMachine {
        id: String = req(Ty::Id),
        name: String = req(string(0, 120)),
    }
}

object! {
    /// The server's build.
    Struct ServerInfo {
        version: String = req(string(1, 64)),
        build: String = req(HEX12),
        jobs_build: Option<String> = req(Ty::Nullable(&HEX12)),
    }
}

object! {
    /// Which of the hello's runs to keep and drop, and which held results to send again.
    Struct WelcomeJobs {
        keep: Vec<String> = req(RUNS),
        drop: Vec<String> = req(RUNS),
        resend: Vec<String> = req(RUNS),
    }
}

object! {
    /// The helper is accepted.
    Struct Welcome {
        protocol: u32 = req(Ty::Exactly(2)),
        machine: WelcomeMachine = req(WelcomeMachine::TY),
        settings: Settings = req(Settings::TY),
        ping_s: u32 = req(int(5, 300)),
        server: ServerInfo = req(ServerInfo::TY),
        update: Option<UpdateOffer> = req(Ty::Nullable(&UpdateOffer::TY)),
        jobs: WelcomeJobs = req(WelcomeJobs::TY),
    }
}

wire_enum! {
    /// Why a hello was refused.
    RefusedCode {
        Token = "token",
        Key = "key",
        Protocol = "protocol",
        Rate = "rate",
        /// Reserved for protocol 3.
        Build = "build",
    }
}

object! {
    /// The helper is not accepted; the server closes the socket after it.
    Struct Refused {
        code: RefusedCode = req(Ty::OneOf(RefusedCode::ALL)),
        message: String = req(TEXT),
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_s: Option<f64> = opt(NUM),
    }
}

object! {
    /// The v1 refusal: protocol 1's `error` frame, the one shape a 0.1.5 helper reads, which a
    /// 0.1.6 server sends to any hello that is not protocol 2 and a 0.1.5 server sends to a
    /// protocol 2 hello. The helper reads it as `refused` `protocol`.
    Struct V1Refusal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String> = opt(SHORT),
        message: String = req(TEXT),
        /// Protocol 1's, sent by a 0.1.5 server; ignored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refusal: Option<String> = opt(SHORT),
        /// Protocol 1's, sent by a 0.1.5 server; ignored.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retryable: Option<bool> = opt(Ty::Bool),
    }
}

// ------------------------------------------------------------------ settings, catalog, hardware, power, status

object! {
    /// The model side in effect.
    Struct EffectiveLlm {
        enabled: bool = req(Ty::Bool),
        slots: u32 = req(SLOTS),
    }
}

object! {
    /// What the helper will actually do.
    Struct Effective {
        llm: EffectiveLlm = req(EffectiveLlm::TY),
        cpu: SiteCpu = req(SiteCpu::TY),
    }
}

wire_enum! {
    /// A figure the helper's ceiling capped, in this order.
    Capped {
        LlmEnabled = "llm.enabled",
        LlmSlots = "llm.slots",
        CpuEnabled = "cpu.enabled",
        CpuThreads = "cpu.threads",
        CpuPercent = "cpu.cpu_percent",
        CpuMemory = "cpu.memory_mb",
        CpuWork = "cpu.work",
    }
}

const CAPPED: Ty = Ty::OneOf(Capped::ALL);

object! {
    /// The acknowledgement of a `settings`.
    Struct SettingsAck {
        effective: Effective = req(Effective::TY),
        capped: Vec<Capped> = req(Ty::List { of: &CAPPED, min: 0, max: 7 }),
        limits: Limits = req(Limits::TY),
        /// The memory each run may use on the job host the helper runs for these settings, in MB
        /// (its `ready`'s, 4-10): what the dispatcher matches a job's `mem_mb` to. Left out while
        /// no host runs for them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        memory_per_run_mb: Option<u64> = opt(MEMORY_MB),
        /// True on the last acknowledgement sent again with the job host's figures, which came
        /// after it (the host was ready late, or started again under new limits once its runs
        /// ended): it answers no `settings` frame. Left out on every other.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        again: Option<bool> = opt(Ty::Bool),
    }
}

object! {
    /// The endpoint's models changed.
    Struct Catalog {
        models: Vec<Model> = req(MODELS),
    }
}

wire_enum! {
    /// Where a `hardware` frame's scan came from.
    HardwareSource { Scan = "scan", File = "file" }
}

object! {
    /// The hardware scan changed, or a rescan was asked for.
    Struct Hardware {
        scan: Map<String, Value> = req(Ty::Object),
        source: HardwareSource = req(Ty::OneOf(HardwareSource::ALL)),
    }
}

object! {
    /// Scan the hardware now.
    Struct Rescan {}
}

object! {
    /// One power sample.
    Struct Sample {
        t: f64 = req(NUM),
        /// Every GPU's draw together, in watts; `None` where it cannot be read.
        gpu_w: Option<f64> = req(Ty::Nullable(&NUM)),
        /// The CPU's utilisation, a fraction.
        cpu_util: Option<f64> = req(Ty::Nullable(&FRACTION)),
    }
}

object! {
    /// A minute of power samples.
    Struct Power {
        samples: Vec<Sample> = req(Ty::List { of: &Sample::TY, min: 1, max: 360 }),
    }
}

object! {
    /// The helper's quiet hours, requests and runs.
    Struct Status {
        quiet_until: Option<f64> = req(Ty::Nullable(&NUM)),
        llm_in_flight: u32 = req(int(0, 64)),
        jobs_running: u32 = req(THREADS),
        jobs_paused: u32 = req(THREADS),
    }
}

// ------------------------------------------------------------------ completions

const MESSAGE_FIELDS: &[Field] = &[req(string(1, 32)).named("role")];
const MESSAGE: Ty = Ty::Open(MESSAGE_FIELDS);
const TOOLS: Ty = Ty::List { of: &Ty::Object, min: 0, max: 512 };

/// The sampling parameters of a request: these five, each optional; other fields are kept in the
/// frame and the helper drops them (logged once per name).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Params {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Anything else the server sent.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Params {
    /// The five the helper passes on.
    pub const FIELDS: &'static [Field] = &[
        opt(Ty::Num { lo: 0.0, hi: 2.0 }).named("temperature"),
        opt(int(1, 1_048_576)).named("max_tokens"),
        opt(FRACTION).named("top_p"),
        opt(INT).named("seed"),
        opt(string(1, 32)).named("reasoning_effort"),
    ];
    /// What the checks hold it to.
    pub const TY: Ty = Ty::Open(Self::FIELDS);
}

object! {
    /// One completion.
    Struct Request {
        id: String = req(Ty::Id),
        model: String = req(string(1, 200)),
        /// The whole OpenAI-format message list, each kept whole.
        messages: Vec<Map<String, Value>> = req(Ty::List { of: &MESSAGE, min: 1, max: 10_000 }),
        /// OpenAI function definitions, or `None` (in `json` tool mode).
        tools: Option<Vec<Map<String, Value>>> = req(Ty::Nullable(&TOOLS)),
        params: Params = req(Params::TY),
        timeout_s: f64 = req(Ty::Num { lo: 1.0, hi: 86_400.0 }),
    }
}

object! {
    /// A tool call's function, raw.
    Struct FunctionCall {
        name: String = req(string(0, 200)),
        /// The arguments' text as the endpoint gave it, even when it is not valid JSON.
        arguments: String = req(ANY_STRING),
    }
}

object! {
    /// A tool call, raw.
    Struct ToolCall {
        id: String = req(string(0, 200)),
        #[serde(rename = "type")]
        kind: String = req(string(0, 32)).wire("type"),
        function: FunctionCall = req(FunctionCall::TY),
    }
}

object! {
    /// The endpoint's message, raw.
    Struct DoneMessage {
        content: Option<String> = req(Ty::Nullable(&ANY_STRING)),
        reasoning: Option<String> = req(Ty::Nullable(&ANY_STRING)),
        tool_calls: Vec<ToolCall> = req(Ty::List { of: &ToolCall::TY, min: 0, max: 512 }),
    }
}

object! {
    /// The endpoint's token counts.
    Struct Usage {
        prompt_tokens: u64 = req(INT),
        completion_tokens: u64 = req(INT),
        reasoning_tokens: Option<u64> = req(Ty::Nullable(&INT)),
        cached_tokens: Option<u64> = req(Ty::Nullable(&INT)),
    }
}

const FINISH: Ty = string(0, 64);

object! {
    /// The completion, as the endpoint returned it.
    Struct Done {
        id: String = req(Ty::Id),
        message: DoneMessage = req(DoneMessage::TY),
        finish_reason: Option<String> = req(Ty::Nullable(&FINISH)),
        usage: Option<Usage> = req(Ty::Nullable(&Usage::TY)),
        gen_s: f64 = req(NUM),
        ttft_s: Option<f64> = req(Ty::Nullable(&NUM)),
        streamed: bool = req(Ty::Bool),
    }
}

wire_enum! {
    /// Why a request did not complete.
    RequestErrorCode {
        Busy = "busy",
        Closed = "closed",
        UnknownModel = "unknown_model",
        Stopping = "stopping",
        Endpoint = "endpoint",
        Timeout = "timeout",
        BadRequest = "bad_request",
    }
}

object! {
    /// The completion failed or was refused.
    Struct RequestError {
        id: String = req(Ty::Id),
        code: RequestErrorCode = req(Ty::OneOf(RequestErrorCode::ALL)),
        message: String = req(TEXT),
        retryable: bool = req(Ty::Bool),
    }
}

object! {
    /// Stop a completion.
    Struct Cancel {
        id: String = req(Ty::Id),
    }
}

object! {
    /// The completion is stopped.
    Struct Cancelled {
        id: String = req(Ty::Id),
        gen_s: f64 = req(NUM),
        tokens_out: u64 = req(INT),
    }
}

// ------------------------------------------------------------------ jobs

object! {
    /// A job run offered.
    Struct JobOffer {
        job: String = req(Ty::JobId),
        run: String = req(Ty::RunId),
        kind: Kind = req(KIND),
        /// The typed spec: an object here, checked by `citar-jobs`.
        spec: Map<String, Value> = req(Ty::Object),
        spec_hash: String = req(HEX64),
        jobs_build: String = req(HEX12),
        deadline_s: f64 = req(Ty::Num { lo: 1.0, hi: 86_400.0 }),
        mem_mb: u64 = req(MEMORY_MB),
        priority: i32 = req(int(-100, 100)),
        offer_ttl_s: f64 = req(Ty::Num { lo: 1.0, hi: 3_600.0 }),
    }
}

object! {
    /// A frame naming only a run: `job_accept`, `job_result_ack`, `job_aborted`.
    Struct RunRef {
        run: String = req(Ty::RunId),
    }
}

wire_enum! {
    /// Why a run will not start.
    RejectReason {
        Busy = "busy",
        Build = "build",
        Kind = "kind",
        Quiet = "quiet",
        Disabled = "disabled",
        Limits = "limits",
        Memory = "memory",
        BadSpec = "bad_spec",
        Unverifiable = "unverifiable",
    }
}

object! {
    /// The run will not start.
    Struct JobReject {
        run: String = req(Ty::RunId),
        reason: RejectReason = req(Ty::OneOf(RejectReason::ALL)),
    }
}

object! {
    /// The run's progress, and its heartbeat.
    Struct JobProgress {
        run: String = req(Ty::RunId),
        round: u64 = req(INT),
        of: u64 = req(INT),
        elapsed_s: f64 = req(NUM),
        cpu_s: f64 = req(NUM),
        paused_s: f64 = req(NUM),
        peak_mb: f64 = req(NUM),
        /// Whether the run is held by a pause now (`yield_to_llm`, `jobs_pause`; 4-10): the server
        /// counts a held run's pause on from this report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        paused: Option<bool> = opt(Ty::Bool),
    }
}

object! {
    /// The state-hash chain of a run: one digest per round.
    Struct Chain {
        head: String = req(HEX64),
        rounds: u64 = req(INT),
        digests: Vec<String> = req(Ty::List { of: &HEX64, min: 0, max: 10_000 }),
    }
}

object! {
    /// What a run produced.
    Struct Outcome {
        /// The typed result, or a crash record: an object here, checked by `citar-jobs`.
        result: Map<String, Value> = req(Ty::Object),
        result_hash: String = req(HEX64),
        chain: Chain = req(Chain::TY),
        cpu_s: f64 = req(NUM),
        wall_s: f64 = req(NUM),
        paused_s: f64 = req(NUM),
        peak_mb: f64 = req(NUM),
    }
}

object! {
    /// What the helper signs (P4.4.4). Closed: its bytes are signed, so a field the reader does
    /// not know is refused rather than dropped.
    #[serde(deny_unknown_fields)]
    Closed Claim {
        v: u32 = req(Ty::Exactly(1)),
        job: String = req(Ty::JobId),
        run: String = req(Ty::RunId),
        machine: String = req(Ty::Id),
        kind: Kind = req(KIND),
        spec_hash: String = req(HEX64),
        jobs_build: String = req(HEX12),
        bots: Vec<String> = req(BOTS),
        result_hash: String = req(HEX64),
        chain_head: String = req(HEX64),
        rounds: u64 = req(INT),
        key: String = req(Ty::Base64(32)),
    }
}

object! {
    /// The run's outcome and its signed claim.
    Struct JobResult {
        run: String = req(Ty::RunId),
        outcome: Outcome = req(Outcome::TY),
        claim: Claim = req(Claim::TY),
        signature: String = req(Ty::Base64(64)),
    }
}

wire_enum! {
    /// Why a run failed (the infrastructure, never the game).
    FailCode {
        Memory = "memory",
        Deadline = "deadline",
        Host = "host",
        AbortedLocally = "aborted_locally",
        Internal = "internal",
    }
}

object! {
    /// The run failed.
    Struct JobFailed {
        run: String = req(Ty::RunId),
        code: FailCode = req(Ty::OneOf(FailCode::ALL)),
        message: String = req(TEXT),
        retryable: bool = req(Ty::Bool),
    }
}

object! {
    /// Stop a run.
    Struct JobAbort {
        run: String = req(Ty::RunId),
        reason: String = req(REASON),
    }
}

object! {
    /// Pause every running game.
    Struct JobsPause {
        reason: String = req(REASON),
    }
}

object! {
    /// Resume them.
    Struct JobsResume {}
}

// ------------------------------------------------------------------ updates, keep-alive, shutdown

wire_enum! {
    /// Where a helper is with an update.
    UpdateState {
        Available = "available",
        Downloading = "downloading",
        Verified = "verified",
        Restarting = "restarting",
        Applied = "applied",
        Failed = "failed",
        Refused = "refused",
        Unavailable = "unavailable",
    }
}

object! {
    /// Where the helper is with an update.
    Struct UpdateStatus {
        state: UpdateState = req(Ty::OneOf(UpdateState::ALL)),
        version: Option<String> = req(Ty::Nullable(&SHORT)),
        message: String = req(TEXT),
    }
}

object! {
    /// Keep-alive, both ways.
    Struct Ping {}
}

object! {
    /// The answer to a `ping`, both ways.
    Struct Pong {}
}

wire_enum! {
    /// Why the machine was taken away.
    ShutdownCode {
        Revoked = "revoked",
        Disabled = "disabled",
        Deleted = "deleted",
        Suspended = "suspended",
        Replaced = "replaced",
    }
}

object! {
    /// The machine was taken away: a permanent refusal.
    Struct Shutdown {
        reason: String = req(TEXT),
        code: ShutdownCode = req(Ty::OneOf(ShutdownCode::ALL)),
    }
}

// ------------------------------------------------------------------ the two directions

/// The frames one side sends, as an enum, with the dispatch the parser and the encoder use.
macro_rules! frames {
    (
        $(#[$m:meta])*
        $name:ident from $sender:ident { $( $(#[$vm:meta])* $variant:ident ( $ty:ty ) = $t:literal ),* $(,)? }
    ) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq)]
        pub enum $name {
            $( $(#[$vm])* $variant($ty), )*
        }

        impl super::FrameSet for $name {
            const SENDER: super::Sender = super::Sender::$sender;
            const TYPES: &'static [&'static str] = &[$($t),*];

            fn ty_of(t: &str) -> Option<Ty> {
                match t {
                    $( $t => Some(<$ty>::TY), )*
                    _ => None,
                }
            }

            fn read(t: &str, v: Value) -> Option<Result<Self, serde_json::Error>> {
                match t {
                    $( $t => Some(serde_json::from_value(v).map(Self::$variant)), )*
                    _ => None,
                }
            }

            fn type_name(&self) -> &'static str {
                match self {
                    $( Self::$variant(_) => $t, )*
                }
            }

            fn fields(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $( Self::$variant(f) => serde_json::to_value(f), )*
                }
            }
        }
    };
}

frames! {
    /// What the helper sends.
    HelperFrame from Helper {
        Hello(Hello) = "hello",
        SettingsAck(SettingsAck) = "settings_ack",
        Catalog(Catalog) = "catalog",
        Hardware(Hardware) = "hardware",
        Power(Power) = "power",
        Status(Status) = "status",
        Done(Done) = "done",
        Error(RequestError) = "error",
        Cancelled(Cancelled) = "cancelled",
        JobAccept(RunRef) = "job_accept",
        JobReject(JobReject) = "job_reject",
        JobProgress(JobProgress) = "job_progress",
        JobResult(JobResult) = "job_result",
        JobFailed(JobFailed) = "job_failed",
        JobAborted(RunRef) = "job_aborted",
        UpdateStatus(UpdateStatus) = "update_status",
        Ping(Ping) = "ping",
        Pong(Pong) = "pong",
    }
}

frames! {
    /// What the server sends.
    ServerFrame from Server {
        Welcome(Welcome) = "welcome",
        Refused(Refused) = "refused",
        /// The v1 refusal (protocol 1's `error`), read as `refused` `protocol`.
        V1Refusal(V1Refusal) = "error",
        Settings(Settings) = "settings",
        Rescan(Rescan) = "rescan",
        Request(Request) = "request",
        Cancel(Cancel) = "cancel",
        JobOffer(JobOffer) = "job_offer",
        JobResultAck(RunRef) = "job_result_ack",
        JobAbort(JobAbort) = "job_abort",
        JobsPause(JobsPause) = "jobs_pause",
        JobsResume(JobsResume) = "jobs_resume",
        Update(UpdateOffer) = "update",
        Ping(Ping) = "ping",
        Pong(Pong) = "pong",
        Shutdown(Shutdown) = "shutdown",
    }
}
