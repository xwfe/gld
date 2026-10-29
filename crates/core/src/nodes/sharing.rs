//! 目标机显式共享名单。查看不持运行锁；修改与节点运行使用同一配置锁。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::data::{AppData, DataStore};

use super::config::NodeConfig;
use super::join::{resolve_workspaces, SharedProject};
use super::local_config::{read_config, LocalConfig};

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

/// 完整替换共享名单，运行期间拒绝修改；重新运行节点后向 Hub 重新报告目录。
pub fn replace(path: &Path, selectors: &[String]) -> Result<SharingView, String> {
    let storage = LocalConfig::lock(path)?;
    let mut config = storage.load()?;
    let projects = resolve_workspaces(selectors)?;
    config.workspaces = projects.iter().map(|project| project.id.clone()).collect();
    storage.replace(&config)?;
    Ok(view(
        &config,
        storage.path(),
        projects.into_iter().map(selected).collect(),
    ))
}

fn selected(project: SharedProject) -> SharedWorkspace {
    SharedWorkspace {
        id: project.id,
        name: Some(project.name),
        path: Some(project.path),
        available: true,
        read_only: true,
    }
}

fn catalog(data: &AppData, config: &NodeConfig) -> Vec<SharedWorkspace> {
    config
        .workspaces
        .iter()
        .map(|id| {
            let profile = data.profiles.iter().find(|profile| profile.id == *id);
            SharedWorkspace {
                id: id.clone(),
                name: profile.map(|profile| profile.name.clone()),
                path: profile.map(|profile| profile.path.clone()),
                available: profile.is_some_and(|profile| profile.runtime.confine_reads),
                read_only: true,
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
