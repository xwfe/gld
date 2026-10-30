//! 一次确认后配对。请求 ID 与节点凭据先私有落盘，网络失败时重用同一请求。

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;

use crate::data::{AppData, DataStore};
use crate::tools::registry::exposed_tool_names;

use super::config::{validate_workspace_ids, NodeConfig, RegisteredMachine};
use super::local_config::{validate_config_location, LocalConfig};
use super::pairing::{invitation_identity, PairRequest};

#[cfg(test)]
#[path = "join_tests.rs"]
mod tests;

const MAX_RESPONSE_BYTES: usize = 16 * 1024;

pub struct JoinOptions {
    pub hub_url: String,
    pub config_path: PathBuf,
    pub workspaces: Vec<String>,
    pub writable_workspaces: Vec<String>,
    pub runnable_workspaces: Vec<String>,
    pub ca_certificate: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SharedProject {
    pub id: String,
    pub name: String,
    pub path: String,
    pub read_only: bool,
    pub commands: bool,
}

#[derive(Debug, Serialize)]
pub struct JoinPreview {
    pub hub_url: String,
    pub hub_id: String,
    pub execution_account: String,
    pub workspaces: Vec<SharedProject>,
    pub access: &'static str,
    pub desktop: bool,
    pub resuming: bool,
    pub config_path: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct JoinedNode {
    pub hub_id: String,
    pub machine_id: String,
    pub name: String,
    pub config_path: PathBuf,
    pub workspaces: Vec<SharedProject>,
}

pub struct PreparedJoin {
    storage: LocalConfig,
    config: NodeConfig,
    invitation: String,
    client: reqwest::Client,
    endpoint: reqwest::Url,
    preview: JoinPreview,
}

impl PreparedJoin {
    pub fn prepare(options: JoinOptions, invitation: String) -> Result<Self, String> {
        let projects = resolve_workspaces(&options.workspaces)?;
        Self::prepare_with_projects(options, invitation, projects)
    }

    fn prepare_with_projects(
        options: JoinOptions,
        invitation: String,
        mut projects: Vec<SharedProject>,
    ) -> Result<Self, String> {
        let (hub_id, hub_public_key) = invitation_identity(&invitation)?;
        let workspaces: Vec<String> = projects.iter().map(|project| project.id.clone()).collect();
        validate_workspace_ids(&workspaces)?;
        let (writable_workspaces, runnable_workspaces) = select_access(
            &mut projects,
            &options.writable_workspaces,
            &options.runnable_workspaces,
        )?;
        validate_config_location(
            &options.config_path,
            &projects
                .iter()
                .map(|project| project.path.clone())
                .collect::<Vec<_>>(),
        )?;
        let ca_certificate = options
            .ca_certificate
            .map(|path| {
                path.canonicalize()
                    .map_err(|_| "Cannot read the selected node CA certificate".to_string())
            })
            .transpose()?;
        let mut config = NodeConfig {
            hub_url: options.hub_url,
            hub_id,
            hub_public_key,
            machine_id: uuid::Uuid::new_v4().simple().to_string(),
            credential: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            workspaces,
            writable_workspaces,
            runnable_workspaces,
            ca_certificate,
        };
        config.validate()?;
        // 规范化语法等价的基地址，使尾斜线不产生另一份配对请求。
        let mut base = reqwest::Url::parse(&config.hub_url).map_err(|_| "Invalid node Hub URL")?;
        base.path_segments_mut()
            .map_err(|_| "Invalid node Hub URL")?
            .pop_if_empty();
        config.hub_url = base.to_string();
        let storage = LocalConfig::lock(&options.config_path)?;
        if storage.exists()? {
            return Err(
                "Node configuration already exists; use node run or a different configuration path"
                    .into(),
            );
        }
        let pending = storage.load_pending()?;
        let resuming = pending.is_some();
        if let Some(pending) = pending {
            if pending.hub_url != config.hub_url
                || pending.hub_id != config.hub_id
                || pending.hub_public_key != config.hub_public_key
                || pending.workspaces != config.workspaces
                || pending.writable_workspaces != config.writable_workspaces
                || pending.runnable_workspaces != config.runnable_workspaces
                || pending.ca_certificate != config.ca_certificate
            {
                return Err("Pending pairing has different Hub or sharing settings; retry with the original settings".into());
            }
            config = pending;
        }
        let endpoint = pairing_url(&config)?;
        let client = pairing_client(&config)?;
        let preview = JoinPreview {
            hub_url: config.hub_url.clone(),
            hub_id: config.hub_id.clone(),
            execution_account: execution_account(),
            workspaces: projects,
            access: if !config.runnable_workspaces.is_empty() {
                "project-commands"
            } else if !config.writable_workspaces.is_empty() {
                "project-file-write"
            } else {
                "read-only"
            },
            desktop: false,
            resuming,
            config_path: storage.path().to_path_buf(),
        };
        Ok(Self {
            storage,
            config,
            invitation,
            client,
            endpoint,
            preview,
        })
    }

