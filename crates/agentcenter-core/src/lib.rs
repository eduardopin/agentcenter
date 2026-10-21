//! agentcenter-core: amostrador `/proc`, modelo de sessao, adapters e contrato
//! de overlay. Sem `sysinfo` nem `procfs` (ADR-001 #4).

pub mod actions;
pub mod adapters;
pub mod cockpit;
pub mod control;
pub mod fixture;
pub mod history;
pub mod machine;
pub mod overlay;
pub mod procfs;
pub mod reclaim;
pub mod sampler;
pub mod session;
pub mod snapshot;
pub mod summary;
pub mod tree;

pub use actions::{Action, Guard, Plan, Target};
pub use adapters::Registry;
pub use cockpit::{
    Cockpit, Contas, Family, Handoff, HandoffIndex, Reason, Recommendation, Thresholds,
};
pub use history::{HISTORY_LEN, History, Ring, SessionHistory};
pub use machine::{LoadAvg, Psi, PsiResource};
pub use procfs::ProcRoot;
pub use reclaim::{Candidate, CpuWindow, IdleBy, Reclaim};
pub use sampler::{Frame, READ_ALL, Sampler, SamplerConfig, SamplerHandle};
pub use session::{ProcessMetrics, Session};
pub use snapshot::{IoStallSource, Machine, Snapshot};
pub use summary::{CockpitCounts, SessionState, StatusCounts, Summary};
