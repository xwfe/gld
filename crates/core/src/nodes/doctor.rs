//! `gld node doctor`：目标机上一条命令回答“这台机器为什么连不上”。
//! 每个失败都说清原因和怎么修。输出不含凭据和代理密码，发给别人看没关系。

use std::path::Path;
use std::time::Duration;

use tokio::net::TcpStream;

use crate::app::{Diagnosis, DoctorCheck, DoctorLevel};

use super::autostart;
use super::client::{probe, HubProbe};
use super::config::NodeConfig;
use super::local_config::{read_config, LocalConfig};
use super::proxy;
use super::service::log_path;
use super::sharing;

#[cfg(test)]
#[path = "doctor_tests.rs"]
mod tests;

const SCOPE: &str = "节点";
const TCP_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_LINES: usize = 5;

fn check(label: &str, level: DoctorLevel, detail: impl Into<String>, fix: &str) -> DoctorCheck {
    DoctorCheck {
        scope: SCOPE.into(),
        label: label.into(),
        level,
        detail: detail.into(),
        fix: fix.into(),
    }
}

fn ok(label: &str, detail: impl Into<String>) -> DoctorCheck {
    check(label, DoctorLevel::Ok, detail, "")
}

fn fail(label: &str, detail: impl Into<String>, fix: &str) -> DoctorCheck {
    check(label, DoctorLevel::Fail, detail, fix)
}

fn warn(label: &str, detail: impl Into<String>, fix: &str) -> DoctorCheck {
    check(label, DoctorLevel::Warn, detail, fix)
}

pub async fn diagnose(path: &Path) -> Diagnosis {
    let mut checks = vec![ok("版本", env!("CARGO_PKG_VERSION"))];
    let config = match read_config(path) {
        Ok(config) => {
            checks.push(ok("配置", path.display().to_string()));
            config
        }
        Err(error) => {
            checks.push(fail(
                "配置",
                format!("{}：{error}", path.display()),
                "要加入 Hub：在 Hub 上 gld node invite <名称>，按它给的命令在这里执行。",
            ));
            return Diagnosis { checks };
        }
    };
    checks.extend(project_checks(path));
    let running = local_checks(path, &mut checks);
    checks.extend(network_checks(&config, running).await);
    if let Some(log) = recent_log(path) {
        checks.push(ok("最近日志", log));
    }
    Diagnosis { checks }
}

fn project_checks(path: &Path) -> Vec<DoctorCheck> {
    let view = match sharing::show(path) {
        Ok(view) => view,
        Err(error) => return vec![fail("共享项目", error, "")],
    };
    view.workspaces
        .into_iter()
        .map(|workspace| {
            let name = workspace.name.unwrap_or(workspace.id);
            if !workspace.available {
                return fail(
                    "共享项目",
                    format!("{name} 没登记了，或关了读取限制"),
                    "节点起不来。重新登记（gld add <目录>），或从名单里去掉：gld node shares --share <要留的项目>",
                );
            }
            let access = if workspace.commands {
                "文件写入、跑命令"
            } else if workspace.read_only {
                "只读"
            } else {
                "文件写入"
            };
            ok(
                "共享项目",
                format!(
                    "{name}（{}）{access}",
                    workspace.path.unwrap_or_default()
                ),
            )
        })
        .collect()
}

/// 返回节点进程在不在跑。
fn local_checks(path: &Path, checks: &mut Vec<DoctorCheck>) -> bool {
    let running = LocalConfig::in_use(path).unwrap_or(false);
    match autostart::status(path) {
        Ok(status) if status.installed && running => {
            checks.push(ok("节点进程", format!("由 {} 管着，在跑", status.manager)));
            for note in &status.notes {
                checks.push(warn("自启动", note.clone(), ""));
            }
        }
        Ok(status) if status.installed => checks.push(fail(
            "节点进程",
            format!("配了 {} 自启动，但现在没在跑", status.manager),
            "看下面的最近日志；修好后重启：gld node autostart enable",
        )),
        Ok(_) if running => checks.push(warn(
            "节点进程",
            "在前台跑；关终端或重启后就没了",
            "要一直在线：结束它，再 gld node autostart enable",
        )),
        Ok(_) => checks.push(warn(
            "节点进程",
            "没在跑，Hub 上这台机器显示离线",
            "gld node autostart enable（一直在线）或 gld node run（前台试一下）",
        )),
        Err(error) => checks.push(warn("节点进程", error, "")),
    }
    running
}