    pub fn preview(&self) -> &JoinPreview {
        &self.preview
    }

    /// 调用方在显示 preview 并取得一次确认之后调用。
    pub async fn execute(self) -> Result<JoinedNode, String> {
        self.storage.save_pending(&self.config)?;
        let machine = request_pairing(
            &self.client,
            self.endpoint.clone(),
            &self.invitation,
            &self.config,
        )
        .await?;
        self.storage.finish(&self.config)?;
        Ok(JoinedNode {
            hub_id: machine.hub_id,
            machine_id: machine.machine_id,
            name: machine.name,
            config_path: self.storage.path().to_path_buf(),
            workspaces: self.preview.workspaces,
        })
    }
}

pub fn resolve_workspaces(selectors: &[String]) -> Result<Vec<SharedProject>, String> {
    DataStore::read_file(|data| Ok(resolve_in(data, selectors)))
        .map_err(|_| "Cannot read local project registrations".to_string())?
}

/// 从刚解析的本机项目收窄权限，顺序按共享名单规范化；省略时全部只读。
/// `--run` 自带文件写入：能跑命令就能改文件，再要求写一遍 `--write` 只是多一步。
pub(super) fn select_access(
    projects: &mut [SharedProject],
    write_selectors: &[String],
    run_selectors: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut writable = pick_projects(projects, write_selectors, "writable")?;
    let runnable = pick_projects(projects, run_selectors, "command")?;
    if projects
        .iter()
        .any(|p| writable.contains(&p.id) && p.read_only)
    {
        return Err(
            "A writable project must use a local tool profile that supports file writes".into(),
        );
    }
    if projects
        .iter()
        .any(|p| runnable.contains(&p.id) && (p.read_only || !p.commands))
    {
        return Err(
            "A command project must use a local tool profile that supports commands".into(),
        );
    }
    writable.extend(runnable.iter().cloned());
    for project in projects.iter_mut() {
        project.read_only = !writable.contains(&project.id);
        project.commands = runnable.contains(&project.id);
    }
    let ids = |keep: fn(&SharedProject) -> bool| {
        projects
            .iter()
            .filter(|project| keep(project))
            .map(|project| project.id.clone())
            .collect::<Vec<_>>()
    };
    Ok((ids(|p| !p.read_only), ids(|p| p.commands)))
}

fn pick_projects(
    projects: &[SharedProject],
    selectors: &[String],
    label: &str,
) -> Result<HashSet<String>, String> {
    let mut picked = HashSet::new();
    for selector in selectors {
        let by_id = projects.iter().any(|project| project.id == *selector);
        let candidates: Vec<_> = projects
            .iter()
            .filter(|project| {
                if by_id {
                    project.id == *selector
                } else {
                    project.name == *selector
                }
            })
            .collect();
        let [project] = candidates.as_slice() else {
            return Err(format!(
                "Every {label} project must be selected in --share by full ID or unique name"
            ));
        };
        if !picked.insert(project.id.clone()) {
            return Err(format!("A {label} project was selected more than once"));
        }
    }
    Ok(picked)
}

fn resolve_in(data: &AppData, selectors: &[String]) -> Result<Vec<SharedProject>, String> {
    if selectors.is_empty() || selectors.len() > 256 {
        return Err("Select 1..=256 local projects by full ID or unique name".into());
    }
    let mut projects = Vec::with_capacity(selectors.len());
    let mut selected = HashSet::new();
    for selector in selectors {
        let by_id = data.profiles.iter().any(|profile| profile.id == *selector);
        let candidates: Vec<_> = data
            .profiles
            .iter()
            .filter(|profile| {
                if by_id {
                    profile.id == *selector
                } else {
                    profile.name == *selector
                }
            })
            .collect();
        let [profile] = candidates.as_slice() else {
            return Err("A selected local project is missing or ambiguous; use its full ID".into());
        };
        if !profile.runtime.confine_reads {
            return Err("Every shared local project must enable confine-reads".into());
        }
        if !selected.insert(&profile.id) {
            return Err("A local project was selected more than once".into());
        }
        let tools = exposed_tool_names(&profile.runtime.tool_profile);
        projects.push(SharedProject {
            id: profile.id.clone(),
            name: profile.name.clone(),
            path: profile.path.clone(),
            read_only: !tools.contains(&"apply_patch"),
            commands: tools.contains(&"exec_command"),
        });
    }
    validate_workspace_ids(
        &projects
            .iter()
            .map(|project| project.id.clone())
            .collect::<Vec<_>>(),
    )?;
    Ok(projects)
}

fn pairing_url(config: &NodeConfig) -> Result<reqwest::Url, String> {
    let mut endpoint =
        reqwest::Url::parse(&config.websocket_url()?).map_err(|_| "Invalid node pairing URL")?;
    let scheme = if endpoint.scheme() == "wss" {
        "https"
    } else {
        "http"
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|_| "Invalid node pairing URL")?;
    endpoint
        .path_segments_mut()
        .map_err(|_| "Invalid node pairing URL")?
        .pop()
        .pop()
        .push("pair");
    Ok(endpoint)
}

fn pairing_client(config: &NodeConfig) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none());
    let base = reqwest::Url::parse(&config.hub_url).map_err(|_| "Invalid node Hub URL")?;
    if base
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|ip| ip.is_loopback())
    {
        // HTTP 例外只用于本机；不能让环境变量里的代理把邀请和节点凭据带离回环地址。
        builder = builder.no_proxy();
    }
    if let Some(path) = &config.ca_certificate {
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Err("Node CA certificate must be a regular DER file".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| "Cannot read the configured node CA certificate")?
            .take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read the configured node CA certificate")?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 {
            return Err("Node CA certificate must be a DER certificate of at most 64 KiB".into());
        }
        let certificate =
            reqwest::Certificate::from_der(&bytes).map_err(|_| "Invalid node CA certificate")?;
        builder = builder
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate);
    }
    builder
        .build()
        .map_err(|_| "Cannot configure the node pairing HTTPS client".into())
}

