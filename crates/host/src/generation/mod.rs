//! 宿主对执行代的选路、替换和回收。

mod link;
pub mod router;

pub use router::Router;

/// 同时活着的代数上限（含准备好还没切过去的）。到了就拒绝再准备新的，绝不为了腾位置杀旧代。
pub const MAX_LIVE_GENERATIONS: usize = 3;