async fn network_checks(config: &NodeConfig, running: bool) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    let Ok(url) = reqwest::Url::parse(&config.hub_url) else {
        checks.push(fail(
            "Hub 地址",
            config.hub_url.clone(),
            "配置里的地址坏了，重新加入。",
        ));
        return checks;
    };
    checks.push(ok("Hub 地址", config.hub_url.clone()));
    let proxy = match proxy::for_hub(&url) {
        Ok(Some(proxy)) => {
            checks.push(ok("代理", format!("经 {}", proxy.address())));
            Some(proxy)
        }
        Ok(None) => {
            checks.push(ok("代理", "直连"));
            None
        }
        Err(error) => {
            checks.push(fail(
                "代理",
                error,
                "节点只支持 HTTP CONNECT 代理。把 HTTPS_PROXY 改成 http://host:port；配了自启动的改完重跑 gld node autostart enable。",
            ));
            return checks;
        }
    };
    if proxy.is_none() {
        if let Err(failed) = reachable(&url).await {
            checks.push(failed);
            return checks;
        }
    }
    let outcome = probe(config).await;
    checks.push(probe_check(outcome, running));
    checks
}

/// 只用来把“域名解析不了”和“连不上”分开；真正的判断在后面的握手。
async fn reachable(url: &reqwest::Url) -> Result<(), DoctorCheck> {
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = match tokio::net::lookup_host((host, port)).await {
        Ok(addresses) => addresses.collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    if addresses.is_empty() {
        return Err(fail(
            "网络",
            format!("解析不了 {host}"),
            "看域名有没有写错、DNS 通不通。只能经代理出网的机器要设 HTTPS_PROXY。",
        ));
    }
    match tokio::time::timeout(TCP_TIMEOUT, TcpStream::connect(addresses.as_slice())).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => Err(fail(
            "网络",
            format!("{host}:{port} 拒绝连接"),
            "看 Hub 和入口（隧道、反代）在不在跑。",
        )),
        Err(_) => Err(fail(
            "网络",
            format!("{host}:{port} {} 秒没响应", TCP_TIMEOUT.as_secs()),
            "防火墙可能拦了出站。只能经代理出网的机器要设 HTTPS_PROXY。",
        )),
    }
}

fn probe_check(outcome: HubProbe, running: bool) -> DoctorCheck {
    const LABEL: &str = "Hub 握手";
    const REJOIN: &str = "在 Hub 上 gld node invite <名称>，这里 gld leave 后按新命令重新加入。";
    match outcome {
        HubProbe::Connected => ok(LABEL, "成功：Hub 身份和节点凭据都对"),
        HubProbe::Busy if running => ok(LABEL, "这台机器已在线，连接正被本机节点占着"),
        HubProbe::Busy => warn(
            LABEL,
            "Hub 认为这台机器已在线，但本机没有节点在跑",
            "刚断的旧连接最多一分钟后释放。一直这样就是别处在用同一份配置，先停掉那边。",
        ),
        HubProbe::Rejected(401) => fail(
            LABEL,
            "节点凭据被撤销或换过（HTTP 401）",
            "在 Hub 上 gld node invite <名称>，这里 gld leave 后按新命令重新加入。Hub 上没撤销过的话，看入口的反代是不是加了自己的认证。",
        ),
        HubProbe::Rejected(404) => fail(
            LABEL,
            "入口通了，但没有节点路由（HTTP 404）",
            "反代要同时转发 /nodes/v1/ 下的 HTTP 和 WebSocket，不能只放行 /mcp；Hub 地址的路径前缀也要写对。",
        ),
        HubProbe::Rejected(status @ (502 | 503 | 504 | 530)) => fail(
            LABEL,
            format!("入口在，但后面的 Hub 没响应（HTTP {status}）"),
            "在 Hub 上 gld status 看服务在不在跑。节点会自己重连，修好了不用动这边。",
        ),
        HubProbe::Rejected(status) => fail(
            LABEL,
            format!("被拒绝（HTTP {status}）"),
            "入口可能有访问规则挡了节点路由，或 Hub 版本太旧。升级 Hub，并检查反代和 CDN 的规则。",
        ),
        HubProbe::WrongHub => fail(LABEL, "这个地址背后不是配对时的 Hub", REJOIN),
        HubProbe::Tls => fail(
            LABEL,
            "TLS 校验失败：证书不受信或域名对不上",
            "公共 Wi-Fi 的认证页面也会这样，先确认能正常上网。公司网络解密 HTTPS 或 Hub 用自签证书时，导出根证书（DER 格式），gld leave 后用 gld join --ca-certificate <文件> 重新加入。",
        ),
        HubProbe::Proxy(message) => fail(
            LABEL,
            message,
            "看代理地址和账号对不对，代理是否放行 Hub 的域名和端口。",
        ),
        HubProbe::Timeout => fail(
            LABEL,
            "10 秒没握手完",
            "网络太慢，或者中间设备拦了 WebSocket。反代要支持 HTTP/1.1 Upgrade。",
        ),
        HubProbe::Failed(message) => fail(LABEL, message, ""),
    }
}

fn recent_log(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log_path(path)).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_LINES)..].join("\n      ");
    (!tail.is_empty()).then(|| format!("{}\n      {tail}", log_path(path).display()))
}
