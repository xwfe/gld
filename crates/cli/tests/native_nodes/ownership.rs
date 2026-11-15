use super::*;

#[test]
fn native_node_connection_is_host_owned_across_switch_and_hub_restart() {
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
    let mut node = NodeProcess::start(&machine, &direct_path);
    let api = find(&wait_inventory(&service, 2, 2), "staying/api");
    assert_eq!(read(&service, &api)["content"], "staying/api\n");
    assert_eq!(relay.connections(), 1);

    let upgrade_status = || hub.json(&["--json", "daemon", "upgrade", "status"]);
    let old = upgrade_status();
    let old_pid = old["generations"][0]["pid"].as_u64().unwrap() as u32;
    hub.ok(&["daemon", "upgrade", "prepare", env!("CARGO_BIN_EXE_gld")]);
    let switched = hub.json(&["--json", "daemon", "upgrade", "switch"]);
    assert_ne!(switched["current"], old["current"]);
    wait_until("the previous execution generation did not exit", || {
        !gld_core::platform::platform().is_process_alive(old_pid)
    });
    let current = upgrade_status();
    assert_eq!(current["current"], switched["current"]);
    assert_eq!(current["host"]["pid"], old["host"]["pid"]);
    assert_eq!(read(&service, &api)["content"], "staying/api\n");
    assert_eq!(
        relay.connections(),
        1,
        "switching must leave the socket open"
    );

    hub.ok(&["hub", "stop"]);
    wait_until("stopping the Hub did not close the original socket", || {
        relay.active.load(Ordering::SeqCst) == 0
    });
    wait_until("the same node did not try to reconnect", || {
        relay.connections() > 1
    });
    assert!(node.0.as_mut().unwrap().try_wait().unwrap().is_none());
    machine.write("api/README.md", "after Hub restart\n");
    hub.ok(&["hub", "start"]);
    let restored = wait_inventory(&service, 2, 2);
    assert_eq!(find(&restored, "staying/api"), api);
    assert_eq!(read(&service, &api)["content"], "after Hub restart\n");
    assert_eq!(upgrade_status()["host"]["pid"], old["host"]["pid"]);
    assert_eq!(relay.active.load(Ordering::SeqCst), 1);
    assert_eq!(relay.peak.load(Ordering::SeqCst), 1);

    // 独立配置绕过本机运行锁，409 必须来自与直连接入共用的节点登记表。
    let duplicate = NodeProcess::start(&machine, &gateway_path).rejected();
    assert!(duplicate.contains("HTTP 409"), "{duplicate}");
    assert_eq!(read(&service, &api)["content"], "after Hub restart\n");
}
