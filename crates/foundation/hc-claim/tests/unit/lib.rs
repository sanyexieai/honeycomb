use super::*;

#[test]
fn highest_score_wins_within_round() {
    let policy = NominationPolicy::default();
    let claims = vec![
        ParticipationClaim::new("m1", "planner", 0.88, 1, 200),
        ParticipationClaim::new("m1", "doctor", 0.93, 1, 300),
    ];

    let winner = select_winner(&claims, &policy, 1)
        .expect("selection should succeed")
        .expect("winner should exist");

    assert_eq!(winner.instance_id, "doctor");
    assert_eq!(winner.threshold_band, ThresholdBand::High);
}

#[test]
fn earlier_timestamp_breaks_equal_score_ties() {
    let policy = NominationPolicy::default();
    let claims = vec![
        ParticipationClaim::new("m1", "planner", 0.90, 1, 100),
        ParticipationClaim::new("m1", "doctor", 0.90, 1, 200),
    ];

    let winner = select_winner(&claims, &policy, 1)
        .expect("selection should succeed")
        .expect("winner should exist");

    assert_eq!(winner.instance_id, "planner");
}

#[test]
fn no_winner_when_no_claim_meets_threshold() {
    let policy = NominationPolicy::default();
    let claims = vec![ParticipationClaim::new("m1", "planner", 0.50, 1, 100)];

    let winner = select_winner(&claims, &policy, 1).expect("selection should succeed");

    assert!(winner.is_none());
}
