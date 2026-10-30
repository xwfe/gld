use super::*;

const HUB: &str = "11111111111111111111111111111111";
const MACHINE: &str = "22222222222222222222222222222222";
const OTHER: &str = "33333333333333333333333333333333";
const MAX: usize = 1024 * 1024;

fn key() -> HubKey {
    HubKey::from_stored(&generate_private_key().unwrap()).unwrap()
}

fn binding(nonce: &str) -> Binding {
    Binding::new(HUB, MACHINE, nonce).unwrap()
}

#[test]
fn stored_keys_round_trip_and_bad_material_is_rejected() {
    let stored = generate_private_key().unwrap();
    let first = HubKey::from_stored(&stored).unwrap();
    assert_eq!(
        first.public_key(),
        HubKey::from_stored(&stored).unwrap().public_key()
    );
    assert!(valid_public_key(&first.public_key()));
    assert_ne!(first.public_key(), key().public_key());
    for bad in [
        "",
        "zz",
        &stored[..stored.len() - 2],
        &stored.to_uppercase(),
    ] {
        assert!(HubKey::from_stored(bad).is_err());
    }
    assert!(!valid_public_key(&first.public_key().to_uppercase()));
    assert!(Binding::new(HUB, MACHINE, "short").is_err());
    assert!(Binding::new("hub", MACHINE, &new_nonce().unwrap()).is_err());
}

#[test]
fn handshake_proof_is_bound_to_key_hub_machine_and_nonce() {
    let hub = key();
    let nonce = new_nonce().unwrap();
    let proof = hub.handshake_proof(&binding(&nonce));
    assert!(verify_handshake(
        &hub.public_key(),
        &binding(&nonce),
        &proof
    ));
    assert!(!verify_handshake(
        &key().public_key(),
        &binding(&nonce),
        &proof
    ));
    assert!(!verify_handshake(
        &hub.public_key(),
        &binding(&new_nonce().unwrap()),
        &proof
    ));
    for other in [
        Binding::new(OTHER, MACHINE, &nonce).unwrap(),
        Binding::new(HUB, OTHER, &nonce).unwrap(),
    ] {
        assert!(!verify_handshake(&hub.public_key(), &other, &proof));
    }
    assert!(!verify_handshake(&hub.public_key(), &binding(&nonce), ""));
    let mut flipped = proof.into_bytes();
    flipped[0] = if flipped[0] == b'0' { b'1' } else { b'0' };
    let flipped = String::from_utf8(flipped).unwrap();
    assert!(!verify_handshake(
        &hub.public_key(),
        &binding(&nonce),
        &flipped
    ));
}

#[test]
fn verified_messages_return_the_exact_hub_text_even_when_arguments_mimic_the_tag() {
    let hub = Arc::new(key());
    let nonce = new_nonce().unwrap();
    let mut signer = MessageSigner::new(hub.clone(), binding(&nonce));
    let mut verifier = MessageVerifier::new(&hub.public_key(), binding(&nonce));
    let lines = [
        r#"{"id":1,"jsonrpc":"2.0","method":"initialize","params":{}}"#.to_string(),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"apply_patch",
            "arguments":{"a":1,"gld_sig":"1.00","patch":",\"gld_sig\":\"9.ff\"}","text":"中文"}
        }})
        .to_string(),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.to_string(),
    ];
    for line in &lines {
        let signed = signer.sign(line, MAX).unwrap();
        serde_json::from_str::<serde_json::Value>(&signed).unwrap();
        assert_eq!(&verifier.verify(&signed).unwrap(), line);
    }
}

#[test]
fn tampered_injected_replayed_or_reordered_messages_are_rejected() {
    let hub = Arc::new(key());
    let nonce = new_nonce().unwrap();
    let mut signer = MessageSigner::new(hub.clone(), binding(&nonce));
    let first = signer
        .sign(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, MAX)
        .unwrap();
    let second = signer
        .sign(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#, MAX)
        .unwrap();
    let fresh = || MessageVerifier::new(&hub.public_key(), binding(&nonce));

    let tampered = first.replace("ping", "pong");
    assert!(fresh().verify(&tampered).is_err());
    assert!(fresh()
        .verify(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)
        .is_err());
    assert!(
        fresh().verify(&second).is_err(),
        "skipped a sequence number"
    );

    let mut verifier = fresh();
    verifier.verify(&first).unwrap();
    assert!(verifier.verify(&first).is_err(), "replay");
    verifier.verify(&second).unwrap();

    let other_connection =
        MessageVerifier::new(&hub.public_key(), binding(&new_nonce().unwrap())).verify(&first);
    assert!(other_connection.is_err());
    let impostor = Arc::new(key());
    let forged = MessageSigner::new(impostor, binding(&nonce))
        .sign(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, MAX)
        .unwrap();
    assert!(fresh().verify(&forged).is_err());
}

#[test]
fn oversized_or_non_object_messages_do_not_consume_a_sequence_number() {
    let hub = Arc::new(key());
    let nonce = new_nonce().unwrap();
    let mut signer = MessageSigner::new(hub.clone(), binding(&nonce));
    let line = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    assert!(signer.sign(line, line.len() + 10).is_err());
    assert!(signer.sign("{}", MAX).is_err());
    assert!(signer.sign("[1]", MAX).is_err());
    let signed = signer.sign(line, MAX).unwrap();
    let mut verifier = MessageVerifier::new(&hub.public_key(), binding(&nonce));
    assert_eq!(verifier.verify(&signed).unwrap(), line);
}