async fn request_pairing(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    invitation: &str,
    config: &NodeConfig,
) -> Result<RegisteredMachine, String> {
    let request = PairRequest {
        request_id: config.machine_id.clone(),
        credential: config.credential.clone(),
    };
    let mut response = client
        .post(endpoint)
        .bearer_auth(invitation)
        .json(&request)
        .send()
        .await
        .map_err(|_| {
            "Node pairing connection failed; retry with the same configuration path and settings"
                .to_string()
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "Node pairing was rejected (HTTP {}); pending state was kept",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("Node pairing response exceeded its size limit; pending state was kept".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        "Node pairing response was interrupted; retry with the same configuration path and settings"
    })? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(
                "Node pairing response exceeded its size limit; pending state was kept".into(),
            );
        }
        bytes.extend_from_slice(&chunk);
    }
    let machine: RegisteredMachine = serde_json::from_slice(&bytes)
        .map_err(|_| "Invalid node pairing response; pending state was kept")?;
    if machine.hub_id != config.hub_id
        || machine.hub_public_key != config.hub_public_key
        || machine.machine_id != config.machine_id
        || machine.revoked
        || machine.name.trim().is_empty()
        || machine.name.len() > 128
        || machine.name.contains(['/', '\\'])
        || machine.name.chars().any(char::is_control)
    {
        return Err(
            "Node pairing response did not match this Hub and request; pending state was kept"
                .into(),
        );
    }
    Ok(machine)
}

fn execution_account() -> String {
    #[cfg(unix)]
    {
        format!("uid:{}", unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME").unwrap_or_else(|_| "current process account".into())
    }
}
