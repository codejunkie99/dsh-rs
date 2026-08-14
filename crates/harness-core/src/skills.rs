use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::broadcast;

pub const BUNDLED_SKILL_RANK: u16 = 600;
const DEFAULT_COLLECT_CACHE_MAX_ENTRIES: usize = 128;
const MAX_COLLECT_ATTEMPTS: u8 = 2;
const RUNTIME_SKILL_RANK: u16 = 250;
const PROJECT_DSH_RANK: u16 = 100;
const PROJECT_AGENTS_RANK: u16 = 200;
const CUSTOM_RANK: u16 = 300;
const USER_DSH_RANK: u16 = 400;
const USER_AGENTS_RANK: u16 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInvocationPolicy {
    pub model_invocable: bool,
    pub user_invocable: bool,
}

impl Default for SkillInvocationPolicy {
    fn default() -> Self {
        Self {
            model_invocable: true,
            user_invocable: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillResourceBase {
    Directory { path: PathBuf },
    Url { url: String },
    Opaque { description: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCatalogEntry {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSlashEntry {
    pub name: String,
    pub description: String,
    pub model_invocable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
    pub rank: u16,
    pub locator: serde_json::Value,
    pub path: Option<PathBuf>,
    pub metadata: Option<serde_json::Value>,
}

impl SkillCandidate {
    fn summary(&self) -> SkillSummary {
        SkillSummary {
            name: self.name.clone(),
            description: self.description.clone(),
            when_to_use: self.when_to_use.clone(),
            invocation: self.invocation,
            source: self.source.clone(),
            provider: self.provider.clone(),
            resource_base: self.resource_base.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub invocation: SkillInvocationPolicy,
    pub source: String,
    pub provider: String,
    pub resource_base: Option<SkillResourceBase>,
    pub path: Option<PathBuf>,
    pub metadata: Option<serde_json::Value>,
    pub content: String,
}

#[derive(Debug, Clone, Default)]
pub struct SkillLookupOptions {
    pub cwd: Option<PathBuf>,
    /// Abort discovery or loading work for the current caller, mirroring
    /// upstream `SkillLookupOptions.signal`
    /// (`packages/skill/skill/src/index.ts:108`).
    pub signal: Option<AbortSignal>,
}

/// Opaque, identity-compared scope key.
///
/// This maps the upstream `@deepseek-ai/dsh-scope` `ScopeKey` (`object`) and
/// `scopeParents` relation onto Rust: a key is a plain identity, and its single
/// enclosing link is held here so a registry can resolve the chain
/// nearest-first. The parent link is what makes "preset recomposition" work:
/// an agent key is re-parented to a different preset's standing key, and a
/// chain-bearing read then resolves the new preset's skill layer.
#[derive(Debug, Clone)]
pub struct SkillScope {
    inner: Arc<SkillScopeInner>,
}

#[derive(Debug)]
struct SkillScopeInner {
    id: u64,
    parent: parking_lot::RwLock<Option<SkillScope>>,
}

static NEXT_SCOPE_ID: AtomicU64 = AtomicU64::new(1);

impl SkillScope {
    pub fn new() -> Self {
        let id = NEXT_SCOPE_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            inner: Arc::new(SkillScopeInner {
                id,
                parent: parking_lot::RwLock::new(None),
            }),
        }
    }

    pub(crate) fn id(&self) -> u64 {
        self.inner.id
    }

    pub fn parent(&self) -> Option<SkillScope> {
        self.inner.parent.read().clone()
    }

    /// The chain from this key to its root ancestor, nearest-first, matching
    /// upstream `scopeChainOf`.
    pub fn chain(&self) -> Vec<SkillScope> {
        let mut chain = Vec::new();
        let mut cursor = Some(self.clone());
        while let Some(key) = cursor {
            cursor = key.parent();
            chain.push(key);
        }
        chain
    }

    /// Bind `parent` as this key's enclosing scope, once. Returns the only
    /// handle allowed to re-link the key later (the preset recomposition seam).
    pub fn bind_parent(&self, parent: SkillScope) -> Result<ScopeParentBinding> {
        {
            let mut link = self.inner.parent.write();
            if link.is_some() {
                bail!("skill scope key is already bound to a parent; re-linking requires the binding returned by the original bind");
            }
            assert_no_scope_cycle(self, &parent)?;
            *link = Some(parent);
        }
        Ok(ScopeParentBinding { key: self.clone() })
    }
}

impl PartialEq for SkillScope {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for SkillScope {}

impl std::hash::Hash for SkillScope {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state);
    }
}

impl Default for SkillScope {
    fn default() -> Self {
        Self::new()
    }
}

/// The privileged handle to move one scope key's parent link, mirroring
/// upstream `ScopeParentBinding.rebind`.
pub struct ScopeParentBinding {
    key: SkillScope,
}

impl ScopeParentBinding {
    pub fn rebind(&self, parent: SkillScope) -> Result<()> {
        assert_no_scope_cycle(&self.key, &parent)?;
        *self.key.inner.parent.write() = Some(parent);
        Ok(())
    }
}

fn assert_no_scope_cycle(key: &SkillScope, parent: &SkillScope) -> Result<()> {
    let mut cursor = Some(parent.clone());
    while let Some(current) = cursor {
        if current == *key {
            bail!("skill scope parent link would form a cycle");
        }
        cursor = current.parent();
    }
    Ok(())
}

/// Registry read options: provider lookup context plus the viewing scope.
///
/// This mirrors upstream `SkillViewOptions` (`SkillLookupOptions` + `scope`).
/// Providers receive only the borrowed `SkillLookupOptions` subset; the
/// registry consumes `scope` to select the viewing agent's layers.
#[derive(Debug, Clone, Default)]
pub struct SkillViewOptions {
    pub cwd: Option<PathBuf>,
    pub signal: Option<AbortSignal>,
    pub scope: Option<SkillScope>,
}

impl SkillViewOptions {
    pub fn lookup(&self) -> SkillLookupOptions {
        SkillLookupOptions {
            cwd: self.cwd.clone(),
            signal: self.signal.clone(),
        }
    }
}

/// Provider candidates plus whether the current discovery is authoritative.
///
/// A provider may return this observation when it has usable candidates but its
/// discovery could not complete (for example, a watcher failed to start). The
/// registry only caches completed observations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillProviderObservation {
    pub candidates: Vec<SkillCandidate>,
    pub complete: bool,
}

impl SkillProviderObservation {
    pub fn complete(candidates: Vec<SkillCandidate>) -> Self {
        Self {
            candidates,
            complete: true,
        }
    }
}

/// One catalog observation plus whether discovery completed within a stable
/// registry revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalogSnapshot {
    pub skills: Vec<SkillSummary>,
    pub complete: bool,
}

#[async_trait]
pub trait SkillProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn list(&self, options: &SkillLookupOptions) -> Result<SkillProviderObservation>;
    async fn get(
        &self,
        candidate: &SkillCandidate,
        options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>>;
}

#[derive(Debug, Clone)]
struct RuntimeRegistration {
    definition: SkillDefinition,
    order: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillRegistryEventKind {
    ProviderRegistered,
    ProviderUnregistered,
    ProviderInvalidated,
    RuntimeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRegistryEvent {
    pub revision: u64,
    pub provider: Option<String>,
    pub kind: SkillRegistryEventKind,
}

struct ProviderRegistration {
    provider: Arc<dyn SkillProvider>,
    token: Option<u64>,
}

/// One scope's complete skill-registry contribution, mirroring upstream
/// `SkillLayer`: providers registered through contexts carrying this scope,
/// plus runtime skills registered through those same contexts.
struct SkillLayer {
    providers: Vec<ProviderRegistration>,
    runtime: HashMap<String, RuntimeRegistration>,
    next_runtime_order: usize,
}

impl SkillLayer {
    fn new() -> Self {
        Self {
            providers: Vec::new(),
            runtime: HashMap::new(),
            next_runtime_order: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.providers.is_empty() && self.runtime.is_empty()
    }
}

struct SkillRegistryState {
    global: SkillLayer,
    scoped: HashMap<u64, SkillLayer>,
    next_provider_token: u64,
}

/// A winning candidate plus the provider and layer that produced it, mirroring
/// upstream `IndexedCandidate` so `get` loads from the exact registration.
#[derive(Clone)]
struct IndexedCandidate {
    candidate: SkillCandidate,
    provider: Option<Arc<dyn SkillProvider>>,
    layer_id: Option<u64>,
}

/// The collect cache is keyed by lookup cwd, the viewing scope chain, and the
/// revision observed when collection started. The chain is part of the key
/// rather than assumed stable: a blank-session recompose re-parents an existing
/// scope without touching the registry, and only a chain-bearing key makes the
/// next read see the new preset.
///
/// `IndexMap` is insertion-ordered, matching upstream's JavaScript `Map`
/// (`packages/skill/skill/src/index.ts`, `collectCache`): re-inserting an
/// existing key keeps its original position, and `keys().next()` yields the
/// oldest entry for capacity eviction.
type CollectCacheKey = (Option<PathBuf>, Vec<u64>, u64);
type CollectCache = IndexMap<CollectCacheKey, Vec<IndexedCandidate>>;

struct SkillRegistryInner {
    state: parking_lot::RwLock<SkillRegistryState>,
    revision: AtomicU64,
    events: broadcast::Sender<SkillRegistryEvent>,
    collect_cache: parking_lot::Mutex<CollectCache>,
    collect_cache_max_entries: usize,
}

#[derive(Clone)]
pub struct SkillRegistry {
    inner: Arc<SkillRegistryInner>,
}

/// The merged winner list for one collection plus whether it may be cached.
struct CollectResult {
    entries: Vec<IndexedCandidate>,
    cacheable: bool,
}

/// A minimal Rust analogue of upstream `AbortSignal` for provider lifecycle.
///
/// Upstream `SkillProviderControl.signal` is an `AbortSignal`
/// (`packages/skill/skill/src/index.ts:273`) that a provider observes via
/// `addEventListener('abort', ...)` or `waitWithAbort` instead of busy-polling a
/// flag. Rust has no built-in abort signal, so this holds an atomic cancelled
/// flag plus a `Notify` waker: `is_cancelled` is a cheap synchronous probe, and
/// `cancelled` is awaitable and resolves immediately when already aborted.
#[derive(Clone)]
pub struct AbortSignal {
    inner: Arc<AbortState>,
}

impl std::fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AbortSignal")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

struct AbortState {
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AbortState {
                cancelled: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    pub fn abort(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    /// Whether the owning registration has been disposed.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    /// Resolve when the owning registration is disposed, immediately if already
    /// aborted. This is the abort-observable surface: a provider awaiting this
    /// is woken by disposal rather than re-checking a flag on a timer.
    pub async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("skill lookup aborted")]
struct SkillLookupAborted;

/// Race provider discovery/loading against the caller's abort signal, mirroring
/// upstream `waitWithAbort` (`packages/skill/skill/src/index.ts:819`): an
/// uncooperative provider is abandoned once the signal fires.
async fn wait_with_abort<T>(
    future: impl std::future::Future<Output = Result<T>>,
    signal: Option<AbortSignal>,
) -> Result<T> {
    let Some(signal) = signal else {
        return future.await;
    };
    if signal.is_cancelled() {
        return Err(SkillLookupAborted.into());
    }
    tokio::pin!(future);
    tokio::select! {
        result = &mut future => result,
        _ = signal.cancelled() => Err(SkillLookupAborted.into()),
    }
}

/// Throw the abort error for an already-aborted lookup, mirroring upstream
/// `throwIfAborted` (`packages/skill/skill/src/index.ts:846`). This is checked
/// at `collect` entry, after a fresh collection, and after `collect` returns in
/// `get`, so an abort is honored even on cache hits and runtime-only layers
/// where no provider call would otherwise observe the signal.
fn throw_if_aborted(signal: Option<AbortSignal>) -> Result<()> {
    if signal.is_some_and(|signal| signal.is_cancelled()) {
        return Err(SkillLookupAborted.into());
    }
    Ok(())
}

pub struct SkillProviderRegistration {
    registry: Weak<SkillRegistryInner>,
    provider: String,
    token: u64,
    scope_id: Option<u64>,
    active: bool,
    signal: AbortSignal,
}

impl SkillProviderRegistration {
    pub fn provider_name(&self) -> &str {
        &self.provider
    }

    pub fn invalidate(&self) -> Result<()> {
        let Some(inner) = self.registry.upgrade() else {
            bail!("skill registry was dropped");
        };
        SkillRegistry { inner }.invalidate_provider_token(&self.provider, self.token, self.scope_id)
    }

    pub fn close(mut self) -> bool {
        self.active = false;
        self.signal.abort();
        let Some(inner) = self.registry.upgrade() else {
            return false;
        };
        SkillRegistry { inner }.unregister_provider(&self.provider, self.token, self.scope_id)
    }
}

impl Drop for SkillProviderRegistration {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.signal.abort();
        let Some(inner) = self.registry.upgrade() else {
            return;
        };
        SkillRegistry { inner }.unregister_provider(&self.provider, self.token, self.scope_id);
    }
}

/// Lifecycle and invalidation capability borrowed by one provider registration.
///
/// A filesystem-backed provider receives this control through the scoped
/// factory registration and uses it to invalidate the registry when its
/// watched roots change, and to observe disposal of the registration.
#[derive(Clone)]
pub struct SkillProviderControl {
    registry: Weak<SkillRegistryInner>,
    token: u64,
    scope_id: Option<u64>,
    signal: AbortSignal,
}

impl SkillProviderControl {
    pub fn invalidate(&self) -> Result<()> {
        // Upstream `SkillProviderControl.invalidate` is a silent no-op once the
        // exact registration is disposed (`registration === undefined`) or its
        // layer entry no longer points at the same provider. Disposal sets the
        // shared cancellation flag, and a dropped registry is likewise silent.
        if self.signal.is_cancelled() {
            return Ok(());
        }
        let Some(inner) = self.registry.upgrade() else {
            return Ok(());
        };
        SkillRegistry { inner }.invalidate_token(self.token, self.scope_id)
    }

    pub fn is_cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }

    /// The abort-observable signal shared with the owning registration. It
    /// aborts when the exact provider registration is disposed, mirroring
    /// upstream `SkillProviderControl.signal`.
    pub fn signal(&self) -> &AbortSignal {
        &self.signal
    }
}

fn layer_mut(state: &mut SkillRegistryState, scope_id: Option<u64>) -> &mut SkillLayer {
    match scope_id {
        None => &mut state.global,
        Some(id) => state.scoped.entry(id).or_insert_with(SkillLayer::new),
    }
}

fn layer_ref(state: &SkillRegistryState, scope_id: Option<u64>) -> Option<&SkillLayer> {
    match scope_id {
        None => Some(&state.global),
        Some(id) => state.scoped.get(&id),
    }
}

fn layer_mut_existing(
    state: &mut SkillRegistryState,
    scope_id: Option<u64>,
) -> Option<&mut SkillLayer> {
    match scope_id {
        None => Some(&mut state.global),
        Some(id) => state.scoped.get_mut(&id),
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::with_collect_cache_max_entries(DEFAULT_COLLECT_CACHE_MAX_ENTRIES)
    }

    pub fn with_collect_cache_max_entries(collect_cache_max_entries: usize) -> Self {
        assert!(
            collect_cache_max_entries >= 1,
            "collectCacheMaxEntries must be a positive integer"
        );
        let (events, _) = broadcast::channel(32);
        Self {
            inner: Arc::new(SkillRegistryInner {
                state: parking_lot::RwLock::new(SkillRegistryState {
                    global: SkillLayer::new(),
                    scoped: HashMap::new(),
                    next_provider_token: 0,
                }),
                revision: AtomicU64::new(0),
                events,
                collect_cache: parking_lot::Mutex::new(IndexMap::new()),
                collect_cache_max_entries,
            }),
        }
    }

    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SkillRegistryEvent> {
        self.inner.events.subscribe()
    }

    pub fn register_provider(&self, provider: Arc<dyn SkillProvider>) -> Result<()> {
        self.insert_provider(None, provider, None).map(|_| ())
    }

    pub fn register_provider_scoped(
        &self,
        provider: Arc<dyn SkillProvider>,
    ) -> Result<SkillProviderRegistration> {
        let (name, token) = self.insert_provider(None, provider, Some(0))?;
        Ok(self.registration_for(None, name, token, AbortSignal::new()))
    }

    /// Register a provider into a specific scope's layer, mirroring upstream
    /// `registerProvider` filing into the calling context's scope. This is the
    /// per-agent skill-scope seam: a preset standing composition mounts its
    /// filesystem provider here, and reads carrying that scope (or a child
    /// scope parented to it) see the contribution.
    pub fn register_provider_for_scope(
        &self,
        scope: SkillScope,
        provider: Arc<dyn SkillProvider>,
    ) -> Result<SkillProviderRegistration> {
        let scope_id = scope.id();
        let (name, token) = self.insert_provider(Some(scope_id), provider, Some(0))?;
        Ok(self.registration_for(Some(scope_id), name, token, AbortSignal::new()))
    }

    /// Register a provider into its own scoped registration while handing it a
    /// lifecycle control it can use to invalidate the catalog and observe
    /// disposal. This is the filesystem watcher seam: the provider starts its
    /// watchers during construction and stops them when the control is
    /// cancelled.
    pub fn register_provider_scoped_with_control(
        &self,
        create: impl FnOnce(&SkillProviderControl) -> Result<Arc<dyn SkillProvider>>,
    ) -> Result<SkillProviderRegistration> {
        self.insert_provider_with_control(None, create)
    }

    /// The scope-aware form of [`Self::register_provider_scoped_with_control`],
    /// for a filesystem provider mounted by a preset's standing composition.
    pub fn register_provider_for_scope_with_control(
        &self,
        scope: SkillScope,
        create: impl FnOnce(&SkillProviderControl) -> Result<Arc<dyn SkillProvider>>,
    ) -> Result<SkillProviderRegistration> {
        self.insert_provider_with_control(Some(scope.id()), create)
    }

    fn registration_for(
        &self,
        scope_id: Option<u64>,
        name: String,
        token: u64,
        signal: AbortSignal,
    ) -> SkillProviderRegistration {
        SkillProviderRegistration {
            registry: Arc::downgrade(&self.inner),
            provider: name,
            token,
            scope_id,
            active: true,
            signal,
        }
    }

    fn reserve_token(&self) -> u64 {
        let mut state = self.inner.state.write();
        let token = state.next_provider_token;
        state.next_provider_token += 1;
        token
    }

    fn insert_provider(
        &self,
        scope_id: Option<u64>,
        provider: Arc<dyn SkillProvider>,
        with_handle: Option<u64>,
    ) -> Result<(String, u64)> {
        let name = provider.name().to_string();
        if name == "runtime" {
            bail!("skill provider named \"runtime\" is reserved");
        }
        let mut state = self.inner.state.write();
        let token = state.next_provider_token;
        state.next_provider_token += 1;
        let layer = layer_mut(&mut state, scope_id);
        if layer
            .providers
            .iter()
            .any(|existing| existing.provider.name() == name)
        {
            bail!("a skill provider named \"{name}\" is already registered in this scope");
        }
        layer.providers.push(ProviderRegistration {
            provider,
            token: with_handle.map(|_| token),
        });
        drop(state);
        self.publish(
            SkillRegistryEventKind::ProviderRegistered,
            Some(name.clone()),
        );
        Ok((name, token))
    }

    fn insert_provider_with_control(
        &self,
        scope_id: Option<u64>,
        create: impl FnOnce(&SkillProviderControl) -> Result<Arc<dyn SkillProvider>>,
    ) -> Result<SkillProviderRegistration> {
        let token = self.reserve_token();
        let signal = AbortSignal::new();
        let control = SkillProviderControl {
            registry: Arc::downgrade(&self.inner),
            token,
            scope_id,
            signal: signal.clone(),
        };
        let provider = create(&control)?;
        let name = provider.name().to_string();
        if name == "runtime" {
            bail!("skill provider named \"runtime\" is reserved");
        }
        {
            let mut state = self.inner.state.write();
            let layer = layer_mut(&mut state, scope_id);
            if layer
                .providers
                .iter()
                .any(|existing| existing.provider.name() == name)
            {
                bail!("a skill provider named \"{name}\" is already registered in this scope");
            }
            layer.providers.push(ProviderRegistration {
                provider,
                token: Some(token),
            });
        }
        self.publish(
            SkillRegistryEventKind::ProviderRegistered,
            Some(name.clone()),
        );
        Ok(self.registration_for(scope_id, name, token, signal))
    }

    pub fn register_runtime(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
        invocation: SkillInvocationPolicy,
    ) -> Result<()> {
        self.insert_runtime(None, name, description, content, invocation)
    }

    /// Register a runtime skill into a specific scope's layer, mirroring
    /// upstream `SkillRegistry.register` filing into the calling context's
    /// scope. Same-name runtime entries within one layer are first-wins: a
    /// duplicate logs a warning and is ignored, matching upstream.
    pub fn register_runtime_for_scope(
        &self,
        scope: SkillScope,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
        invocation: SkillInvocationPolicy,
    ) -> Result<()> {
        self.insert_runtime(Some(scope.id()), name, description, content, invocation)
    }

    fn insert_runtime(
        &self,
        scope_id: Option<u64>,
        name: impl Into<String>,
        description: impl Into<String>,
        content: impl Into<String>,
        invocation: SkillInvocationPolicy,
    ) -> Result<()> {
        let name = name.into();
        {
            let state = self.inner.state.read();
            if layer_ref(&state, scope_id).is_some_and(|layer| layer.runtime.contains_key(&name)) {
                eprintln!("runtime skill \"{name}\" ignored because it is already registered");
                return Ok(());
            }
        }
        let definition = SkillDefinition {
            name: name.clone(),
            description: description.into(),
            when_to_use: None,
            invocation,
            source: "runtime".into(),
            provider: "runtime".into(),
            resource_base: None,
            path: None,
            metadata: None,
            content: content.into(),
        };
        validate_definition(&definition)?;
        let mut state = self.inner.state.write();
        let layer = layer_mut(&mut state, scope_id);
        let order = layer.next_runtime_order;
        layer.next_runtime_order += 1;
        layer
            .runtime
            .insert(name, RuntimeRegistration { definition, order });
        drop(state);
        self.publish(
            SkillRegistryEventKind::RuntimeChanged,
            Some("runtime".into()),
        );
        Ok(())
    }

    pub fn invalidate_provider(&self, provider: &str) -> Result<()> {
        let exists = self
            .inner
            .state
            .read()
            .global
            .providers
            .iter()
            .any(|entry| entry.provider.name() == provider);
        if !exists {
            bail!("skill provider \"{provider}\" is not registered");
        }
        self.publish(
            SkillRegistryEventKind::ProviderInvalidated,
            Some(provider.to_string()),
        );
        Ok(())
    }

    fn invalidate_provider_token(
        &self,
        provider: &str,
        token: u64,
        scope_id: Option<u64>,
    ) -> Result<()> {
        let exists = {
            let state = self.inner.state.read();
            layer_ref(&state, scope_id).is_some_and(|layer| {
                layer
                    .providers
                    .iter()
                    .any(|entry| entry.provider.name() == provider && entry.token == Some(token))
            })
        };
        if !exists {
            bail!("skill provider registration for \"{provider}\" is no longer active");
        }
        self.publish(
            SkillRegistryEventKind::ProviderInvalidated,
            Some(provider.to_string()),
        );
        Ok(())
    }

    fn invalidate_token(&self, token: u64, scope_id: Option<u64>) -> Result<()> {
        let provider = {
            let state = self.inner.state.read();
            layer_ref(&state, scope_id)
                .and_then(|layer| {
                    layer
                        .providers
                        .iter()
                        .find(|entry| entry.token == Some(token))
                })
                .map(|entry| entry.provider.name().to_string())
        };
        let Some(provider) = provider else {
            // A registration that has been unregistered or replaced publishes
            // nothing and raises no error: this is only reachable from
            // `SkillProviderControl::invalidate`, whose upstream contract is a
            // silent no-op after disposal.
            return Ok(());
        };
        self.publish(SkillRegistryEventKind::ProviderInvalidated, Some(provider));
        Ok(())
    }

    fn unregister_provider(&self, provider: &str, token: u64, scope_id: Option<u64>) -> bool {
        let mut state = self.inner.state.write();
        let empty =
            {
                let Some(layer) = layer_mut_existing(&mut state, scope_id) else {
                    return false;
                };
                let Some(index) = layer.providers.iter().position(|entry| {
                    entry.provider.name() == provider && entry.token == Some(token)
                }) else {
                    return false;
                };
                layer.providers.remove(index);
                layer.is_empty()
            };
        if let Some(id) = scope_id {
            if empty {
                state.scoped.remove(&id);
            }
        }
        drop(state);
        self.publish(
            SkillRegistryEventKind::ProviderUnregistered,
            Some(provider.to_string()),
        );
        true
    }

    fn publish(&self, kind: SkillRegistryEventKind, provider: Option<String>) {
        let revision = self.inner.revision.fetch_add(1, Ordering::AcqRel) + 1;
        self.inner.collect_cache.lock().clear();
        let _ = self.inner.events.send(SkillRegistryEvent {
            revision,
            provider,
            kind,
        });
    }

    pub async fn snapshot(&self, options: &SkillViewOptions) -> Result<SkillCatalogSnapshot> {
        let collected = self.collect(options).await?;
        let mut skills: Vec<_> = collected
            .entries
            .into_iter()
            .map(|entry| entry.candidate.summary())
            .collect();
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(SkillCatalogSnapshot {
            skills,
            complete: collected.cacheable,
        })
    }

    pub async fn list(&self, options: &SkillViewOptions) -> Result<Vec<SkillSummary>> {
        Ok(self.snapshot(options).await?.skills)
    }

    pub async fn get(
        &self,
        name: &str,
        options: &SkillViewOptions,
    ) -> Result<Option<SkillDefinition>> {
        if !is_skill_name(name) {
            return Ok(None);
        }
        let collected = self.collect(options).await?;
        throw_if_aborted(options.signal.clone())?;
        let Some(entry) = collected
            .entries
            .into_iter()
            .find(|entry| entry.candidate.name == name)
        else {
            return Ok(None);
        };

        if let Some(provider) = &entry.provider {
            let lookup = options.lookup();
            let Some(definition) = wait_with_abort(
                provider.get(&entry.candidate, &lookup),
                lookup.signal.clone(),
            )
            .await?
            else {
                return Ok(None);
            };
            validate_definition(&definition)?;
            if definition.name != entry.candidate.name {
                self.invalidate_entry(&entry);
                return Ok(None);
            }
            return Ok(Some(definition));
        }

        // Runtime skill: the candidate carries no provider, and its body lives
        // in the exact layer the candidate was collected from.
        let definition = {
            let state = self.inner.state.read();
            layer_ref(&state, entry.layer_id)
                .and_then(|layer| layer.runtime.get(name))
                .map(|registration| registration.definition.clone())
        };
        Ok(definition)
    }

    fn invalidate_entry(&self, entry: &IndexedCandidate) {
        // Upstream invalidates a stale definition load only while the exact
        // provider registration that produced the entry is still live
        // (`entry.layer.providers.get(name)?.provider === entry.provider`).
        // Comparing by name would wrongly invalidate a replacement provider of
        // the same name. Runtime candidates carry no provider and never reach
        // this path (their definitions are keyed by name in the layer).
        let Some(entry_provider) = &entry.provider else {
            return;
        };
        let still_live = {
            let state = self.inner.state.read();
            layer_ref(&state, entry.layer_id).is_some_and(|layer| {
                layer
                    .providers
                    .iter()
                    .any(|registration| Arc::ptr_eq(&registration.provider, entry_provider))
            })
        };
        if still_live {
            self.publish(
                SkillRegistryEventKind::ProviderInvalidated,
                Some(entry.candidate.provider.clone()),
            );
        }
    }

    async fn collect(&self, options: &SkillViewOptions) -> Result<CollectResult> {
        throw_if_aborted(options.signal.clone())?;
        let mut attempt: u8 = 1;
        loop {
            let revision = self.revision();
            let chain = options
                .scope
                .as_ref()
                .map(|scope| scope.chain().into_iter().map(|key| key.id()).collect())
                .unwrap_or_default();
            let key = (options.cwd.clone(), chain, revision);
            if let Some(cached) = self.inner.collect_cache.lock().get(&key).cloned() {
                return Ok(CollectResult {
                    entries: cached,
                    cacheable: true,
                });
            }

            let result = self.collect_fresh(options).await?;
            throw_if_aborted(options.signal.clone())?;
            if revision != self.revision() {
                if attempt < MAX_COLLECT_ATTEMPTS {
                    attempt += 1;
                    continue;
                }
                return Ok(CollectResult {
                    entries: result.entries,
                    cacheable: false,
                });
            }
            if result.cacheable {
                let mut cache = self.inner.collect_cache.lock();
                cache.insert(key, result.entries.clone());
                if cache.len() > self.inner.collect_cache_max_entries {
                    if let Some(oldest) = cache.keys().next().cloned() {
                        cache.shift_remove(&oldest);
                    }
                }
            }
            return Ok(result);
        }
    }

    async fn collect_fresh(&self, options: &SkillViewOptions) -> Result<CollectResult> {
        let lookup = options.lookup();
        // Global first, then the scope chain farthest-ancestor-first and the
        // exact scope last, so the nearest layer's same-name entry replaces the
        // farther ones — the upstream `SkillRegistry` shadowing rule.
        let mut layer_ids = vec![None];
        if let Some(scope) = &options.scope {
            layer_ids.extend(scope.chain().into_iter().rev().map(|key| Some(key.id())));
        }

        let mut by_name: HashMap<String, IndexedCandidate> = HashMap::new();
        let mut cacheable = true;
        for layer_id in layer_ids {
            let collected = self.collect_layer(layer_id, &lookup).await?;
            if !collected.cacheable {
                cacheable = false;
            }
            for entry in collected.entries {
                by_name.insert(entry.candidate.name.clone(), entry);
            }
        }
        Ok(CollectResult {
            entries: by_name.into_values().collect(),
            cacheable,
        })
    }

    async fn collect_layer(
        &self,
        layer_id: Option<u64>,
        lookup: &SkillLookupOptions,
    ) -> Result<CollectResult> {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
        struct Indexed {
            rank: u16,
            provider_order: i64,
            local_order: usize,
        }
        type PendingCandidate = (Indexed, SkillCandidate, Option<Arc<dyn SkillProvider>>);

        let (providers, mut runtime): (Vec<_>, Vec<_>) = {
            let state = self.inner.state.read();
            let Some(layer) = layer_ref(&state, layer_id) else {
                return Ok(CollectResult {
                    entries: Vec::new(),
                    cacheable: true,
                });
            };
            (
                layer
                    .providers
                    .iter()
                    .map(|entry| entry.provider.clone())
                    .collect(),
                layer.runtime.values().cloned().collect(),
            )
        };
        let mut indexed: Vec<PendingCandidate> = Vec::new();
        let mut cacheable = true;
        runtime.sort_by(|left, right| {
            left.definition
                .name
                .cmp(&right.definition.name)
                .then(left.order.cmp(&right.order))
        });
        for (local_order, registration) in runtime.into_iter().enumerate() {
            let definition = registration.definition;
            let candidate = SkillCandidate {
                name: definition.name.clone(),
                description: definition.description.clone(),
                when_to_use: definition.when_to_use.clone(),
                invocation: definition.invocation,
                source: definition.source.clone(),
                provider: definition.provider.clone(),
                resource_base: definition.resource_base.clone(),
                rank: RUNTIME_SKILL_RANK,
                locator: serde_json::Value::Null,
                path: definition.path.clone(),
                metadata: definition.metadata.clone(),
            };
            indexed.push((
                Indexed {
                    rank: RUNTIME_SKILL_RANK,
                    // Upstream ranks runtime candidates with `providerOrder: -1`
                    // (`packages/skill/skill/src/index.ts:484`) so a runtime
                    // skill outranks a same-rank provider candidate within one
                    // layer. Using `i64::MIN`-style `-1` preserves that.
                    provider_order: -1,
                    local_order,
                },
                candidate,
                None,
            ));
        }

        for (provider_order, provider) in providers.iter().enumerate() {
            let observation =
                match wait_with_abort(provider.list(lookup), lookup.signal.clone()).await {
                    Ok(observation) => observation,
                    Err(error) if error.downcast_ref::<SkillLookupAborted>().is_some() => {
                        return Err(error);
                    }
                    Err(error) => {
                        eprintln!("skill provider \"{}\" skipped: {error}", provider.name());
                        cacheable = false;
                        continue;
                    }
                };
            if !observation.complete {
                cacheable = false;
            }
            for (local_order, candidate) in observation.candidates.into_iter().enumerate() {
                validate_candidate(&candidate, provider.name())?;
                indexed.push((
                    Indexed {
                        rank: candidate.rank,
                        provider_order: provider_order as i64,
                        local_order,
                    },
                    candidate,
                    Some(provider.clone()),
                ));
            }
        }

        indexed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut seen = HashSet::new();
        let mut winners = Vec::new();
        for (_, candidate, provider) in indexed {
            if seen.insert(candidate.name.clone()) {
                winners.push(IndexedCandidate {
                    candidate,
                    provider,
                    layer_id,
                });
            }
        }
        Ok(CollectResult {
            entries: winners,
            cacheable,
        })
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct SkillFileSystemConfig {
    pub include_default_roots: bool,
    pub dsh_home: PathBuf,
    pub agents_home: PathBuf,
    pub custom_skill_dirs: Vec<PathBuf>,
    pub bundled_skill_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub struct SkillWatchConfig {
    pub poll_interval: Duration,
}

impl Default for SkillWatchConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(200),
        }
    }
}

const MAX_WATCHED_ROOTS: usize = 128;

pub struct FileSystemSkillProvider {
    config: SkillFileSystemConfig,
    watch: Option<SkillWatch>,
}

struct SkillWatch {
    control: SkillProviderControl,
    poll_interval: Duration,
    state: Arc<parking_lot::Mutex<SkillWatchState>>,
}

struct SkillWatchState {
    thread: Option<std::thread::JoinHandle<()>>,
    watched: IndexMap<PathBuf, WatchedRoot>,
}

struct WatchedRoot {
    root: SkillRoot,
    fingerprint: u64,
}

impl SkillWatch {
    fn observe(&self, roots: &[SkillRoot]) {
        if self.control.is_cancelled() {
            return;
        }
        let mut state = self.state.lock();
        for root in roots {
            if !state.watched.contains_key(&root.path) {
                if let Ok(fingerprint) = root_fingerprint_sync(&root.path) {
                    state.watched.insert(
                        root.path.clone(),
                        WatchedRoot {
                            root: root.clone(),
                            fingerprint,
                        },
                    );
                }
            }
        }
        while state.watched.len() > MAX_WATCHED_ROOTS {
            if let Some(oldest) = state.watched.keys().next().cloned() {
                state.watched.shift_remove(&oldest);
            }
        }
        if state.thread.is_none() {
            let control = self.control.clone();
            let poll_interval = self.poll_interval;
            let watcher_state = self.state.clone();
            let thread = std::thread::Builder::new()
                .name("skill-filesystem-watcher".into())
                .spawn(move || watch_thread(control, poll_interval, watcher_state))
                .expect("failed to spawn skill filesystem watcher");
            state.thread = Some(thread);
        }
    }

    /// Synchronously invalidate when a first-party filesystem mutation could
    /// affect a watched skill entry, mirroring upstream
    /// `SkillWatchManager.observeHostMutation`
    /// (`packages/skill/skill-filesystem/src/index.ts:332`).
    fn observe_host_mutation(&self, path: &Path) {
        if self.control.is_cancelled() {
            return;
        }
        let normalized = absolute_path(path).unwrap_or_else(|_| path.to_path_buf());
        let state = self.state.lock();
        let affects_root = state
            .watched
            .values()
            .any(|entry| is_potential_skill_path(&entry.root, &normalized));
        drop(state);
        if affects_root {
            let _ = self.control.invalidate();
        }
    }
}

impl FileSystemSkillProvider {
    pub fn new(config: SkillFileSystemConfig) -> Result<Self> {
        if config.dsh_home.as_os_str().is_empty() || config.agents_home.as_os_str().is_empty() {
            bail!("skill filesystem homes cannot be empty");
        }
        Ok(Self {
            config,
            watch: None,
        })
    }

    pub fn with_watching(
        mut self,
        control: SkillProviderControl,
        watch_config: SkillWatchConfig,
    ) -> Self {
        self.watch = Some(SkillWatch {
            control,
            poll_interval: watch_config.poll_interval,
            state: Arc::new(parking_lot::Mutex::new(SkillWatchState {
                thread: None,
                watched: IndexMap::new(),
            })),
        });
        self
    }

    /// Synchronously invalidate this provider after a first-party filesystem
    /// mutation, mirroring upstream `observeHostMutation`
    /// (`packages/skill/skill-filesystem/src/index.ts:228`). Only a mutation
    /// that could affect a watched skill entry invalidates; everything else is a
    /// no-op.
    pub fn observe_host_mutation(&self, display_path: &Path) {
        if let Some(watch) = &self.watch {
            watch.observe_host_mutation(display_path);
        }
    }

    /// The `fs/observed` listener bridge. Upstream
    /// (`packages/skill/skill-filesystem/src/index.ts:139`) forwards only
    /// first-party `edit`/`write` tool mutations to `observeHostMutation`; any
    /// other actor is ignored.
    pub fn observe_fs_event(&self, display_path: &Path, actor_name: &str) {
        if mutation_tool_name(actor_name).is_none() {
            return;
        }
        self.observe_host_mutation(display_path);
    }

    async fn roots(&self, cwd: Option<&Path>) -> Result<Vec<SkillRoot>> {
        let mut roots = Vec::new();
        if self.config.include_default_roots {
            if let Some(cwd) = cwd {
                let cwd = absolute_path(cwd)?;
                let project = find_project_root(&cwd).await;
                roots.push(SkillRoot {
                    path: project.join(".dsh/skills"),
                    source: "project-dsh".into(),
                    rank: PROJECT_DSH_RANK,
                    skip_system: false,
                });
                roots.push(SkillRoot {
                    path: project.join(".agents/skills"),
                    source: "project-agents".into(),
                    rank: PROJECT_AGENTS_RANK,
                    skip_system: false,
                });
            }
        }

        for path in &self.config.custom_skill_dirs {
            roots.push(SkillRoot {
                path: absolute_path(path)?,
                source: "custom".into(),
                rank: CUSTOM_RANK,
                skip_system: false,
            });
        }

        if self.config.include_default_roots {
            roots.push(SkillRoot {
                path: absolute_path(&self.config.dsh_home)?.join("skills"),
                source: "user-dsh".into(),
                rank: USER_DSH_RANK,
                skip_system: true,
            });
            roots.push(SkillRoot {
                path: absolute_path(&self.config.agents_home)?.join("skills"),
                source: "user-agents".into(),
                rank: USER_AGENTS_RANK,
                skip_system: false,
            });
        }

        if let Some(path) = &self.config.bundled_skill_dir {
            roots.push(SkillRoot {
                path: absolute_path(path)?,
                source: "bundled".into(),
                rank: BUNDLED_SKILL_RANK,
                skip_system: false,
            });
        }
        Ok(roots)
    }
}

#[derive(Debug, Clone)]
struct SkillRoot {
    path: PathBuf,
    source: String,
    rank: u16,
    skip_system: bool,
}

#[async_trait]
impl SkillProvider for FileSystemSkillProvider {
    fn name(&self) -> &str {
        "filesystem"
    }

    async fn list(&self, options: &SkillLookupOptions) -> Result<SkillProviderObservation> {
        let roots = self.roots(options.cwd.as_deref()).await?;
        if let Some(watch) = &self.watch {
            watch.observe(&roots);
        }
        let mut candidates = Vec::new();
        for root in roots {
            let entries = list_skill_entries(&root.path).await?;
            for entry in entries {
                if root.skip_system && entry.name == ".system" {
                    continue;
                }
                let (path, directory) = if entry.metadata.is_dir() {
                    (entry.path.join("SKILL.md"), entry.path)
                } else if entry.metadata.is_file() && entry.name.ends_with(".md") {
                    (entry.path.clone(), root.path.clone())
                } else {
                    continue;
                };
                let Some(parsed) = parse_skill_file(&path).await? else {
                    continue;
                };
                let resource_base = SkillResourceBase::Directory {
                    path: directory.clone(),
                };
                candidates.push(SkillCandidate {
                    name: parsed.name,
                    description: parsed.description,
                    when_to_use: parsed.when_to_use,
                    invocation: parsed.invocation,
                    source: root.source.clone(),
                    provider: self.name().into(),
                    resource_base: Some(resource_base.clone()),
                    rank: root.rank,
                    locator: serde_json::to_value(LocalLocator {
                        path: path.clone(),
                        directory: directory.clone(),
                    })?,
                    path: Some(path),
                    metadata: parsed.metadata,
                });
            }
        }
        Ok(SkillProviderObservation::complete(candidates))
    }

    async fn get(
        &self,
        candidate: &SkillCandidate,
        _options: &SkillLookupOptions,
    ) -> Result<Option<SkillDefinition>> {
        let Some(path) = &candidate.path else {
            return Ok(None);
        };
        let Some(parsed) = parse_skill_file(path).await? else {
            return Ok(None);
        };
        if parsed.name != candidate.name {
            return Ok(None);
        }
        Ok(Some(SkillDefinition {
            name: parsed.name,
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            invocation: parsed.invocation,
            source: candidate.source.clone(),
            provider: candidate.provider.clone(),
            resource_base: candidate.resource_base.clone(),
            path: Some(path.clone()),
            metadata: parsed.metadata,
            content: parsed.content,
        }))
    }
}

#[derive(Debug, Clone, Serialize)]
struct LocalLocator {
    path: PathBuf,
    directory: PathBuf,
}

#[derive(Debug, Clone)]
struct RootEntry {
    name: String,
    path: PathBuf,
    metadata: std::fs::Metadata,
}

#[derive(Debug, Clone)]
struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    invocation: SkillInvocationPolicy,
    metadata: Option<serde_json::Value>,
    content: String,
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
    when_to_use: Option<String>,
    #[serde(rename = "disable-model-invocation")]
    disable_model_invocation: Option<serde_yaml::Value>,
    #[serde(rename = "user-invocable")]
    user_invocable: Option<serde_yaml::Value>,
    metadata: Option<serde_json::Value>,
}

pub fn is_skill_name(value: &str) -> bool {
    if value.is_empty() || value.starts_with('-') || value.ends_with('-') {
        return false;
    }
    let mut previous_hyphen = false;
    for character in value.chars() {
        let is_segment = character.is_ascii_lowercase() || character.is_ascii_digit();
        if character == '-' {
            if previous_hyphen {
                return false;
            }
            previous_hyphen = true;
        } else if is_segment {
            previous_hyphen = false;
        } else {
            return false;
        }
    }
    true
}

pub fn render_skill_content(
    name: &str,
    provider: &str,
    resource_base: Option<SkillResourceBase>,
    content: &str,
) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "<skill_content name=\"{}\">",
        escape_attribute(name)
    ));
    output.push_str("\n<skill_resources>\n");
    match resource_base {
        Some(SkillResourceBase::Directory { path }) => {
            output.push_str(&format!(
                "Base directory for this skill: {}\n",
                escape_text(&path.display().to_string())
            ));
            output.push_str(
                "Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n",
            );
        }
        Some(SkillResourceBase::Url { url }) => {
            output.push_str(&format!("Base URL for this skill: {}\n", escape_text(&url)));
            output.push_str(
                "Resolve relative URLs mentioned by this skill against the base URL before using them. Load referenced resources only as needed.\n",
            );
        }
        Some(SkillResourceBase::Opaque { description }) => {
            output.push_str(&format!(
                "Resources for this skill: {}\n",
                escape_text(&description)
            ));
            output.push_str("Load referenced resources only as needed.\n");
        }
        None => {
            output.push_str(&format!(
                "Resources for this skill are managed by provider \"{}\".\n",
                escape_text(provider)
            ));
            output.push_str("Load referenced resources only as needed.\n");
        }
    }
    output.push_str("</skill_resources>\n\n<skill_instructions>\n");
    output.push_str(content);
    output.push_str("\n</skill_instructions>\n</skill_content>");
    output
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn validate_candidate(candidate: &SkillCandidate, provider_name: &str) -> Result<()> {
    if !is_skill_name(&candidate.name) {
        bail!(
            "skill provider \"{provider_name}\" returned invalid skill name \"{}\"",
            candidate.name
        );
    }
    if candidate.description.is_empty() {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" without a description",
            candidate.name
        );
    }
    if candidate.source.is_empty() {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" without a source",
            candidate.name
        );
    }
    if candidate.provider != provider_name {
        bail!(
            "skill provider \"{provider_name}\" returned skill \"{}\" for provider \"{}\"",
            candidate.name,
            candidate.provider
        );
    }
    Ok(())
}

fn validate_definition(definition: &SkillDefinition) -> Result<()> {
    if !is_skill_name(&definition.name) {
        bail!("invalid skill name \"{}\"", definition.name);
    }
    if definition.description.is_empty() {
        bail!("skill \"{}\" requires a description", definition.name);
    }
    if definition.source.is_empty() || definition.provider.is_empty() {
        bail!(
            "skill \"{}\" requires a source and provider",
            definition.name
        );
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let current = std::env::current_dir().context("failed to resolve relative skill root")?;
    Ok(current.join(path))
}

async fn find_project_root(cwd: &Path) -> PathBuf {
    let mut current = cwd.to_path_buf();
    loop {
        if tokio::fs::try_exists(current.join(".git"))
            .await
            .unwrap_or(false)
        {
            return current;
        }
        let Some(parent) = current.parent() else {
            return cwd.to_path_buf();
        };
        if parent == current {
            return cwd.to_path_buf();
        }
        current = parent.to_path_buf();
    }
}

async fn list_skill_entries(path: &Path) -> Result<Vec<RootEntry>> {
    let mut entries = Vec::new();
    let mut directory = match tokio::fs::read_dir(path).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill root {}", path.display()))
        }
    };
    while let Some(entry) = directory
        .next_entry()
        .await
        .with_context(|| format!("failed to read skill root {}", path.display()))?
    {
        let Ok(metadata) = tokio::fs::metadata(entry.path()).await else {
            continue;
        };
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        entries.push(RootEntry {
            name,
            path: entry.path(),
            metadata,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn list_skill_entries_sync(path: &Path) -> Result<Vec<RootEntry>> {
    let mut entries = Vec::new();
    let directory = match std::fs::read_dir(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill root {}", path.display()))
        }
    };
    for entry in directory {
        let entry =
            entry.with_context(|| format!("failed to read skill root {}", path.display()))?;
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        entries.push(RootEntry {
            name,
            path: entry.path(),
            metadata,
        });
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(entries)
}

fn root_fingerprint_sync(path: &Path) -> Result<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    // Mirror upstream `resolveRootWatchMode`
    // (`packages/skill/skill-filesystem/src/index.ts`): watch the nearest
    // existing directory ancestor. When the root directory itself exists we
    // fingerprint its contents; when it does not, we fingerprint the nearest
    // existing ancestor's contents so that creating the missing segment (or an
    // intermediate directory) is observed even before any `SKILL.md` appears.
    // The `is_root` marker distinguishes a missing root from an existing empty
    // root, both of which otherwise hash an empty listing.
    let mut candidate = path;
    loop {
        match std::fs::metadata(candidate) {
            Ok(metadata) if metadata.is_dir() => {
                let is_root = candidate == path;
                is_root.hash(&mut hasher);
                hash_directory_listing(&mut hasher, candidate)?;
                return Ok(hasher.finish());
            }
            Ok(_) => {
                // Exists but is not a directory; walk up exactly as upstream
                // does when a path component is a non-directory.
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to fingerprint skill root {}", path.display())
                });
            }
        }
        let Some(parent) = candidate.parent() else {
            false.hash(&mut hasher);
            return Ok(hasher.finish());
        };
        if parent.as_os_str().is_empty() || parent == candidate {
            false.hash(&mut hasher);
            return Ok(hasher.finish());
        }
        candidate = parent;
    }
}

fn hash_directory_listing(hasher: &mut impl std::hash::Hasher, path: &Path) -> Result<()> {
    use std::hash::Hash;

    let entries = list_skill_entries_sync(path)?;
    entries.len().hash(hasher);
    for entry in &entries {
        entry.name.hash(hasher);
        entry.metadata.is_dir().hash(hasher);
        hash_metadata(hasher, &entry.metadata);
        if entry.metadata.is_dir() {
            match std::fs::metadata(entry.path.join("SKILL.md")) {
                Ok(metadata) => {
                    true.hash(hasher);
                    hash_metadata(hasher, &metadata);
                }
                Err(_) => {
                    false.hash(hasher);
                }
            }
        }
    }
    Ok(())
}

fn hash_metadata(hasher: &mut impl std::hash::Hasher, metadata: &std::fs::Metadata) {
    use std::hash::Hash;

    metadata.len().hash(hasher);
    if let Ok(modified) = metadata.modified() {
        if let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH) {
            duration.as_nanos().hash(hasher);
        }
    }
}

fn watch_thread(
    control: SkillProviderControl,
    poll_interval: Duration,
    state: Arc<parking_lot::Mutex<SkillWatchState>>,
) {
    loop {
        std::thread::sleep(poll_interval);
        if control.is_cancelled() {
            break;
        }
        let paths: Vec<PathBuf> = state.lock().watched.keys().cloned().collect();
        let mut updated = Vec::new();
        let mut removed = Vec::new();
        let mut changed = false;
        for path in paths {
            match root_fingerprint_sync(&path) {
                Ok(fingerprint) => {
                    let previous = state
                        .lock()
                        .watched
                        .get(&path)
                        .map(|entry| entry.fingerprint);
                    if previous != Some(fingerprint) {
                        changed = true;
                        updated.push((path, fingerprint));
                    }
                }
                Err(_) => {
                    changed = true;
                    removed.push(path);
                }
            }
        }
        if changed {
            let _ = control.invalidate();
        }
        let mut state = state.lock();
        for (path, fingerprint) in updated {
            if let Some(entry) = state.watched.get_mut(&path) {
                entry.fingerprint = fingerprint;
            }
        }
        for path in removed {
            state.watched.shift_remove(&path);
        }
    }
}

/// The upstream `mutationToolName` filter
/// (`packages/skill/skill-filesystem/src/index.ts:693`): only first-party
/// `edit` and `write` tools produce `fs/observed` mutations the provider cares
/// about.
fn mutation_tool_name(actor_name: &str) -> Option<&'static str> {
    match actor_name {
        "edit" => Some("edit"),
        "write" => Some("write"),
        _ => None,
    }
}

