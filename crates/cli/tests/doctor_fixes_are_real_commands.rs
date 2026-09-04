//! `gld doctor` 给出的每条修复命令都必须是真命令。
//!
//! 体检的价值全在 fix 那一行：如果它让人去跑一个不存在的子命令或已经改名的
//! 参数，比不给建议更糟。这里把 core 里所有 fix 文本抽出来，逐条丢给 clap 校验。

mod common;

use std::collections::BTreeSet;

use gld_core::app::{config_checks, port_check, software_check, PortOccupant};
use gld_core::runtime::ServiceKind;
use gld_core::settings::{AppSettings, FrpProfile};
use gld_core::workspace::WorkspaceProfile;

/// 造出尽可能多种坏配置，把所有 fix 分支都跑出来。
fn all_fix_texts() -> BTreeSet<String> {
    let temp = tempfile::tempdir().expect("workspace");
    let path = temp.path().to_str().unwrap().to_string();

    let mut profiles = Vec::new();

    // 认证缺密钥（工作区级与共享池各一份）。
    for (name, shared, auth) in [
        ("oauth-broken", false, "oauth"),
        ("oauth-shared", true, "oauth"),
        ("bearer-broken", false, "bearer"),
        ("noauth-lan", false, "noauth"),
    ] {
        let mut profile = WorkspaceProfile::new(path.clone(), Some(name.into()));
        profile.id = format!("id-{name}");
        profile.auth.auth_type = auth.into();
        profile.auth.use_shared_secrets = shared;
        profile.tunnel.tunnel_type = "none".into();
        profile.actions.tunnel_type = "none".into();
        profiles.push(profile);
    }

    // 隧道各种残缺形态。
    let tunnel_cases: [(&str, &str, &str, &str, bool); 5] = [
        ("frp-no-profile", "frp", "", "", false),
        ("frp-unknown-profile", "frp", "missing-id", "sub", false),
        ("frp-no-subdomain", "frp", "known", "", false),
        ("cf-named-no-token", "cloudflare", "", "", false),
        ("gateway-off", "none", "", "", true),
    ];
    for (name, tunnel, profile_id, subdomain, gateway) in tunnel_cases {
        let mut profile = WorkspaceProfile::new(path.clone(), Some(name.into()));
        profile.id = format!("id-{name}");
        profile.auth.auth_type = "bearer".into();
        profile.tunnel.tunnel_type = tunnel.into();
        profile.tunnel.frp_profile_id = profile_id.into();
        profile.tunnel.frp_subdomain = subdomain.into();
        profile.tunnel.cloudflare_mode = "named".into();
        profile.tunnel.use_global_gateway = gateway;
        profile.actions.tunnel_type = "none".into();
        profiles.push(profile);
    }

    // 一个工作区两条线路指向不同 FRP 配置。
    let mut split = WorkspaceProfile::new(path.clone(), Some("split".into()));
    split.id = "id-split".into();
    split.auth.auth_type = "bearer".into();
    split.tunnel.tunnel_type = "frp".into();
    split.tunnel.frp_profile_id = "known".into();
    split.tunnel.frp_subdomain = "a".into();
    split.actions.tunnel_type = "frp".into();
    split.actions.frp_profile_id = "other".into();
    split.actions.frp_subdomain = "b".into();
    profiles.push(split);

    // 端口与子域名冲突。
    let mut clash_a = WorkspaceProfile::new(path.clone(), Some("clash-a".into()));
    clash_a.id = "id-clash-a".into();
    clash_a.auth.auth_type = "bearer".into();
    clash_a.tunnel.tunnel_type = "frp".into();
    clash_a.tunnel.frp_profile_id = "known".into();
    clash_a.tunnel.frp_subdomain = "dup".into();
    clash_a.actions.tunnel_type = "none".into();
    clash_a.runtime.local_port = 40001;
    clash_a.actions.local_port = 40002;
    let mut clash_b = clash_a.clone();
    clash_b.id = "id-clash-b".into();
    clash_b.name = "clash-b".into();
    profiles.push(clash_a);
    profiles.push(clash_b);

    // 目录不存在。
    let mut gone = WorkspaceProfile::new("/definitely/not/here/gld".into(), Some("gone".into()));
    gone.id = "id-gone".into();
    gone.auth.auth_type = "bearer".into();
    gone.tunnel.tunnel_type = "none".into();
    gone.actions.tunnel_type = "none".into();
    profiles.push(gone);

    let mut settings = AppSettings {
        allow_lan_access: true,
        ..AppSettings::default()
    };
    settings.frp_profiles.push(FrpProfile {
        id: "known".into(),
        name: "known".into(),
        server: "frp.example.com".into(),
        server_port: 7000,
    });

    let mut checks = config_checks(&profiles, &settings, &|_, _, _| false);

    // 端口检查的每种组合（判定已抽成纯函数，这里穷举）。
    let occupied = Some(PortOccupant {
        is_self: false,
        image: "/usr/bin/other".into(),
    });
    let own = Some(PortOccupant {
        is_self: true,
        image: "/usr/bin/gld".into(),
    });
    for kind in [ServiceKind::Mcp, ServiceKind::Actions] {
        for (running, occupant) in [
            (true, None),
            (true, occupied.clone()),
            (false, occupied.clone()),
            (false, own.clone()),
            (false, None),
        ] {
            checks.push(port_check("ws", kind, 28766, running, occupant));
        }
    }

    // 外部二进制：装了 / 没装。
    for kind in ["frpc", "cloudflared"] {
        checks.push(software_check(kind, None));
        checks.push(software_check(kind, Some("/usr/local/bin/x")));
    }

    let fixes: BTreeSet<String> = checks
        .into_iter()
        .filter(|check| !check.fix.trim().is_empty())
        .map(|check| check.fix)
        .collect();
    assert!(
        fixes.len() >= 12,
        "坏配置没覆盖到足够多的 fix 分支：{fixes:#?}"
    );
    fixes
}

#[test]
fn every_doctor_fix_names_a_command_that_exists() {
    let mut checked = 0usize;
    for fix in all_fix_texts() {
        for command in common::docs::extract_anywhere(&fix, "doctor fix") {
            common::docs::assert_valid(&command);
            checked += 1;
        }
    }
    assert!(checked >= 10, "只从 fix 文本里抽出了 {checked} 条命令");
}
