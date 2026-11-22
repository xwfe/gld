//! 命令会话与输出的兼容导入路径。

pub use gld_runtime::session::{
    kill_session, list_runs, read_output, write_stdin, ExecSession, Found, SessionStore,
};
