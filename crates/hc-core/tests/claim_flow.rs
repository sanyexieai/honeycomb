use hc_core::{
    EventKind, MessageKind, MessageRoute, NominationStatus, ParticipationClaim, RuntimeCommand,
    RuntimeCommandResult, RuntimeError, RuntimeSupervisor,
};

fn seeded_runtime() -> (RuntimeSupervisor, String, String, String, String) {
    let mut runtime = RuntimeSupervisor::new();
    let session = runtime.create_session("demo");
    let alice = runtime
        .create_instance(&session.id, "alice", None)
        .expect("alice should be created");
    let doctor = runtime
        .create_instance(&session.id, "doctor", None)
        .expect("doctor should be created");
    let coder = runtime
        .create_instance(&session.id, "coder", None)
        .expect("coder should be created");
    (runtime, session.id, alice.id, doctor.id, coder.id)
}

#[test]
fn broadcast_message_can_be_awarded_to_highest_claim_in_round() {
    let (mut runtime, session_id, alice_id, doctor_id, coder_id) = seeded_runtime();

    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "medical question",
            None,
        )
        .expect("broadcast should succeed");

    runtime
        .submit_participation_claim(
            ParticipationClaim::new(&message.id, &coder_id, 0.86, 1, 200)
                .with_reason("general fallback"),
        )
        .expect("coder claim should succeed");
    runtime
        .submit_participation_claim(
            ParticipationClaim::new(&message.id, &doctor_id, 0.93, 1, 100)
                .with_reason("medical specialty match"),
        )
        .expect("doctor claim should succeed");

    let grant = runtime
        .resolve_speaking_grant(&message.id, 1)
        .expect("grant resolution should succeed")
        .expect("winner should exist");

    assert_eq!(grant.instance_id, doctor_id);
    assert_eq!(runtime.state().speaking_grants.len(), 1);
    assert_eq!(
        runtime.state().events.last().map(|event| &event.kind),
        Some(&EventKind::SpeakingGranted)
    );
}

#[test]
fn broadcast_chat_message_opens_nomination_automatically() {
    let (mut runtime, session_id, alice_id, _doctor_id, _coder_id) = seeded_runtime();

    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "medical question",
            None,
        )
        .expect("broadcast should succeed");

    let nomination = runtime
        .nomination_for_message(&message.id)
        .expect("nomination should exist");

    assert_eq!(nomination.current_round, 1);
    assert_eq!(nomination.status, NominationStatus::Open);
    assert_eq!(
        runtime.state().events.last().map(|event| &event.kind),
        Some(&EventKind::NominationOpened)
    );
}

#[test]
fn low_score_claim_waits_until_lower_threshold_round() {
    let (mut runtime, session_id, alice_id, doctor_id, _coder_id) = seeded_runtime();

    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "unclear topic",
            None,
        )
        .expect("broadcast should succeed");

    runtime
        .submit_participation_claim(
            ParticipationClaim::new(&message.id, &doctor_id, 0.62, 2, 100)
                .with_reason("reasonable but not top confidence"),
        )
        .expect("claim should succeed");

    let first_round = runtime
        .resolve_speaking_grant(&message.id, 1)
        .expect("round 1 should resolve");
    assert!(first_round.is_none());
    let nomination_after_first_round = runtime
        .nomination_for_message(&message.id)
        .expect("nomination should still exist");
    assert_eq!(nomination_after_first_round.current_round, 2);
    assert_eq!(nomination_after_first_round.status, NominationStatus::Open);
    assert_eq!(
        runtime.state().events.last().map(|event| &event.kind),
        Some(&EventKind::NominationAdvanced)
    );

    let second_round = runtime
        .resolve_speaking_grant(&message.id, 2)
        .expect("round 2 should resolve")
        .expect("round 2 should have winner");
    assert_eq!(second_round.instance_id, doctor_id);
    assert_eq!(second_round.round, 2);
    let nomination_after_second_round = runtime
        .nomination_for_message(&message.id)
        .expect("nomination should still exist");
    assert_eq!(nomination_after_second_round.status, NominationStatus::Granted);
}

