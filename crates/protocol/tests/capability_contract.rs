//! D19-A1 的脱敏样例：只固定契约形状，不代表任何已发布能力。

use gld_protocol::capabilities::{
    check_snapshot, CapabilityId, CapabilitySnapshot, CAPABILITY_CONTRACT_VERSION,
};

#[test]
fn provisional_snapshot_sample_round_trips_and_passes_freshness() {
    let raw = include_str!("fixtures/capability_snapshot.sample.json");
    let parsed: CapabilitySnapshot = serde_json::from_str(raw).unwrap();
    assert_eq!(parsed.contract_version, CAPABILITY_CONTRACT_VERSION);
    assert_eq!(parsed.node.hub_id, "hub-a");
    assert_eq!(parsed.node.machine_id, "machine-a");
    assert_eq!(parsed.module_generation, 7);
    assert_eq!(parsed.capabilities.len(), CapabilityId::ALL.len());
    assert!(check_snapshot(&parsed, 1_200, 7).is_ok());
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        serde_json::from_str::<serde_json::Value>(raw).unwrap()
    );
}
