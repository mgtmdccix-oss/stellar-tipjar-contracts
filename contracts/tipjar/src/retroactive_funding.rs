//! Retroactive Public Goods Funding (RetroPGF)
//!
//! Rewards past contributions to public goods:
//! 1. Admin creates a round with a reward pool and evaluation criteria.
//! 2. Projects are nominated with impact metrics (tip volume, unique tippers).
//! 3. Voters (token-weighted) cast votes during the voting window.
//! 4. Admin finalizes; rewards are distributed proportionally to votes received.

use soroban_sdk::{contracttype, panic_with_error, symbol_short, token, Address, Env, String, Vec};

// ── Storage keys ──────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RFKey {
    Round(u64),
    RoundCtr,
    Project(u64, Address),
    RoundProjects(u64),
    RoundTotalVotes(u64),
    HasVoted(u64, Address),
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoundStatus {
    Nominations,
    Voting,
    Finalized,
}

/// Evaluation criteria stored on-chain for transparency.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvalCriteria {
    pub min_tip_volume: i128,
    pub min_unique_tippers: u32,
    pub description: String,
}

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

/// Impact metrics and vote/reward tracking for a nominated project.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRecord {
    pub project: Address,
    pub round_id: u64,
    pub tip_volume: i128,
    pub unique_tippers: u32,
    pub impact_description: String,
    pub votes: i128,
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
    let round_id: u64 = env.storage().instance().get(&RFKey::RoundCtr).unwrap_or(0);
    env.storage().instance().set(&RFKey::RoundCtr, &(round_id + 1));

    token::Client::new(env, token).transfer(admin, &env.current_contract_address(), &reward_pool);

    env.storage().persistent().set(
        &RFKey::Round(round_id),
        &RetroRound {
            round_id,
            admin: admin.clone(),
            token: token.clone(),
            reward_pool,
            criteria,
            voting_start,
            voting_end,
            status: RoundStatus::Nominations,
        },
    );
    env.events().publish((symbol_short!("rf_new"),), (round_id, admin.clone(), token.clone(), reward_pool));
    round_id
}

pub fn nominate_project(
    env: &Env,
    round_id: u64,
    project: &Address,
    tip_volume: i128,
    unique_tippers: u32,
    impact_description: String,
) {
    let round: RetroRound = env.storage().persistent().get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));
    if round.status != RoundStatus::Nominations {
        panic_with_error!(env, RFError::InvalidStatus);
    }
    if tip_volume < round.criteria.min_tip_volume || unique_tippers < round.criteria.min_unique_tippers {
        panic_with_error!(env, RFError::ProjectNotEligible);
    }
    let proj_key = RFKey::Project(round_id, project.clone());
    if env.storage().persistent().has(&proj_key) {
        panic_with_error!(env, RFError::ProjectAlreadyNominated);
    }
    env.storage().persistent().set(&proj_key, &ProjectRecord {
        project: project.clone(),
        round_id,
        tip_volume,
        unique_tippers,
        impact_description,
        votes: 0,
        reward: 0,
    });
    let projects_key = RFKey::RoundProjects(round_id);
    let mut projects: Vec<Address> = env.storage().persistent().get(&projects_key).unwrap_or_else(|| Vec::new(env));
    projects.push_back(project.clone());
    env.storage().persistent().set(&projects_key, &projects);
    env.events().publish((symbol_short!("rf_nom"),), (round_id, project.clone(), tip_volume, unique_tippers));
}

pub fn open_voting(env: &Env, admin: &Address, round_id: u64) {
    let mut round: RetroRound = env.storage().persistent().get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));
    if round.admin != *admin { panic_with_error!(env, RFError::Unauthorized); }
    if round.status != RoundStatus::Nominations { panic_with_error!(env, RFError::InvalidStatus); }
    round.status = RoundStatus::Voting;
    env.storage().persistent().set(&RFKey::Round(round_id), &round);
    env.events().publish((symbol_short!("rf_vote"),), (round_id,));
}

