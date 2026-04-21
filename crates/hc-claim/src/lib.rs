//! Claim and speaking-right protocol for distributed participation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParticipationClaim {
    pub message_id: String,
    pub instance_id: String,
    pub score: f32,
    pub reason: Option<String>,
    pub round: u32,
    pub timestamp_ms: u64,
}

impl ParticipationClaim {
    pub fn new(
        message_id: impl Into<String>,
        instance_id: impl Into<String>,
        score: f32,
        round: u32,
        timestamp_ms: u64,
    ) -> Self {
        Self {
            message_id: message_id.into(),
            instance_id: instance_id.into(),
            score,
            reason: None,
            round,
            timestamp_ms,
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdBand {
    High,
    Medium,
    Low,
}

impl ThresholdBand {
    pub fn minimum_score(self) -> f32 {
        match self {
            Self::High => 0.85,
            Self::Medium => 0.60,
            Self::Low => 0.35,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NominationRound {
    pub round: u32,
    pub band: ThresholdBand,
    pub wait_ms: u64,
}

impl NominationRound {
    pub fn new(round: u32, band: ThresholdBand, wait_ms: u64) -> Self {
        Self {
            round,
            band,
            wait_ms,
        }
    }

    pub fn minimum_score(&self) -> f32 {
        self.band.minimum_score()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NominationPolicy {
    pub rounds: Vec<NominationRound>,
}

impl Default for NominationPolicy {
    fn default() -> Self {
        Self {
            rounds: vec![
                NominationRound::new(1, ThresholdBand::High, 750),
                NominationRound::new(2, ThresholdBand::Medium, 1000),
                NominationRound::new(3, ThresholdBand::Low, 1250),
            ],
        }
    }
}

impl NominationPolicy {
    pub fn round(&self, round: u32) -> Option<&NominationRound> {
        self.rounds
            .iter()
            .find(|candidate| candidate.round == round)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakingGrant {
    pub message_id: String,
    pub instance_id: String,
    pub round: u32,
    pub score: f32,
    pub threshold_band: ThresholdBand,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ClaimError {
    #[error("score must be within 0.0..=1.0")]
    InvalidScore,
    #[error("claim round is not defined by policy: {0}")]
    UnknownRound(u32),
}

pub fn select_winner(
    claims: &[ParticipationClaim],
    policy: &NominationPolicy,
    round: u32,
) -> Result<Option<SpeakingGrant>, ClaimError> {
    let nomination_round = policy.round(round).ok_or(ClaimError::UnknownRound(round))?;

    let winner = claims
        .iter()
        .filter(|claim| claim.round == round)
        .map(|claim| validate_claim_score(claim).map(|_| claim))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|claim| claim.score >= nomination_round.minimum_score())
        .max_by(|left, right| {
            left.score
                .total_cmp(&right.score)
                .then_with(|| right.timestamp_ms.cmp(&left.timestamp_ms))
        });

    Ok(winner.map(|claim| SpeakingGrant {
        message_id: claim.message_id.clone(),
        instance_id: claim.instance_id.clone(),
        round,
        score: claim.score,
        threshold_band: nomination_round.band,
    }))
}

fn validate_claim_score(claim: &ParticipationClaim) -> Result<(), ClaimError> {
    if !(0.0..=1.0).contains(&claim.score) {
        return Err(ClaimError::InvalidScore);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
