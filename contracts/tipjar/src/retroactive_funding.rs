//! Retroactive Public Goods Funding (RetroPGF)
//!
//! Rewards past contributions to public goods:
//! 1. Admin creates a round with a reward pool and evaluation criteria.
//! 2. Nominated projects are registered with impact metrics (tip volume, unique tippers).
//! 3. Voters (token-weighted) cast votes for projects during the voting window.
//! 4. Admin finalizes the round; rewards are distributed proportionally to votes received.

use soroban_sdk::{contracttype, panic_with_error, symbol_short, token, Address, Env, String, Vec};

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RFKey {
    /// Round record keyed by round_id.
    Round(u64),
    /// Global round counter.
    RoundCtr,
    /// Project record keyed by (round_id, project).
    Project(u64, Address),
    /// List of projects in a round.
    RoundProjects(u64),
    /// Vote weight cast by a voter for a project: (round_id, project, voter).
    Vote(u64, Address, Address),
    /// Total votes accumulated for a project: (round_id, project).
    ProjectVotes(u64, Address),
    /// Total votes cast in a round.
    RoundTotalVotes(u64),
    /// Whether a voter has voted in a round (Sybil guard): (round_id, voter).
    HasVoted(u64, Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoundStatus {
    Nominations,  // accepting project nominations
    Voting,       // voting window open
    Finalized,    // voting closed, rewards distributed
}

/// Evaluation criteria stored on-chain for transparency.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalCriteria {
    /// Minimum historical tip volume (in token units) for a project to be eligible.
    pub min_tip_volume: i128,
    /// Minimum number of unique tippers required.
    pub min_unique_tippers: u32,
    /// Description of qualitative criteria (stored for transparency).
    pub description: String,
}

/// A retroactive funding round.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetroRound {
    pub round_id: u64,
    pub admin: Address,
    pub token: Address,
    pub reward_pool: i128,
    pub criteria: EvalCriteria,
    pub voting_start: u64,
    pub voting_end: u64,
    pub status: RoundStatus,
}

/// Impact metrics and nomination data for a project.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRecord {
    pub project: Address,
    pub round_id: u64,
    /// Total historical tip volume used as impact metric.
    pub tip_volume: i128,
    /// Number of unique tippers as impact metric.
    pub unique_tippers: u32,
    /// Free-form impact description.
    pub impact_description: String,
    /// Votes accumulated during voting window.
    pub votes: i128,
    /// Reward distributed (set after finalization).
    pub reward: i128,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[soroban_sdk::contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum RFError {
    RoundNotFound = 210,
    InvalidStatus = 211,
    VotingNotOpen = 212,
    AlreadyVoted = 213,
    InvalidAmount = 214,
    Unauthorized = 215,
    ProjectNotEligible = 216,
    ProjectAlreadyNominated = 217,
    ProjectNotFound = 218,
    VotingNotEnded = 219,
}

// ── Core functions ────────────────────────────────────────────────────────────

/// Creates a new retroactive funding round.
/// Admin deposits the reward pool immediately.
pub fn create_round(
    env: &Env,
    admin: &Address,
    token: &Address,
    reward_pool: i128,
    criteria: EvalCriteria,
    voting_start: u64,
    voting_end: u64,
) -> u64 {
    if reward_pool <= 0 || voting_end <= voting_start {
        panic_with_error!(env, RFError::InvalidAmount);
    }

    let round_id: u64 = env
        .storage()
        .instance()
        .get(&RFKey::RoundCtr)
        .unwrap_or(0);
    env.storage()
        .instance()
        .set(&RFKey::RoundCtr, &(round_id + 1));

    let round = RetroRound {
        round_id,
        admin: admin.clone(),
        token: token.clone(),
        reward_pool,
        criteria,
        voting_start,
        voting_end,
        status: RoundStatus::Nominations,
    };

    token::Client::new(env, token).transfer(admin, &env.current_contract_address(), &reward_pool);

    env.storage().persistent().set(&RFKey::Round(round_id), &round);

    env.events().publish(
        (symbol_short!("rf_new"),),
        (round_id, admin.clone(), token.clone(), reward_pool),
    );

    round_id
}

/// Nominates a project for a round, recording its impact metrics.
/// Anyone may nominate; eligibility is checked against the round's criteria.
pub fn nominate_project(
    env: &Env,
    round_id: u64,
    project: &Address,
    tip_volume: i128,
    unique_tippers: u32,
    impact_description: String,
) {
    let round: RetroRound = env
        .storage()
        .persistent()
        .get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));

    if round.status != RoundStatus::Nominations {
        panic_with_error!(env, RFError::InvalidStatus);
    }

    // Check eligibility against evaluation criteria.
    if tip_volume < round.criteria.min_tip_volume
        || unique_tippers < round.criteria.min_unique_tippers
    {
        panic_with_error!(env, RFError::ProjectNotEligible);
    }

    let proj_key = RFKey::Project(round_id, project.clone());
    if env.storage().persistent().has(&proj_key) {
        panic_with_error!(env, RFError::ProjectAlreadyNominated);
    }

    let record = ProjectRecord {
        project: project.clone(),
        round_id,
        tip_volume,
        unique_tippers,
        impact_description,
        votes: 0,
        reward: 0,
    };
    env.storage().persistent().set(&proj_key, &record);

    let projects_key = RFKey::RoundProjects(round_id);
    let mut projects: Vec<Address> = env
        .storage()
        .persistent()
        .get(&projects_key)
        .unwrap_or_else(|| Vec::new(env));
    projects.push_back(project.clone());
    env.storage().persistent().set(&projects_key, &projects);

    env.events().publish(
        (symbol_short!("rf_nom"),),
        (round_id, project.clone(), tip_volume, unique_tippers),
    );
}

