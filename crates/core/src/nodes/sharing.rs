//! 目标机显式共享名单。查看不持运行锁；修改与节点运行使用同一配置锁。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::data::{AppData, DataStore};
use crate::tools::registry::exposed_tool_names;

use super::config::NodeConfig;
use super::join::{resolve_workspaces, select_access, SharedProject};
use super::local_config::{read_config, validate_config_location, LocalConfig};

#[cfg(test)]
#[path = "sharing_tests.rs"]
mod tests;

#[derive(Debug, Serialize)]
pub struct SharedWorkspace {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub available: bool,
    pub read_only: bool,
    pub commands: bool,
}

#[derive(Debug, Serialize)]
pub struct SharingView {
    pub hub_url: String,
    pub hub_id: String,
    pub machine_id: String,
    pub config_path: PathBuf,
    pub workspaces: Vec<SharedWorkspace>,
}

/// 已停止或运行中的配置都可查看，输出只包含身份与共享项目。
pub fn show(path: &Path) -> Result<SharingView, String> {
    let config = read_config(path)?;
    let path = path
        .canonicalize()
        .map_err(|_| "Cannot resolve node configuration path")?;
    DataStore::read_file(|data| Ok(view(&config, &path, catalog(data, &config))))
        .map_err(|_| "Cannot read local project registrations".into())
}

/// 完整替换共享、文件写入和跑命令名单，运行期间拒绝修改；重新运行节点后向 Hub 重新报告目录。
pub fn replace(
    path: &Path,
    selectors: &[String],
    writable_selectors: &[String],
    runnable_selectors: &[String],
) -> Result<SharingView, String> {
    let storage = LocalConfig::lock(path)?;
    let mut config = storage.load()?;
    let mut projects = resolve_workspaces(selectors)?;
    validate_config_location(
        storage.path(),
        &projects
            .iter()
            .map(|project| project.path.clone())
            .collect::<Vec<_>>(),
    )?;
    (config.writable_workspaces, config.runnable_workspaces) =
        select_access(&mut projects, writable_selectors, runnable_selectors)?;
    config.workspaces = projects.iter().map(|project| project.id.clone()).collect();
    storage.replace(&config)?;
    Ok(view(
        &config,
        storage.path(),
        projects.into_iter().map(selected).collect(),
    ))
}

#[derive(Debug, Serialize)]
pub struct LeftNode {
    pub hub_id: String,
    pub machine_id: String,
    pub config_path: PathBuf,
}

/// 目标机单方面退出：删掉本机配置和凭据。节点要先停（同一把配置锁）。不碰项目文件和运行记录；
/// Hub 上的登记要操作员撤销，节点凭据只能用来连 WSS，不能调 Hub 的管理操作。
pub fn leave(path: &Path) -> Result<LeftNode, String> {
    let storage = LocalConfig::lock(path)?;
    let config = storage.remove()?;
    Ok(LeftNode {
        hub_id: config.hub_id,
        machine_id: config.machine_id,
        config_path: storage.path().to_path_buf(),
    })
}

fn selected(project: SharedProject) -> SharedWorkspace {
    SharedWorkspace {
        id: project.id,
        name: Some(project.name),
        path: Some(project.path),
        available: true,
        read_only: project.read_only,
        commands: project.commands,
    }
}

fn catalog(data: &AppData, config: &NodeConfig) -> Vec<SharedWorkspace> {
    config
        .workspaces
        .iter()
        .map(|id| {
            let profile = data.profiles.iter().find(|profile| profile.id == *id);
            // 和节点运行时同一个口径：项目关了 confine-reads 或工具档收紧了，就算名单里有也不算。
            let allows = |tool: &str| {
                profile.is_some_and(|profile| {
                    profile.runtime.confine_reads
                        && exposed_tool_names(&profile.runtime.tool_profile).contains(&tool)
                })
            };
            let read_only = !config.writable_workspaces.contains(id) || !allows("apply_patch");
            SharedWorkspace {
                id: id.clone(),
                name: profile.map(|profile| profile.name.clone()),
                path: profile.map(|profile| profile.path.clone()),
                available: profile.is_some_and(|profile| profile.runtime.confine_reads),
                read_only,
                commands: !read_only
                    && config.runnable_workspaces.contains(id)
                    && allows("exec_command"),
            }
        })
        .collect()
}

fn view(config: &NodeConfig, path: &Path, workspaces: Vec<SharedWorkspace>) -> SharingView {
    SharingView {
        hub_url: config.hub_url.clone(),
        hub_id: config.hub_id.clone(),
        machine_id: config.machine_id.clone(),
        config_path: path.to_path_buf(),
        workspaces,
    }
}
