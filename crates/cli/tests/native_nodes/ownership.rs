//! 稳定宿主保留机器登记；执行代切换和服务重启不能放开尚未接通的授权边界。

use super::*;

#[test]
fn machine_identity_and_authorization_refusal_survive_generation_and_hub_restart() {
    let hub = Env::new();
    let service = hub.serve("local", "bearer");
    let gateway_port = free_port();
    let gateway_relay = TlsRelay::new(gateway_port);
    hub.ok(&[
        "gateway",
        "set",
        "--enabled",
        "true",
        "--tunnel",
        "off",
        "--public-url",
        &format!("https://localhost:{}", gateway_relay.port),
        "--port",
        &gateway_port.to_string(),
    ]);
    hub.ok(&["gateway", "start"]);
    hub.ok(&["hub", "set", "--global-gateway", "true"]);
    let relay = TlsRelay::new(service.port);
    let machine = Env::new();
    let ids = projects(&machine, "staying");
    let registered = register(&hub, "staying", TOKEN_A);
    let gateway_config = config(&machine, &gateway_relay, &registered, TOKEN_A, &ids);
    let gateway_path = write_config(&machine, "gateway.json", &gateway_config);
    let mut direct_config = gateway_config;
    direct_config["hub_url"] = json!(format!("https://localhost:{}", relay.port));
    let direct_path = write_config(&machine, "direct.json", &direct_config);
    let saved = std::fs::read(&direct_path).unwrap();
    let machines = stored_machines(&hub);
    let check_boundary = || {
        for path in [&direct_path, &gateway_path] {
            assert_node_auth_pending(&NodeProcess::start(&machine, path).rejected());
        }
        assert_no_native_projects(&hub);
        assert_eq!(stored_machines(&hub), machines);
        assert_eq!(std::fs::read(&direct_path).unwrap(), saved);
        assert_auth_not_ready(&service.rpc("tools/list", json!({})));
    };
    check_boundary();
    let old = hub.json(&["--json", "daemon", "upgrade", "status"]);
    let old_pid = old["generations"][0]["pid"].as_u64().unwrap() as u32;
    hub.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    let switched = hub.json(&["--json", "daemon", "upgrade", "switch"]);
    assert_ne!(switched["current"], old["current"]);
    wait_until("the previous execution generation did not exit", || {
        !gld_core::platform::platform().is_process_alive(old_pid)
    });
    let current = hub.json(&["--json", "daemon", "upgrade", "status"]);
    assert_eq!(current["host"]["pid"], old["host"]["pid"]);
    check_boundary();

    hub.ok(&["hub", "stop"]);
    assert!(std::net::TcpStream::connect(("127.0.0.1", service.port)).is_err());
    hub.ok(&["hub", "start"]);
    assert_eq!(
        hub.json(&["--json", "daemon", "upgrade", "status"])["host"]["pid"],
        old["host"]["pid"]
    );
    check_boundary();
    assert_no_daemon(&machine);
}
