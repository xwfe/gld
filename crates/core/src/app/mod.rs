//! 应用服务层：所有“对状态做点什么”的入口。
//!
//! 守护进程和命令行都只通过 [`App`] 操作工作区、密钥、设置与服务生命周期，
//! 这样两边的行为完全一致：命令行直连时和经守护进程转发时跑的是同一段代码。
//!
//! 每个子模块对应一组用例：
//!
//! | 模块 | 用例 |
//! | --- | --- |
//! | [`workspace`] | 工作区增删改查、按 id / 名称 / 路径 / 当前目录定位 |
//! | [`runtime`] | 启动 / 停止 / 重启 MCP 与 Actions，恢复上次运行状态 |
//! | [`tunnel`] | 隧道启停、测试、frpc 配置片段 |
//! | [`gateway`] | 全局共享公网入口 |
//! | [`secrets`] | 工作区 / 共享密钥，变更后自动重启相关服务 |
//! | [`settings`] | 代理、下载镜像、FRP 服务器配置、全局运行时设置 |
//! | [`logs`] | 读取工作区日志尾部 |
//! | [`planning`] | Goal / Plan / 模式切换与人工验收 |
//! | [`inspect`] | 健康检查、历史会话、Token 用量、Agent 上下文扫描 |
//! | [`software`] | frpc / cloudflared 的安装与卸载 |
//! | [`tools`] | 直接调用工具内核（命令行验证 AI 侧行为） |
//! | [`doctor`] | 配置体检：把“哪里配错了”变成一条条可执行的修复建议 |

mod doctor;
mod gateway;
mod inspect;
mod logs;
mod planning;
mod runtime;
mod secrets;
mod settings;
mod state;
mod tools;
mod tunnel;
mod workspace;
mod workspace_fields;

pub use doctor::{
    config_checks, port_check, software_check, Diagnosis, DoctorCheck, DoctorLevel, PortOccupant,
    SecretLookup,
};
pub use logs::LogChunk;
pub use planning::PlanStepUpdate;
pub use runtime::{RunningService, ServiceOverview};
pub use secrets::{reads_from_shared_pool, SHARED_SECRET_KEYS, WORKSPACE_SECRET_KEYS};
pub use settings::{FrpProfileDto, GlobalRuntimeSettingsDto};
pub use state::App;
pub use tunnel::TunnelTestResult;
pub use workspace::{RestartFailure, WorkspaceCreateOptions, WorkspaceTarget, WorkspaceUpdate};
pub use workspace_fields::{actions_field_suffixes, workspace_field_catalog, WorkspaceFieldDoc};
