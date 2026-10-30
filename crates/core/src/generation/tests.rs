use super::wire::{
    read_frame, FrameWriter, GenerationFrame, HostFrame, Op, WireAuth, MAX_FRAME_BYTES,
};
use super::*;
use crate::auth::{AuthContext, Grant, Principal};

fn identity() -> BuildIdentity {
    BuildIdentity {
        version: "0.0.0".into(),
        build_commit: None,
        generation_protocol: GENERATION_PROTOCOL,
        state_format: STATE_FORMAT,
        pid: 1,
        exe: None,
    }
}

#[test]
fn only_a_matching_protocol_and_state_format_is_usable() {
    assert_eq!(identity().incompatibility(), None);
    let newer = BuildIdentity {
        generation_protocol: GENERATION_PROTOCOL + 1,
        ..identity()
    };
    assert!(newer.incompatibility().unwrap().contains("协议"));
    let older = BuildIdentity {
        generation_protocol: GENERATION_PROTOCOL_MIN - 1,
        ..identity()
    };
    assert!(older.incompatibility().is_some());
    let other_state = BuildIdentity {
        state_format: STATE_FORMAT + 1,
        ..identity()
    };
    assert!(other_state.incompatibility().unwrap().contains("状态格式"));
}

/// `Grant::browser` 标了 serde(skip)。直接序列化会丢，浏览器地址进来的 grant 到了执行代
/// 就成了能委派全权 Agent 的 grant，这是一次权限提升，不是展示问题。
#[test]
fn the_browser_flag_of_a_grant_survives_the_private_channel() {
    let grant = Grant {
        id: "g".into(),
        name: "n".into(),
        workspaces: vec!["w".into()],
        read_only: false,
        browser: true,
    };
    let auth = AuthContext::new(Principal::SharedSecret, "hub").with_grant(Some(grant));
    let wire = serde_json::to_string(&WireAuth::from(&auth)).unwrap();
    let back = serde_json::from_str::<WireAuth>(&wire).unwrap().into_auth();
    assert_eq!(back, auth);
    assert!(!back.grant().unwrap().may_delegate());
}

#[test]
fn every_principal_keeps_its_identity_across_the_channel() {
    for auth in [
        AuthContext::anonymous("hub"),
        AuthContext::new(Principal::SharedSecret, "hub"),
        AuthContext::new(
            Principal::OAuthClient {
                client_id: "c1".into(),
            },
            "hub",
        ),
        AuthContext::new(Principal::OpenAiTunnel, "hub"),
    ] {
        let wire = serde_json::to_string(&WireAuth::from(&auth)).unwrap();
        let back = serde_json::from_str::<WireAuth>(&wire).unwrap().into_auth();
        assert_eq!(back, auth);
        assert_eq!(back.tag(), auth.tag());
    }
}

#[test]
fn frames_round_trip_one_per_line() {
    let writer = FrameWriter::new(Vec::new());
    writer
        .send(&HostFrame::Call {
            id: 7,
            op: serde_json::to_value(Op::Ping).unwrap(),
        })
        .unwrap();
    writer
        .send(&GenerationFrame::Ready {
            identity: identity(),
        })
        .unwrap();
    let bytes = writer_bytes(writer);
    let mut reader = std::io::BufReader::new(bytes.as_slice());
    let first = read_frame(&mut reader).unwrap().unwrap();
    assert!(matches!(
        serde_json::from_str::<HostFrame>(&first).unwrap(),
        HostFrame::Call { id: 7, .. }
    ));
    let second = read_frame(&mut reader).unwrap().unwrap();
    assert!(matches!(
        serde_json::from_str::<GenerationFrame>(&second).unwrap(),
        GenerationFrame::Ready { .. }
    ));
    assert!(read_frame(&mut reader).unwrap().is_none());
}

fn writer_bytes(writer: FrameWriter<Vec<u8>>) -> Vec<u8> {
    writer.into_inner()
}

/// 对端发一条没有换行的巨型消息时要报错，不能一直攒到把内存吃光。
#[test]
fn an_oversized_frame_is_an_error_not_a_memory_leak() {
    let data = vec![b'x'; MAX_FRAME_BYTES + 1];
    let mut reader = std::io::BufReader::new(data.as_slice());
    assert_eq!(
        read_frame(&mut reader).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

/// 工作区所有权的键：同一个目录换个写法还是同一个主人，否则两代会同时写它。
#[test]
fn a_workspace_has_one_owner_key_however_its_path_is_written() {
    let root = tempfile::tempdir().unwrap();
    let plain = router::root_key(root.path());
    let dotted = router::root_key(&root.path().join("."));
    assert_eq!(plain, dotted);
}
