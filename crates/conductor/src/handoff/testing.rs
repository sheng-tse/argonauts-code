//! Shared test fixtures for the planner and the limits service: settings you can change, fake
//! boards, a registry, and track records.

use std::str::FromStr;
use std::sync::Arc;

use argonauts_core::events::card::{CardBudget, CardKind, CardSpec};
use argonauts_core::limits::{CandidateQuery, QuotaReading, QuotaSource, QuotaStatus};
use argonauts_core::names::{RoleName, VendorId, WorkerRef};
use argonauts_core::registry::{CostModel, CostUnit, ModelAccess, ModelEntry, RegistryKind};
use argonauts_core::settings::Settings;
use argonauts_core::settings::handoff::FallbackEntry;
use argonauts_core::{CardId, RunId};
use argonauts_testkit::services::{
    MemoryHealthBoard, MemoryQuotaBoard, StaticRegistry, StaticTrackRecords,
};
use time::OffsetDateTime;

use super::Planner;
use crate::recovery::Limits;

pub const RUN: &str = "run-01";
pub const CARD: &str = "card-003";

pub struct Fixture {
    pub settings: Settings,
    pub registry: Arc<StaticRegistry>,
    pub quota: Arc<MemoryQuotaBoard>,
    pub health: Arc<MemoryHealthBoard>,
    pub records: Arc<StaticTrackRecords>,
}

impl Fixture {
    /// The default settings, whose crew is `claude-code`, `codex`, and `antigravity-cli`.
    pub fn new() -> Self {
        Self {
            settings: Settings::default(),
            registry: Arc::default(),
            quota: Arc::default(),
            health: Arc::default(),
            records: Arc::default(),
        }
    }

    /// A crew of named models in three families: `claude-code/opus` and `claude-code/sonnet`
    /// (claude), `codex/gpt-5` (gpt), and `antigravity-cli/gemini-3` (gemini).
    pub fn with_models() -> Self {
        let mut fixture = Self::new();
        let crew = [
            ("claude-code", "opus", "claude"),
            ("claude-code", "sonnet", "claude"),
            ("codex", "gpt-5", "gpt"),
            ("antigravity-cli", "gemini-3", "gemini"),
        ];
        fixture.settings.crew = crew
            .iter()
            .map(|(vendor, model_id, _)| WorkerRef::new(*vendor, Some((*model_id).to_owned())))
            .collect();
        for (vendor, model_id, family) in crew {
            fixture.registry.add(model(vendor, model_id, family));
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
            self.quota.clone(),
            self.health.clone(),
            self.records.clone(),
        )
    }

    pub fn limits(&self) -> Limits {
        Limits::new(
            Arc::new(self.settings.clone()),
            self.quota.clone(),
            self.health.clone(),
            Arc::new(self.planner()),
        )
        .with_registry(self.registry.clone())
    }

    /// The owner of [`CARD`] stopped on `stopped`.
    pub fn query(&self, stopped: &str) -> CandidateQuery {
        CandidateQuery {
            card: card(CARD),
            role: RoleName::Owner,
            stopped: worker(stopped),
            exclude_families: Vec::new(),
            fallback: Vec::new(),
            all_valid: false,
        }
    }
}

pub fn worker(text: &str) -> WorkerRef {
    WorkerRef::from_str(text).expect("a valid worker")
}

pub fn vendor(text: &str) -> VendorId {
    VendorId(text.to_owned())
}

pub fn entry(text: &str) -> FallbackEntry {
    FallbackEntry::Worker(worker(text))
}

pub fn run() -> RunId {
    RunId::new(RUN).expect("a valid run id")
}

/// A subscription model billed by quota.
pub fn model(vendor: &str, model: &str, family: &str) -> ModelEntry {
    ModelEntry {
        id: WorkerRef::new(vendor, Some(model.to_owned())),
        family: family.to_owned(),
        kind: RegistryKind::Harness,
        access: ModelAccess::Subscription,
        tools: true,
        cost: CostModel {
            unit: CostUnit::Quota,
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
    }
}

/// A model billed to an API key, at `usd_per_mtok` for input and output alike.
pub fn api_model(vendor: &str, model_id: &str, family: &str, usd_per_mtok: f64) -> ModelEntry {
    let mut entry = model(vendor, model_id, family);
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

pub fn quota(vendor: &str, status: QuotaStatus, used: Option<f64>) -> QuotaReading {
    QuotaReading {
        vendor: VendorId(vendor.to_owned()),
        status,
        used,
        resets_at: None,
        window: None,
        source: QuotaSource::UsageProbe,
        observed_at: OffsetDateTime::now_utc(),
        detail: None,
        wrap_up: false,
    }
}

pub fn card(id: &str) -> CardSpec {
    CardSpec {
        id: CardId::new(id).expect("a valid card id"),
        title: "Middleware".to_owned(),
        task: "Add the rate-limit middleware.".to_owned(),
        kind: CardKind::Build,
        owned_paths: vec!["src/middleware/**".to_owned()],
        read_only_paths: Vec::new(),
        forbidden_paths: Vec::new(),
        hidden_paths: Vec::new(),
        depends_on: Vec::new(),
        criteria: vec!["C1".to_owned()],
        owner: Some(worker("claude-code/opus")),
        reviewers: Vec::new(),
        budget: CardBudget::default(),
        predictions: None,
        item: None,
    }
}
