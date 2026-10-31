//! Shared test fixtures for the planner and the limits service: settings you can change, fake
//! boards, a registry, and track records.

use std::str::FromStr;
use std::sync::Arc;

use argonauts_core::agents::session_runner::RunnerDirectory;
use argonauts_core::base::names::{AgentId, ModelRef, RoleName};
use argonauts_core::models::limits::{
    CandidateQuery, PlanUsageReading, PlanUsageSource, PlanUsageStatus,
};
use argonauts_core::models::registry::{
    CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind,
};
use argonauts_core::runs::events::task::{TaskKind, TaskLimit, TaskSpec};
use argonauts_core::settings::Settings;
use argonauts_core::settings::fallback::FallbackEntry;
use argonauts_core::{RunId, TaskId};
use argonauts_testkit::engine::services::{
    MemoryHealthBoard, MemoryPlanUsageBoard, StaticRegistry, StaticTrackRecords,
};
use time::OffsetDateTime;

use super::Planner;
use crate::limits::recovery::Limits;

pub const RUN: &str = "run-01";
pub const TASK: &str = "task-003";

pub struct Fixture {
    pub settings: Settings,
    pub registry: Arc<StaticRegistry>,
    pub plan_usage: Arc<MemoryPlanUsageBoard>,
    pub health: Arc<MemoryHealthBoard>,
    pub records: Arc<StaticTrackRecords>,
}

impl Fixture {
    /// The default settings, whose crew is `claude-code`, `codex`, and `antigravity-cli`.
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            registry: Arc::default(),
            plan_usage: Arc::default(),
            health: Arc::default(),
            records: Arc::default(),
        }
    }

    /// A crew of named models in three providers: `claude-code/opus` and `claude-code/sonnet`
    /// (anthropic), `codex/gpt-5` (openai), and `antigravity-cli/gemini-3` (google).
    pub fn with_models() -> Self {
        let mut fixture = Self::new();
        let crew = [
            ("claude-code", "opus", "anthropic"),
            ("claude-code", "sonnet", "anthropic"),
            ("codex", "gpt-5", "openai"),
            ("antigravity-cli", "gemini-3", "google"),
        ];
        fixture.settings.crew = crew
            .iter()
            .map(|(agent_id, model_id, _)| ModelRef::new(*agent_id, Some((*model_id).to_owned())))
            .collect();
        for (agent_id, model_id, provider) in crew {
            fixture.registry.add(model(agent_id, model_id, provider));
        }
        fixture
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    pub fn planner(&self) -> Planner {
        Planner::new(
            Arc::new(self.settings.clone()),
            self.registry.clone(),
            self.plan_usage.clone(),
            self.health.clone(),
            self.records.clone(),
        )
    }

    pub fn limits(&self) -> Limits {
        Limits::new(
            Arc::new(self.settings.clone()),
            self.plan_usage.clone(),
            self.health.clone(),
            Arc::new(self.planner()),
        )
        .with_registry(self.registry.clone())
    }

    /// [`Fixture::limits`] with `workers` known to its planner too, as the services give them.
    pub fn limits_with_workers(&self, workers: Arc<dyn RunnerDirectory>) -> Limits {
        Limits::new(
            Arc::new(self.settings.clone()),
            self.plan_usage.clone(),
            self.health.clone(),
            Arc::new(self.planner().with_workers(workers.clone())),
        )
        .with_registry(self.registry.clone())
        .with_workers(workers)
    }

    /// The worker of [`TASK`] stopped on `stopped`.
    pub fn query(&self, stopped: &str) -> CandidateQuery {
        CandidateQuery {
            task: task(TASK),
            role: RoleName::Worker,
            stopped: worker(stopped),
            exclude_providers: Vec::new(),
            fallback: Vec::new(),
            all_valid: false,
        }
    }
}

pub fn worker(text: &str) -> ModelRef {
    ModelRef::from_str(text).expect("a valid model")
}

pub fn agent_id(text: &str) -> AgentId {
    AgentId(text.to_owned())
}

pub fn entry(text: &str) -> FallbackEntry {
    FallbackEntry::Model(worker(text))
}

pub fn run() -> RunId {
    RunId::new(RUN).expect("a valid run id")
}

/// A subscription model billed by plan usage.
pub fn model(agent_id: &str, model: &str, provider: &str) -> ModelEntry {
    ModelEntry {
        id: ModelRef::new(agent_id, Some(model.to_owned())),
        provider: provider.to_owned(),
        kind: RegistryKind::Cli,
        access: ModelAccess::Subscription,
        tools: true,
        cost: CostModel {
            unit: CostUnit::PlanUsage,
            input_per_mtok: None,
            output_per_mtok: None,
            cached_input_per_mtok: None,
        },
        strengths: Vec::new(),
        good_for: Vec::new(),
        context_window: None,
        efforts: Vec::new(),
        aliases: Vec::new(),
        retired: false,
        successor: None,
        discovered: false,
        cli_default: false,
        default_effort: None,
        launch: None,
        tier: None,
        name: None,
        description: None,
        window: None,
        ram_bytes: None,
        thinking: None,
        variants: None,
        listed_cost: false,
    }
}

/// A model billed to an API key, at `usd_per_mtok` for input and output alike.
pub fn api_model(agent_id: &str, model_id: &str, provider: &str, usd_per_mtok: f64) -> ModelEntry {
    let mut entry = model(agent_id, model_id, provider);
    entry.kind = RegistryKind::Endpoint;
    entry.access = ModelAccess::ApiKey;
    entry.cost = CostModel {
        unit: CostUnit::Usd,
        input_per_mtok: Some(usd_per_mtok),
        output_per_mtok: Some(usd_per_mtok),
        cached_input_per_mtok: None,
    };
    entry
}

pub fn plan_usage(agent_id: &str, status: PlanUsageStatus, used: Option<f64>) -> PlanUsageReading {
    PlanUsageReading {
        agent_id: AgentId(agent_id.to_owned()),
        status,
        used,
        resets_at: None,
        window: None,
        source: PlanUsageSource::UsageProbe,
        observed_at: OffsetDateTime::now_utc(),
        detail: None,
        wrap_up: false,
    }
}

pub fn task(id: &str) -> TaskSpec {
    TaskSpec {
        id: TaskId::new(id).expect("a valid task id"),
        title: "Middleware".to_owned(),
        task: "Add the rate-limit middleware.".to_owned(),
        kind: TaskKind::Build,
        owned_paths: vec!["src/middleware/**".to_owned()],
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: vec!["C1".to_owned()],
        worker: Some(worker("claude-code/opus")),
        reviewers: Vec::new(),
        limit: TaskLimit::default(),
        predictions: None,
        item: None,
        way: None,
    }
}