/// Opens the voting window. Admin only.
pub fn open_voting(env: &Env, admin: &Address, round_id: u64) {
    let mut round: RetroRound = env
        .storage()
        .persistent()
        .get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));

    if round.admin != *admin {
        panic_with_error!(env, RFError::Unauthorized);
    }
    if round.status != RoundStatus::Nominations {
        panic_with_error!(env, RFError::InvalidStatus);
    }

    round.status = RoundStatus::Voting;
    env.storage().persistent().set(&RFKey::Round(round_id), &round);

    env.events()
        .publish((symbol_short!("rf_vote"),), (round_id,));
}

/// Casts `weight` votes for `project` in `round_id`.
/// Each voter may vote once per round. Weight represents token-weighted influence.
pub fn cast_vote(
    env: &Env,
    voter: &Address,
    round_id: u64,
    project: &Address,
    weight: i128,
) {
    if weight <= 0 {
        panic_with_error!(env, RFError::InvalidAmount);
    }

    let round: RetroRound = env
        .storage()
        .persistent()
        .get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));

    if round.status != RoundStatus::Voting {
        panic_with_error!(env, RFError::VotingNotOpen);
    }
    let now = env.ledger().timestamp();
    if now < round.voting_start || now > round.voting_end {
        panic_with_error!(env, RFError::VotingNotOpen);
    }

    // One vote per voter per round.
    let voted_key = RFKey::HasVoted(round_id, voter.clone());
    if env.storage().persistent().has(&voted_key) {
        panic_with_error!(env, RFError::AlreadyVoted);
    }

    // Project must be nominated.
    let proj_key = RFKey::Project(round_id, project.clone());
    let mut record: ProjectRecord = env
        .storage()
        .persistent()
        .get(&proj_key)
        .unwrap_or_else(|| panic_with_error!(env, RFError::ProjectNotFound));

    // Record vote.
    env.storage().persistent().set(&voted_key, &true);

    record.votes = record.votes.saturating_add(weight);
    env.storage().persistent().set(&proj_key, &record);

    let total_key = RFKey::RoundTotalVotes(round_id);
    let total: i128 = env.storage().persistent().get(&total_key).unwrap_or(0);
    env.storage()
        .persistent()
        .set(&total_key, &total.saturating_add(weight));

    env.events().publish(
        (symbol_short!("rf_cast"),),
        (round_id, voter.clone(), project.clone(), weight),
    );
}

/// Finalizes the round and distributes rewards proportionally to votes.
/// Admin only; voting window must have ended.
pub fn finalize_and_distribute(env: &Env, admin: &Address, round_id: u64) {
    let mut round: RetroRound = env
        .storage()
        .persistent()
        .get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));

    if round.admin != *admin {
        panic_with_error!(env, RFError::Unauthorized);
    }
    if round.status != RoundStatus::Voting {
        panic_with_error!(env, RFError::InvalidStatus);
    }
    if env.ledger().timestamp() <= round.voting_end {
        panic_with_error!(env, RFError::VotingNotEnded);
    }

    let total_votes: i128 = env
        .storage()
        .persistent()
        .get(&RFKey::RoundTotalVotes(round_id))
        .unwrap_or(0);

    let projects: Vec<Address> = env
        .storage()
        .persistent()
        .get(&RFKey::RoundProjects(round_id))
        .unwrap_or_else(|| Vec::new(env));

    let token_client = token::Client::new(env, &round.token);
    let contract = env.current_contract_address();
    let reward_pool = round.reward_pool;
    let mut distributed: i128 = 0;
    let count = projects.len();

    for (i, project) in projects.iter().enumerate() {
        let proj_key = RFKey::Project(round_id, project.clone());
        let mut record: ProjectRecord = env
            .storage()
            .persistent()
            .get(&proj_key)
            .unwrap_or_else(|| panic_with_error!(env, RFError::ProjectNotFound));

        let reward = if total_votes == 0 {
            // No votes: split equally.
            if i == (count - 1) as usize {
                reward_pool - distributed
            } else {
                reward_pool / count as i128
            }
        } else if i == (count - 1) as usize {
            // Last project absorbs rounding remainder.
            reward_pool - distributed
        } else {
            (reward_pool * record.votes) / total_votes
        };

        if reward > 0 {
            token_client.transfer(&contract, &project, &reward);
            distributed += reward;
        }

        record.reward = reward;
        env.storage().persistent().set(&proj_key, &record);

        env.events().publish(
            (symbol_short!("rf_dist"),),
            (round_id, project.clone(), record.votes, reward),
        );
    }

    round.status = RoundStatus::Finalized;
    env.storage().persistent().set(&RFKey::Round(round_id), &round);

    env.events()
        .publish((symbol_short!("rf_done"),), (round_id, distributed));
}

// ── Queries ───────────────────────────────────────────────────────────────────

pub fn get_round(env: &Env, round_id: u64) -> Option<RetroRound> {
    env.storage().persistent().get(&RFKey::Round(round_id))
}

pub fn get_project(env: &Env, round_id: u64, project: &Address) -> Option<ProjectRecord> {
    env.storage()
        .persistent()
        .get(&RFKey::Project(round_id, project.clone()))
}

pub fn get_round_projects(env: &Env, round_id: u64) -> Vec<Address> {
    env.storage()
        .persistent()
        .get(&RFKey::RoundProjects(round_id))
        .unwrap_or_else(|| Vec::new(env))
}

pub fn has_voted(env: &Env, round_id: u64, voter: &Address) -> bool {
    env.storage()
        .persistent()
        .has(&RFKey::HasVoted(round_id, voter.clone()))
}