/// The upstream `containedSegments`
/// (`packages/skill/skill-filesystem/src/index.ts:685`): the path segments under
/// `root`, or `None` when `path` is not contained by `root`.
fn contained_segments(root: &Path, path: &Path) -> Option<Vec<String>> {
    let child = path.strip_prefix(root).ok()?;
    Some(
        child
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect(),
    )
}

/// The upstream `isPotentialSkillPath`
/// (`packages/skill/skill-filesystem/src/index.ts:677`): whether a mutated path
/// could be a skill entry under a watched root.
fn is_potential_skill_path(root: &SkillRoot, path: &Path) -> bool {
    let Some(segments) = contained_segments(&root.path, path) else {
        return false;
    };
    if segments.is_empty() || segments.len() > 2 {
        return false;
    }
    if root.skip_system && segments.first().map(String::as_str) == Some(".system") {
        return false;
    }
    if segments.len() == 1 {
        segments[0].ends_with(".md")
    } else {
        segments[1] == "SKILL.md"
    }
}

async fn parse_skill_file(path: &Path) -> Result<Option<ParsedSkill>> {
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read skill file {}", path.display()))
        }
    };
    let Some((yaml, body)) = split_frontmatter(&raw) else {
        return Ok(None);
    };
    let Ok(frontmatter) = parse_frontmatter(yaml) else {
        return Ok(None);
    };
    let Some(frontmatter) = frontmatter else {
        return Ok(None);
    };
    if !is_skill_name(&frontmatter.name) || frontmatter.description.is_empty() {
        return Ok(None);
    }
    let disable_model = match frontmatter_boolean(
        frontmatter.disable_model_invocation,
        "disable-model-invocation",
    ) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let user_invocable = match frontmatter_boolean(frontmatter.user_invocable, "user-invocable") {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    Ok(Some(ParsedSkill {
        name: frontmatter.name,
        description: frontmatter.description,
        when_to_use: frontmatter.when_to_use.filter(|value| !value.is_empty()),
        invocation: SkillInvocationPolicy {
            model_invocable: disable_model != Some(true),
            user_invocable: user_invocable != Some(false),
        },
        metadata: frontmatter.metadata.filter(|value| value.is_object()),
        content: body.trim().to_string(),
    }))
}

fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let first_end = raw.find('\n')?;
    if raw[..first_end].trim_end_matches('\r') != "---" {
        return None;
    }
    let start = first_end + 1;
    let mut line_start = start;
    loop {
        let relative_end = raw[line_start..].find('\n')?;
        let line_end = line_start + relative_end;
        let line = raw[line_start..line_end].trim_end_matches('\r');
        if line == "---" {
            return Some((&raw[start..line_start], &raw[line_end + 1..]));
        }
        line_start = line_end + 1;
    }
}

fn parse_frontmatter(yaml: &str) -> Result<Option<Frontmatter>> {
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let Some(mapping) = value.as_mapping() else {
        return Ok(None);
    };
    for legacy in ["disableModelInvocation", "modelInvocable", "userInvocable"] {
        if mapping.contains_key(serde_yaml::Value::String(legacy.into())) {
            return Ok(None);
        }
    }
    serde_yaml::from_value(value)
        .context("invalid skill frontmatter")
        .map(Some)
}

fn frontmatter_boolean(value: Option<serde_yaml::Value>, field: &str) -> Result<Option<bool>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let rendered = match &value {
        serde_yaml::Value::Bool(value) => value.to_string(),
        serde_yaml::Value::Number(value) => value.to_string(),
        serde_yaml::Value::String(value) => value.to_lowercase(),
        _ => bail!("frontmatter field \"{field}\" must be a boolean"),
    };
    match rendered.as_str() {
        "true" | "yes" | "on" | "1" => Ok(Some(true)),
        "false" | "no" | "off" | "0" => Ok(Some(false)),
        _ => bail!("frontmatter field \"{field}\" must be a boolean"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_root(path: &str, skip_system: bool) -> SkillRoot {
        SkillRoot {
            path: PathBuf::from(path),
            source: "test".into(),
            rank: 0,
            skip_system,
        }
    }

    #[test]
    fn mutation_tool_name_accepts_only_edit_and_write() {
        assert_eq!(mutation_tool_name("edit"), Some("edit"));
        assert_eq!(mutation_tool_name("write"), Some("write"));
        assert_eq!(mutation_tool_name("read"), None);
        assert_eq!(mutation_tool_name(""), None);
    }

    #[test]
    fn contained_segments_rejects_paths_outside_the_root() {
        let root = Path::new("/workspace/.dsh/skills");
        assert_eq!(
            contained_segments(root, Path::new("/workspace/.dsh/skills/a/SKILL.md")),
            Some(vec!["a".into(), "SKILL.md".into()])
        );
        assert_eq!(
            contained_segments(root, Path::new("/workspace/.dsh/skills/a.md")),
            Some(vec!["a.md".into()])
        );
        assert_eq!(
            contained_segments(root, Path::new("/workspace/.dsh/skills")),
            Some(vec![])
        );
        assert_eq!(
            contained_segments(root, Path::new("/workspace/.dsh/other/a.md")),
            None
        );
        assert_eq!(
            contained_segments(root, Path::new("/workspace/.dsh/skills-extra/a.md")),
            None
        );
    }

    #[test]
    fn is_potential_skill_path_matches_upstream_gating() {
        let root = skill_root("/workspace/.dsh/skills", false);
        // One segment ending in `.md` is a flat skill.
        assert!(is_potential_skill_path(
            &root,
            Path::new("/workspace/.dsh/skills/alpha.md")
        ));
        // A directory skill is `<name>/SKILL.md`.
        assert!(is_potential_skill_path(
            &root,
            Path::new("/workspace/.dsh/skills/alpha/SKILL.md")
        ));
        // Deep paths are not skill entries.
        assert!(!is_potential_skill_path(
            &root,
            Path::new("/workspace/.dsh/skills/a/b/c.md")
        ));
        // A non-SKILL.md file directly under a skill directory is not an entry.
        assert!(!is_potential_skill_path(
            &root,
            Path::new("/workspace/.dsh/skills/alpha/other.md")
        ));
        // Outside the root is not observed.
        assert!(!is_potential_skill_path(
            &root,
            Path::new("/workspace/.agents/skills/a/SKILL.md")
        ));

        // skipSystem roots ignore `.system` skill entries.
        let system_root = skill_root("/home/user/.dsh/skills", true);
        assert!(!is_potential_skill_path(
            &system_root,
            Path::new("/home/user/.dsh/skills/.system/SKILL.md")
        ));
        assert!(is_potential_skill_path(
            &system_root,
            Path::new("/home/user/.dsh/skills/normal/SKILL.md")
        ));
    }
}
