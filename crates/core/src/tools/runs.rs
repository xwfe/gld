//! 运行记录的兼容导入路径；项目身份仍由应用层计算。

use std::path::Path;

pub(crate) use gld_runtime::runs::{
    create_private_dir, create_private_file, owner_alive, project_has_running, take_owner_lock,
};
pub use gld_runtime::runs::{
    instance_id, running_under, RunLog, RunRecord, RunWriter, MAX_AGE, MAX_FINISHED_RUNS,
    SEGMENT_BYTES,
};

/// 项目 id：和任务数据同一个算法，同一个项目在 `harness/` 和 `runs/` 下是同一个名字。
pub fn project_id(workspace_root: &Path) -> String {
    crate::harness::state::workspace_id(workspace_root)
}