pub fn cast_vote(env: &Env, voter: &Address, round_id: u64, project: &Address, weight: i128) {
    if weight <= 0 { panic_with_error!(env, RFError::InvalidAmount); }
    let round: RetroRound = env.storage().persistent().get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));
    if round.status != RoundStatus::Voting { panic_with_error!(env, RFError::VotingNotOpen); }
    let now = env.ledger().timestamp();
    if now < round.voting_start || now > round.voting_end { panic_with_error!(env, RFError::VotingNotOpen); }

    let voted_key = RFKey::HasVoted(round_id, voter.clone());
    if env.storage().persistent().has(&voted_key) { panic_with_error!(env, RFError::AlreadyVoted); }

    let proj_key = RFKey::Project(round_id, project.clone());
    let mut record: ProjectRecord = env.storage().persistent().get(&proj_key)
        .unwrap_or_else(|| panic_with_error!(env, RFError::ProjectNotFound));

    env.storage().persistent().set(&voted_key, &true);
    record.votes = record.votes.saturating_add(weight);
    env.storage().persistent().set(&proj_key, &record);

    let total: i128 = env.storage().persistent().get(&RFKey::RoundTotalVotes(round_id)).unwrap_or(0);
    env.storage().persistent().set(&RFKey::RoundTotalVotes(round_id), &total.saturating_add(weight));
    env.events().publish((symbol_short!("rf_cast"),), (round_id, voter.clone(), project.clone(), weight));
}

pub fn finalize_and_distribute(env: &Env, admin: &Address, round_id: u64) {
    let mut round: RetroRound = env.storage().persistent().get(&RFKey::Round(round_id))
        .unwrap_or_else(|| panic_with_error!(env, RFError::RoundNotFound));
    if round.admin != *admin { panic_with_error!(env, RFError::Unauthorized); }
    if round.status != RoundStatus::Voting { panic_with_error!(env, RFError::InvalidStatus); }
    if env.ledger().timestamp() <= round.voting_end { panic_with_error!(env, RFError::VotingNotEnded); }

    let total_votes: i128 = env.storage().persistent().get(&RFKey::RoundTotalVotes(round_id)).unwrap_or(0);
    let projects: Vec<Address> = env.storage().persistent().get(&RFKey::RoundProjects(round_id))
        .unwrap_or_else(|| Vec::new(env));

    let token_client = token::Client::new(env, &round.token);
    let contract = env.current_contract_address();
    let pool = round.reward_pool;
    let count = projects.len() as i128;
    let mut distributed: i128 = 0;

    for (i, project) in projects.iter().enumerate() {
        let proj_key = RFKey::Project(round_id, project.clone());
        let mut record: ProjectRecord = env.storage().persistent().get(&proj_key)
            .unwrap_or_else(|| panic_with_error!(env, RFError::ProjectNotFound));

        let reward = if i == (projects.len() - 1) as usize {
            pool - distributed
        } else if total_votes > 0 {
            (pool * record.votes) / total_votes
        } else {
            pool / count
        };

        if reward > 0 {
            token_client.transfer(&contract, &project, &reward);
            distributed += reward;
        }
        record.reward = reward;
        env.storage().persistent().set(&proj_key, &record);
        env.events().publish((symbol_short!("rf_dist"),), (round_id, project.clone(), record.votes, reward));
    }

    round.status = RoundStatus::Finalized;
    env.storage().persistent().set(&RFKey::Round(round_id), &round);
    env.events().publish((symbol_short!("rf_done"),), (round_id, distributed));
}

// ── Queries ───────────────────────────────────────────────────────────────────

pub fn get_round(env: &Env, round_id: u64) -> Option<RetroRound> {
    env.storage().persistent().get(&RFKey::Round(round_id))
}

pub fn get_project(env: &Env, round_id: u64, project: &Address) -> Option<ProjectRecord> {
    env.storage().persistent().get(&RFKey::Project(round_id, project.clone()))
}

pub fn get_round_projects(env: &Env, round_id: u64) -> Vec<Address> {
    env.storage().persistent().get(&RFKey::RoundProjects(round_id)).unwrap_or_else(|| Vec::new(env))
}

pub fn has_voted(env: &Env, round_id: u64, voter: &Address) -> bool {
    env.storage().persistent().has(&RFKey::HasVoted(round_id, voter.clone()))
}
