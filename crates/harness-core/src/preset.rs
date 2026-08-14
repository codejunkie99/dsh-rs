//! Preset-scope ownership.
//!
//! This maps the upstream `@deepseek-ai/dsh-agent-presets` `AgentPresets`
//! roster (`packages/preset/agent-presets/src/index.ts`) onto the Rust skill
//! scope primitive. Each preset/harness id owns one standing `SkillScope`
//! (upstream `StandingMount.key`), and every session/agent joins it by binding
//! its own scope key to that standing key. The roster also holds the one
//! `ScopeParentBinding` per agent (`AgentPresets.bindings`), which is the only
//! authority allowed to re-link an agent to another preset later.

use crate::skills::{ScopeParentBinding, SkillProviderRegistration, SkillScope};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// Registers one preset's standing-scope provider contribution.
///
/// Upstream evidence: `mountPreset` (in `packages/preset/agent-presets/src/mount.ts`)
/// composes a preset's `cordis.yml` under its standing scope, and plugins such
/// as `@deepseek-ai/dsh-skill-filesystem` call `ctx.skills.registerProvider(...)`
/// inside that composition (`packages/skill/skill-filesystem/src/index.ts:132`).
/// The factory maps that to `SkillRegistry::register_provider_for_scope_with_control`,
/// receiving the preset id and the freshly-minted standing scope.
pub type StandingProviderFactory =
    Arc<dyn Fn(&str, &SkillScope) -> Result<SkillProviderRegistration> + Send + Sync>;

/// One preset's standing scope key, shared by every session joined to it.
#[derive(Debug, Clone)]
struct StandingMount {
    preset_id: String,
    key: SkillScope,
}

/// Owns per-preset standing scope keys and per-session parent bindings.
///
/// Upstream evidence:
/// - `AgentPresets.standing: Map<string, Promise<StandingMount>>`
/// - `AgentPresets.bindings: WeakMap<ScopeKey, ScopeParentBinding>`
/// - `mount()`, `composeFrom()`, `composedPreset()`, `recompose()`,
///   `standingKeyFor()`, and the private `ensureStanding()`.
pub struct PresetRoster {
    default_id: String,
    standing: HashMap<String, StandingMount>,
    bindings: HashMap<SkillScope, ScopeParentBinding>,
    /// Per-preset provider registrations, retained so each standing mount's
    /// providers live exactly as long as the roster and unwind with it.
    standing_providers: HashMap<String, SkillProviderRegistration>,
    provider_factory: Option<StandingProviderFactory>,
}

impl PresetRoster {
    pub fn new(default_id: impl Into<String>) -> Self {
        Self {
            default_id: default_id.into(),
            standing: HashMap::new(),
            bindings: HashMap::new(),
            standing_providers: HashMap::new(),
            provider_factory: None,
        }
    }

    /// Construct with a standing-provider factory, so every preset's standing
    /// scope mounts its provider contribution as soon as the key is minted.
    pub fn with_standing_provider_factory(
        default_id: impl Into<String>,
        factory: StandingProviderFactory,
    ) -> Self {
        let mut roster = Self::new(default_id);
        roster.provider_factory = Some(factory);
        roster
    }

    pub fn default_id(&self) -> &str {
        &self.default_id
    }

    /// The standing scope key of one preset, minted once and reused.
    pub fn standing_key_for(&mut self, preset_id: Option<&str>) -> Result<SkillScope> {
        let preset_id = preset_id.unwrap_or(&self.default_id).to_string();
        self.ensure_standing(&preset_id)
    }

    /// Compose one session/agent from a preset: ensure the standing key, then
    /// parent the agent's own scope key to it (a first bind).
    pub fn mount(&mut self, agent: SkillScope, preset_id: Option<&str>) -> Result<()> {
        let preset_id = preset_id.unwrap_or(&self.default_id).to_string();
        let standing_key = self.ensure_standing(&preset_id)?;
        let binding = agent.bind_parent(standing_key)?;
        self.bindings.insert(agent, binding);
        Ok(())
    }

    /// Re-link one session/agent to a different preset's standing composition.
    /// Mirrors upstream `recompose`: a first-time join binds, otherwise the
    /// retained binding re-links.
    pub fn recompose(&mut self, agent: SkillScope, preset_id: &str) -> Result<()> {
        let standing_key = self.ensure_standing(preset_id)?;
        if let Some(binding) = self.bindings.get(&agent) {
            binding.rebind(standing_key)?;
        } else {
            let binding = agent.bind_parent(standing_key)?;
            self.bindings.insert(agent, binding);
        }
        Ok(())
    }

