use super::wire::{
    auth_from_wire, auth_to_wire, read_frame, FrameWriter, GenerationFrame, HostFrame, Op,
    WireAuth, WireCredentialKind, WireGrantStatus, WireProjectScope, MAX_FRAME_BYTES,
};
use super::*;
use gld_policy::{
    ActionsSubject, AuthContext, CredentialKind, Grant, GrantPermissions, GrantStatus, McpSubject,
    ProjectScope,
};

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

fn grant() -> Grant {
    Grant {
        id: "grant-id".into(),
        name: "client".into(),
        scope: ProjectScope::AllProjects,
        permissions: GrantPermissions::default(),
        status: GrantStatus::Active,
    }
}

#[test]
fn every_network_identity_keeps_its_authority_across_the_private_channel() {
    let subjects = [
        (
            CredentialKind::OAuth,
            McpSubject::OAuth {
                client_id: "c1".into(),
            },
        ),
        (CredentialKind::Token, McpSubject::Token),
        (CredentialKind::Url, McpSubject::Url),
        (
            CredentialKind::Token,
            McpSubject::Tunnel {
                binding_id: "binding-id".into(),
            },
        ),
    ];
    for (kind, subject) in subjects {
        let auth = AuthContext::verified_mcp("hub", grant(), kind, subject).unwrap();
        let encoded = serde_json::to_string(&auth_to_wire(&auth).unwrap()).unwrap();
        let back = auth_from_wire(serde_json::from_str::<WireAuth>(&encoded).unwrap()).unwrap();
        assert_eq!(back, auth);
        assert_eq!(back.id(), auth.id());
        assert!(!back.is_local_operator());
        assert!(!back.may_read_user_prompts());
    }
    for subject in [
        ActionsSubject::Anonymous,
        ActionsSubject::ApiKey,
        ActionsSubject::OAuth {
            client_id: "c1".into(),
        },
    ] {
        let auth = AuthContext::actions("workspace-a", "project-a", subject, true).unwrap();
        let encoded = serde_json::to_string(&auth_to_wire(&auth).unwrap()).unwrap();
        let back = auth_from_wire(serde_json::from_str::<WireAuth>(&encoded).unwrap()).unwrap();
        assert_eq!(back, auth);
        assert!(back.allows_project("project-a"));
        assert!(!back.allows_project("project-b"));
        assert!(!back.may_manage_workspaces());
    }
    assert!(auth_to_wire(&AuthContext::local_operator()).is_err());
}

#[test]
fn url_authority_cannot_turn_into_delegation_on_the_private_channel() {
    let mut grant = grant();
    grant.permissions.write = true;
    let auth =
        AuthContext::verified_mcp("hub", grant, CredentialKind::Url, McpSubject::Url).unwrap();
    let back = auth_from_wire(auth_to_wire(&auth).unwrap()).unwrap();
    assert!(!back.read_only());
    assert!(!back.may_delegate());
    assert!(!back.may_use_machine_mcp());
}

#[test]
fn malformed_or_revoked_authority_is_refused_at_the_receiver() {
    let auth = AuthContext::verified_mcp("hub", grant(), CredentialKind::Token, McpSubject::Token)
        .unwrap();
    for case in 0..5 {
        let mut wire = auth_to_wire(&auth).unwrap();
        let WireAuth::Mcp {
            grant,
            credential_kind,
            ..
        } = &mut wire
        else {
            panic!("MCP fixture")
        };
        match case {
            0 => grant.status = WireGrantStatus::Revoked,
            1 => grant.id.clear(),
            2 => grant.scope = WireProjectScope::Selected(vec![]),
            3 => *credential_kind = WireCredentialKind::OAuth,
            4 => {
                grant.scope = WireProjectScope::Selected(vec!["project-a".into()]);
                grant.permissions.service.user_prompts = true;
            }
            _ => unreachable!(),
        }
        assert!(auth_from_wire(wire).is_err(), "case {case}");
    }
}

#[test]
fn frames_round_trip_one_per_line() {
    let writer = FrameWriter::new(Vec::new());
    writer
        .send(&HostFrame::Call {
            id: 7,
            op: serde_json::to_string(&Op::Ping).unwrap(),
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
    let plain = root_key(root.path());
    let dotted = root_key(&root.path().join("."));
    assert_eq!(plain, dotted);
    assert_eq!(plain, router::root_key(root.path()));
}

#[test]
fn an_unavailable_root_keeps_its_original_owner_key() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing");
    assert_eq!(root_key(&missing), missing.display().to_string());
}

/// 只有确定没发出去的才能叫客户端重试：其余的工具可能已经跑过了。
#[test]
fn only_a_call_that_never_left_is_safe_to_retry() {
    assert!(CallError::Unavailable("restarting".into()).surely_not_executed());
    assert!(!CallError::Lost.surely_not_executed());
    assert!(!CallError::TimedOut.surely_not_executed());
    assert!(!CallError::Refused("worker_panicked".into()).surely_not_executed());
}