#[test]
fn invalid_claim_score_is_rejected() {
    let (mut runtime, session_id, alice_id, doctor_id, _coder_id) = seeded_runtime();
    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "medical question",
            None,
        )
        .expect("broadcast should succeed");

    let error = runtime
        .submit_participation_claim(ParticipationClaim::new(
            &message.id,
            &doctor_id,
            1.5,
            1,
            100,
        ))
        .expect_err("invalid score should fail");

    assert!(matches!(error, RuntimeError::InvalidClaimScore(_)));
}

#[test]
fn dispatch_claim_and_grant_path_is_wired() {
    let (mut runtime, session_id, alice_id, doctor_id, _coder_id) = seeded_runtime();
    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "medical question",
            None,
        )
        .expect("broadcast should succeed");

    let claim_result = runtime
        .dispatch(RuntimeCommand::SubmitParticipationClaim {
            claim: ParticipationClaim::new(&message.id, &doctor_id, 0.91, 1, 100),
        })
        .expect("claim dispatch should succeed");
    let RuntimeCommandResult::Claim(claim) = claim_result else {
        panic!("expected claim result");
    };
    assert_eq!(claim.instance_id, doctor_id);

    let grant_result = runtime
        .dispatch(RuntimeCommand::ResolveSpeakingGrant {
            message_id: message.id.clone(),
            round: 1,
        })
        .expect("grant dispatch should succeed");
    let RuntimeCommandResult::SpeakingGrant(grant) = grant_result else {
        panic!("expected speaking grant result");
    };
    assert_eq!(grant.expect("winner should exist").instance_id, doctor_id);
}

#[test]
fn claims_for_message_returns_only_matching_claims() {
    let (mut runtime, session_id, alice_id, doctor_id, coder_id) = seeded_runtime();

    let message_one = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "first topic",
            None,
        )
        .expect("first message should succeed");
    let message_two = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "second topic",
            None,
        )
        .expect("second message should succeed");

    runtime
        .submit_participation_claim(ParticipationClaim::new(
            &message_one.id,
            &doctor_id,
            0.90,
            1,
            100,
        ))
        .expect("first claim should succeed");
    runtime
        .submit_participation_claim(ParticipationClaim::new(
            &message_two.id,
            &coder_id,
            0.88,
            1,
            200,
        ))
        .expect("second claim should succeed");

    let claims = runtime
        .claims_for_message(&message_one.id)
        .expect("claims query should succeed");

    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].instance_id, doctor_id);
    assert_eq!(claims[0].message_id, message_one.id);
}

#[test]
fn nomination_exhausts_when_all_rounds_fail() {
    let (mut runtime, session_id, alice_id, _doctor_id, _coder_id) = seeded_runtime();

    let message = runtime
        .post_message(
            &session_id,
            &alice_id,
            MessageRoute::Broadcast,
            MessageKind::Chat,
            "nobody knows",
            None,
        )
        .expect("broadcast should succeed");

    assert!(runtime
        .resolve_speaking_grant(&message.id, 1)
        .expect("round 1 should resolve")
        .is_none());
    assert!(runtime
        .resolve_speaking_grant(&message.id, 2)
        .expect("round 2 should resolve")
        .is_none());
    assert!(runtime
        .resolve_speaking_grant(&message.id, 3)
        .expect("round 3 should resolve")
        .is_none());

    let nomination = runtime
        .nomination_for_message(&message.id)
        .expect("nomination should exist");
    assert_eq!(nomination.status, NominationStatus::Exhausted);
    assert_eq!(
        runtime.state().events.last().map(|event| &event.kind),
        Some(&EventKind::NominationExhausted)
    );
}