    /// The preset id one live session/agent runs on, read from its scope chain.
    pub fn composed_preset(&self, agent: &SkillScope) -> Option<String> {
        let parent = agent.parent()?;
        self.standing
            .values()
            .find(|mount| mount.key == parent)
            .map(|mount| mount.preset_id.clone())
    }

    fn ensure_standing(&mut self, preset_id: &str) -> Result<SkillScope> {
        if let Some(mount) = self.standing.get(preset_id) {
            return Ok(mount.key.clone());
        }
        let key = SkillScope::new();
        if let Some(factory) = &self.provider_factory {
            let registration = factory(preset_id, &key)?;
            self.standing_providers
                .insert(preset_id.to_string(), registration);
        }
        self.standing.insert(
            preset_id.to_string(),
            StandingMount {
                preset_id: preset_id.to_string(),
                key: key.clone(),
            },
        );
        Ok(key)
    }
}

impl Default for PresetRoster {
    fn default() -> Self {
        Self::new("standard")
    }
}

impl std::fmt::Debug for PresetRoster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PresetRoster")
            .field("default_id", &self.default_id)
            .field("standing", &self.standing)
            .field("bindings", &self.bindings.len())
            .field("standing_providers", &self.standing_providers.len())
            .field("provider_factory", &self.provider_factory.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{
        FileSystemSkillProvider, SkillFileSystemConfig, SkillRegistry, SkillViewOptions,
        SkillWatchConfig,
    };
    use std::path::Path;

    fn write_skill(root: &Path, name: &str, description: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n\nBody.\n"),
        )
        .unwrap();
    }

    #[test]
    fn sessions_on_the_same_preset_share_a_standing_key() {
        let mut roster = PresetRoster::new("standard");
        let a = SkillScope::new();
        let b = SkillScope::new();
        roster.mount(a.clone(), Some("standard")).unwrap();
        roster.mount(b.clone(), Some("standard")).unwrap();

        assert_eq!(roster.composed_preset(&a).as_deref(), Some("standard"));
        assert_eq!(roster.composed_preset(&b).as_deref(), Some("standard"));
        assert_eq!(a.parent(), b.parent());
    }

    #[test]
    fn different_presets_have_distinct_standing_keys() {
        let mut roster = PresetRoster::new("standard");
        let a = SkillScope::new();
        let b = SkillScope::new();
        roster.mount(a.clone(), Some("standard")).unwrap();
        roster.mount(b.clone(), Some("research")).unwrap();

        assert_ne!(a.parent(), b.parent());
        assert_eq!(roster.composed_preset(&b).as_deref(), Some("research"));
    }

    #[test]
    fn mount_without_a_preset_uses_the_default() {
        let mut roster = PresetRoster::new("research");
        let a = SkillScope::new();
        roster.mount(a.clone(), None).unwrap();
        assert_eq!(roster.composed_preset(&a).as_deref(), Some("research"));
    }

    #[test]
    fn recompose_relinks_an_existing_agent_to_a_new_standing_key() {
        let mut roster = PresetRoster::new("standard");
        let a = SkillScope::new();
        roster.mount(a.clone(), Some("standard")).unwrap();
        let original_parent = a.parent().unwrap();

        roster.recompose(a.clone(), "research").unwrap();

        assert_eq!(roster.composed_preset(&a).as_deref(), Some("research"));
        assert_ne!(a.parent(), Some(original_parent));
    }

    #[test]
    fn recompose_on_an_unjoined_agent_is_a_first_bind() {
        let mut roster = PresetRoster::new("standard");
        let a = SkillScope::new();
        roster.recompose(a.clone(), "research").unwrap();
        assert_eq!(roster.composed_preset(&a).as_deref(), Some("research"));
    }

    #[test]
    fn recompose_shares_the_target_preset_standing_key() {
        let mut roster = PresetRoster::new("standard");
        let a = SkillScope::new();
        let b = SkillScope::new();
        roster.mount(a.clone(), Some("standard")).unwrap();
        roster.mount(b.clone(), Some("research")).unwrap();

        roster.recompose(a.clone(), "research").unwrap();

        assert_eq!(a.parent(), b.parent());
    }

    #[test]
    fn standing_key_for_is_stable_per_preset() {
        let mut roster = PresetRoster::new("standard");
        let first = roster.standing_key_for(Some("research")).unwrap();
        let second = roster.standing_key_for(Some("research")).unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn standing_preset_provider_is_scoped_per_preset() {
        let home = tempfile::tempdir().unwrap();
        let presets = tempfile::tempdir().unwrap();
        write_skill(
            &presets.path().join("research"),
            "research-only",
            "Research skill",
        );
        write_skill(
            &presets.path().join("standard"),
            "standard-only",
            "Standard skill",
        );

        let registry = Arc::new(SkillRegistry::new());
        let registry_for_factory = registry.clone();
        let presets_root = presets.path().to_path_buf();
        let home_root = home.path().to_path_buf();
        let factory: StandingProviderFactory =
            Arc::new(move |preset_id: &str, scope: &SkillScope| {
                let skill_dir = presets_root.join(preset_id);
                registry_for_factory.register_provider_for_scope_with_control(
                    scope.clone(),
                    |control| {
                        let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                            include_default_roots: false,
                            dsh_home: home_root.join(".dsh"),
                            agents_home: home_root.join(".agents"),
                            custom_skill_dirs: vec![skill_dir.clone()],
                            bundled_skill_dir: None,
                        })?
                        .with_watching(control.clone(), SkillWatchConfig::default());
                        Ok(Arc::new(provider))
                    },
                )
            });

        let mut roster = PresetRoster::with_standing_provider_factory("standard", factory);
        let research_agent = SkillScope::new();
        let standard_agent = SkillScope::new();
        roster
            .mount(research_agent.clone(), Some("research"))
            .unwrap();
        roster
            .mount(standard_agent.clone(), Some("standard"))
            .unwrap();

        let research = registry
            .list(&SkillViewOptions {
                cwd: None,
                scope: Some(research_agent.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        let standard = registry
            .list(&SkillViewOptions {
                cwd: None,
                scope: Some(standard_agent.clone()),
                ..Default::default()
            })
            .await
            .unwrap();

        let research_names: Vec<_> = research.iter().map(|skill| skill.name.clone()).collect();
        let standard_names: Vec<_> = standard.iter().map(|skill| skill.name.clone()).collect();
        assert!(research_names.iter().any(|name| name == "research-only"));
        assert!(!research_names.iter().any(|name| name == "standard-only"));
        assert!(standard_names.iter().any(|name| name == "standard-only"));
        assert!(!standard_names.iter().any(|name| name == "research-only"));
    }

    #[tokio::test]
    async fn standing_preset_provider_unwinds_with_the_roster() {
        let home = tempfile::tempdir().unwrap();
        let presets = tempfile::tempdir().unwrap();
        write_skill(&presets.path().join("research"), "only", "Only skill");

        let registry = Arc::new(SkillRegistry::new());
        let registry_for_factory = registry.clone();
        let presets_root = presets.path().to_path_buf();
        let home_root = home.path().to_path_buf();
        let factory: StandingProviderFactory =
            Arc::new(move |_preset_id: &str, scope: &SkillScope| {
                let skill_dir = presets_root.join("research");
                registry_for_factory.register_provider_for_scope_with_control(
                    scope.clone(),
                    |control| {
                        let provider = FileSystemSkillProvider::new(SkillFileSystemConfig {
                            include_default_roots: false,
                            dsh_home: home_root.join(".dsh"),
                            agents_home: home_root.join(".agents"),
                            custom_skill_dirs: vec![skill_dir.clone()],
                            bundled_skill_dir: None,
                        })?
                        .with_watching(control.clone(), SkillWatchConfig::default());
                        Ok(Arc::new(provider))
                    },
                )
            });

        let mut roster = PresetRoster::with_standing_provider_factory("standard", factory);
        let agent = SkillScope::new();
        roster.mount(agent.clone(), Some("research")).unwrap();

        let before = registry
            .list(&SkillViewOptions {
                cwd: None,
                scope: Some(agent.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(before.iter().any(|skill| skill.name == "only"));

        // Dropping the roster drops the per-preset registration, which
        // unregisters the provider from the standing scope's layer.
        drop(roster);

        let after = registry
            .list(&SkillViewOptions {
                cwd: None,
                scope: Some(agent.clone()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!after.iter().any(|skill| skill.name == "only"));
    }
}
