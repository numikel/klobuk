//! Deterministic scheduler: DAG validation, ready promotion, scope conflicts, allocation plan.
//!
//! Everything here is a pure function of its inputs so the same store snapshot always yields the
//! same plan (property-tested). The orchestrator echoes `inputs_hash` on approval so a stale plan
//! cannot be started.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

use globset::{Glob, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::IssueId;
use crate::model::{Claim, Dep, Issue, IssueType, Status};
use crate::run::{Run, RunState};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("dependency cycle among {0:?}")]
    Cycle(Vec<IssueId>),
    #[error("unknown issue referenced in deps: {0}")]
    UnknownIssue(IssueId),
    #[error("bad glob in {issue} file_scope `{glob}`: {msg}")]
    BadGlob {
        issue: IssueId,
        glob: String,
        msg: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub issue: IssueId,
    pub title: String,
    pub branch: String,
    pub worktree: String,
    pub scope: Vec<String>,
    pub priority: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum DeferReason {
    DepsOpen(Vec<IssueId>),
    ScopeConflict(IssueId),
    Claimed(String),
    SlotsFull,
    NoScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deferred {
    pub issue: IssueId,
    #[serde(flatten)]
    pub reason: DeferReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllocationPlan {
    pub slots: Vec<Slot>,
    pub deferred: Vec<Deferred>,
    pub inputs_hash: String,
}

pub struct SchedulerInput<'a> {
    pub issues: &'a [Issue],
    pub deps: &'a [Dep],
    /// Every claimed issue. Claims decide scope conflicts, whether or not a worker is running:
    /// an issue waiting in `validating` still has an unmerged branch.
    pub claims: &'a [Claim],
    pub tracked_files: &'a [String],
    /// Worker runs that are active right now (see [`count_active_runs`]); only these use up
    /// `max_parallel`.
    pub active_workers: usize,
    pub max_parallel: usize,
    pub hotspots: &'a [String],
}

/// Active runs, split by the kind of issue they work on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCounts {
    /// Runs on issues that are not reviews; they count against `[scheduler] max_parallel`.
    pub workers: usize,
    /// Runs on review issues; they count against `[review] max_parallel`.
    pub reviewers: usize,
}

/// Whether a run occupies a slot: it is starting, working, blocked, finishing or verifying.
fn run_is_active(state: RunState) -> bool {
    matches!(
        state,
        RunState::Starting
            | RunState::Working
            | RunState::Blocked
            | RunState::Finishing
            | RunState::Verifying
    )
}

/// Counts the active runs, workers apart from reviewers. A claim without an active run (an issue in
/// `validating`, a run that already succeeded) is not counted. A run whose issue is unknown counts
/// as a worker.
pub fn count_active_runs(issues: &[Issue], runs: &[Run]) -> RunCounts {
    let reviews: HashSet<&IssueId> = issues
        .iter()
        .filter(|i| i.kind == IssueType::Review)
        .map(|i| &i.id)
        .collect();
    let mut counts = RunCounts {
        workers: 0,
        reviewers: 0,
    };
    for r in runs.iter().filter(|r| run_is_active(r.state)) {
        if reviews.contains(&r.issue_id) {
            counts.reviewers += 1;
        } else {
            counts.workers += 1;
        }
    }
    counts
}

/// Validates the dependency graph and returns a topological order (dependencies first).
/// Ties are broken by id so the order is deterministic.
pub fn validate_dag(issues: &[Issue], deps: &[Dep]) -> Result<Vec<IssueId>, SchedulerError> {
    let known: HashSet<&IssueId> = issues.iter().map(|i| &i.id).collect();
    for d in deps {
        for id in [&d.issue_id, &d.depends_on] {
            if !known.contains(id) {
                return Err(SchedulerError::UnknownIssue(id.clone()));
            }
        }
    }
    let mut indegree: BTreeMap<&IssueId, usize> = issues.iter().map(|i| (&i.id, 0)).collect();
    let mut dependents: HashMap<&IssueId, Vec<&IssueId>> = HashMap::new();
    for d in deps {
        *indegree.get_mut(&d.issue_id).expect("validated above") += 1;
        dependents.entry(&d.depends_on).or_default().push(&d.issue_id);
    }
    let mut queue: VecDeque<&IssueId> = indegree
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order = Vec::with_capacity(issues.len());
    while let Some(id) = queue.pop_front() {
        order.push(id.clone());
        if let Some(children) = dependents.get(id) {
            let mut now_ready: Vec<&IssueId> = Vec::new();
            for c in children {
                let n = indegree.get_mut(c).expect("validated above");
                *n -= 1;
                if *n == 0 {
                    now_ready.push(c);
                }
            }
            now_ready.sort();
            queue.extend(now_ready);
        }
    }
    if order.len() != issues.len() {
        let mut stuck: Vec<IssueId> = indegree
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(id, _)| id.clone())
            .collect();
        stuck.sort();
        return Err(SchedulerError::Cycle(stuck));
    }
    Ok(order)
}

/// Backlog issues whose every dependency is `done`. A dep on an unknown issue blocks promotion.
pub fn promotable(issues: &[Issue], deps: &[Dep]) -> Vec<IssueId> {
    let status: HashMap<&IssueId, Status> = issues.iter().map(|i| (&i.id, i.status)).collect();
    let mut out: Vec<IssueId> = issues
        .iter()
        .filter(|i| i.status == Status::Backlog && i.kind != IssueType::Epic)
        .filter(|i| {
            deps.iter()
                .filter(|d| d.issue_id == i.id)
                .all(|d| status.get(&d.depends_on) == Some(&Status::Done))
        })
        .map(|i| i.id.clone())
        .collect();
    out.sort();
    out
}

/// Open epics whose children are all terminal and at least one is `done`.
///
/// An epic without children, or whose children were all cancelled, is left to the operator.
pub fn completable_epics(issues: &[Issue]) -> Vec<IssueId> {
    let mut out: Vec<IssueId> = issues
        .iter()
        .filter(|e| e.kind == IssueType::Epic && !e.status.is_terminal())
        .filter(|e| {
            let mut children = issues
                .iter()
                .filter(|c| c.parent_id.as_ref() == Some(&e.id))
                .peekable();
            children.peek().is_some() && {
                let mut any_done = false;
                let all_terminal = children.all(|c| {
                    any_done |= c.status == Status::Done;
                    c.status.is_terminal()
                });
                all_terminal && any_done
            }
        })
        .map(|e| e.id.clone())
        .collect();
    out.sort();
    out
}

/// Validates every glob in a scope; the error names the offending pattern.
pub fn validate_scope(scope: &[String]) -> Result<(), String> {
    for g in scope {
        Glob::new(g).map_err(|e| format!("bad glob `{g}`: {e}"))?;
    }
    Ok(())
}

fn is_literal(pattern: &str) -> bool {
    !pattern.chars().any(|c| matches!(c, '*' | '?' | '[' | '{'))
}

/// The concrete files a scope stands for: tracked files matched by its globs plus literal paths
/// (which may not exist yet).
fn expand_scope(scope: &[String], tracked: &[String]) -> Result<BTreeSet<String>, globset::Error> {
    let mut out = BTreeSet::new();
    if scope.is_empty() {
        return Ok(out);
    }
    let mut b = GlobSetBuilder::new();
    for p in scope {
        if is_literal(p) {
            out.insert(p.clone());
        }
        b.add(Glob::new(p)?);
    }
    let set = b.build()?;
    for f in tracked {
        if set.is_match(f) {
            out.insert(f.clone());
        }
    }
    Ok(out)
}

/// Indices of hotspot globs touched by any file of an expanded scope.
fn hotspot_hits(files: &BTreeSet<String>, hotspots: &[String]) -> Result<BTreeSet<usize>, globset::Error> {
    let mut hits = BTreeSet::new();
    for (i, h) in hotspots.iter().enumerate() {
        let m = Glob::new(h)?.compile_matcher();
        if files.iter().any(|f| m.is_match(f)) {
            hits.insert(i);
        }
    }
    Ok(hits)
}

/// Two scopes conflict when their expansions intersect, when both touch the same hotspot, or when
/// either is empty (an unscoped issue may touch anything).
pub fn scopes_conflict(
    a: &[String],
    b: &[String],
    tracked: &[String],
    hotspots: &[String],
) -> Result<bool, globset::Error> {
    let ea = expand_scope(a, tracked)?;
    let eb = expand_scope(b, tracked)?;
    if a.is_empty() || b.is_empty() {
        return Ok(true);
    }
    if ea.intersection(&eb).next().is_some() {
        return Ok(true);
    }
    let ha = hotspot_hits(&ea, hotspots)?;
    let hb = hotspot_hits(&eb, hotspots)?;
    Ok(ha.intersection(&hb).next().is_some())
}

/// Length of the longest chain of issues that (transitively) depend on `id`.
fn downstream_chain(
    id: &IssueId,
    dependents: &HashMap<&IssueId, Vec<&IssueId>>,
    memo: &mut HashMap<IssueId, usize>,
) -> usize {
    if let Some(&n) = memo.get(id) {
        return n;
    }
    let n = dependents
        .get(id)
        .map(|ch| {
            ch.iter()
                .map(|c| 1 + downstream_chain(c, dependents, memo))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    memo.insert(id.clone(), n);
    n
}

fn inputs_hash(input: &SchedulerInput<'_>) -> String {
    let mut h = DefaultHasher::new();
    for i in input.issues {
        (
            i.id.as_str(),
            i.status.as_str(),
            i.priority,
            &i.file_scope,
            i.kind.as_str(),
        )
            .hash(&mut h);
    }
    for d in input.deps {
        (d.issue_id.as_str(), d.depends_on.as_str()).hash(&mut h);
    }
    for c in input.claims {
        (c.issue_id.as_str(), c.worker.as_str()).hash(&mut h);
    }
    input.active_workers.hash(&mut h);
    input.max_parallel.hash(&mut h);
    input.hotspots.hash(&mut h);
    input.tracked_files.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn check_globs(input: &SchedulerInput<'_>) -> Result<(), SchedulerError> {
    for i in input.issues {
        for g in &i.file_scope {
            Glob::new(g).map_err(|e| SchedulerError::BadGlob {
                issue: i.id.clone(),
                glob: g.clone(),
                msg: e.to_string(),
            })?;
        }
    }
    Ok(())
}

/// Builds the allocation plan. Candidates are `ready`, non-epic issues ordered by priority
/// (ascending), longest downstream chain (descending), then id. Deferred reasons are reported in
/// the order `Claimed`, `DepsOpen`, `ScopeConflict`/`NoScope`, `SlotsFull`.
pub fn plan(input: &SchedulerInput<'_>, worktrees_dir: &str) -> Result<AllocationPlan, SchedulerError> {
    validate_dag(input.issues, input.deps)?;
    check_globs(input)?;
    let by_id: HashMap<&IssueId, &Issue> = input.issues.iter().map(|i| (&i.id, i)).collect();
    let claimed: HashMap<&IssueId, &str> = input
        .claims
        .iter()
        .map(|c| (&c.issue_id, c.worker.as_str()))
        .collect();

    let mut dependents: HashMap<&IssueId, Vec<&IssueId>> = HashMap::new();
    for d in input.deps {
        dependents.entry(&d.depends_on).or_default().push(&d.issue_id);
    }
    let mut memo = HashMap::new();
    let mut candidates: Vec<&Issue> = input
        .issues
        .iter()
        .filter(|i| i.status == Status::Ready && i.kind != IssueType::Epic)
        .collect();
    candidates.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| {
                downstream_chain(&b.id, &dependents, &mut memo).cmp(&downstream_chain(
                    &a.id,
                    &dependents,
                    &mut memo,
                ))
            })
            .then_with(|| a.id.cmp(&b.id))
    });

    // only active worker runs use up slots; `occupied` below still holds every claim for conflicts
    let capacity = input.max_parallel.saturating_sub(input.active_workers);
    let mut occupied: Vec<(&IssueId, &[String])> = input
        .claims
        .iter()
        .filter_map(|c| by_id.get(&c.issue_id).map(|i| (&i.id, i.file_scope.as_slice())))
        .collect();

    let mut slots = Vec::new();
    let mut deferred = Vec::new();
    for c in candidates {
        if let Some(w) = claimed.get(&c.id) {
            deferred.push(Deferred {
                issue: c.id.clone(),
                reason: DeferReason::Claimed((*w).to_string()),
            });
            continue;
        }
        let open: Vec<IssueId> = input
            .deps
            .iter()
            .filter(|d| d.issue_id == c.id)
            .filter(|d| by_id.get(&d.depends_on).is_none_or(|i| i.status != Status::Done))
            .map(|d| d.depends_on.clone())
            .collect();
        if !open.is_empty() {
            deferred.push(Deferred {
                issue: c.id.clone(),
                reason: DeferReason::DepsOpen(open),
            });
            continue;
        }
        let mut clash: Option<&IssueId> = None;
        for (other, scope) in &occupied {
            let hit =
                scopes_conflict(&c.file_scope, scope, input.tracked_files, input.hotspots).map_err(|e| {
                    SchedulerError::BadGlob {
                        issue: c.id.clone(),
                        glob: String::new(),
                        msg: e.to_string(),
                    }
                })?;
            if hit {
                clash = Some(other);
                break;
            }
        }
        if let Some(other) = clash {
            let reason = if c.file_scope.is_empty() {
                DeferReason::NoScope
            } else {
                DeferReason::ScopeConflict(other.clone())
            };
            deferred.push(Deferred {
                issue: c.id.clone(),
                reason,
            });
            continue;
        }
        if slots.len() >= capacity {
            deferred.push(Deferred {
                issue: c.id.clone(),
                reason: DeferReason::SlotsFull,
            });
            continue;
        }
        slots.push(Slot {
            issue: c.id.clone(),
            title: c.title.clone(),
            branch: format!("feat/{}", c.id),
            worktree: format!("{}/{}", worktrees_dir.trim_end_matches('/'), c.id),
            scope: c.file_scope.clone(),
            priority: c.priority,
        });
        occupied.push((&c.id, c.file_scope.as_slice()));
    }

    Ok(AllocationPlan {
        slots,
        deferred,
        inputs_hash: inputs_hash(input),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::run::{Run, RunState};
    use proptest::prelude::*;

    fn id(s: &str) -> IssueId {
        s.parse().unwrap()
    }

    fn mk(i: &str, status: Status, scope: &[&str]) -> Issue {
        Issue {
            id: id(i),
            uid: format!("uid-{i}"),
            title: format!("issue {i}"),
            body: String::new(),
            kind: IssueType::Task,
            status,
            priority: 2,
            parent_id: None,
            file_scope: scope.iter().map(|s| s.to_string()).collect(),
            acceptance: vec![],
            estimate: None,
            labels: vec![],
            source: None,
            created_at: "2026-11-02T00:00:00Z".into(),
            updated_at: "2026-11-02T00:00:00Z".into(),
            closed_at: None,
        }
    }

    fn dep(a: &str, on: &str) -> Dep {
        Dep {
            issue_id: id(a),
            depends_on: id(on),
        }
    }

    fn claim(i: &str, worker: &str) -> Claim {
        Claim {
            issue_id: id(i),
            worker: worker.into(),
            run_id: None,
            worktree: None,
            branch: None,
            claimed_at: "2026-11-02T00:00:00Z".into(),
            note: None,
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Every claim in these tests stands for an active worker run, unless a test says otherwise.
    fn input<'a>(
        issues: &'a [Issue],
        deps: &'a [Dep],
        claims: &'a [Claim],
        tracked: &'a [String],
        max_parallel: usize,
        hotspots: &'a [String],
    ) -> SchedulerInput<'a> {
        SchedulerInput {
            issues,
            deps,
            claims,
            tracked_files: tracked,
            active_workers: claims.len(),
            max_parallel,
            hotspots,
        }
    }

    fn run_of(i: &str, state: RunState) -> Run {
        Run {
            id: format!("run-{i}-{state}"),
            issue_id: id(i),
            attempt: 1,
            runtime: "fake".into(),
            agent_name: None,
            workspace_id: None,
            pane_id: None,
            claude_session_id: None,
            transcript_path: None,
            worktree: format!(".worktrees/{i}"),
            branch: format!("feat/{i}"),
            base_sha: "0".repeat(40),
            state,
            budget_usd: None,
            cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            turns: 0,
            tool_calls: 0,
            exit_reason: None,
            report_path: None,
            started_at: "2026-11-02T00:00:00Z".into(),
            last_seen_at: None,
            finished_at: None,
            herdr_revision: None,
        }
    }

    #[test]
    fn held_claim_without_active_run_takes_no_slot() {
        let issues = [
            mk("asf-1", Status::Validating, &["src/a.rs"]),
            mk("asf-2", Status::Ready, &["src/b.rs"]),
            mk("asf-3", Status::Ready, &["src/c.rs"]),
        ];
        let t = strs(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let claims = [claim("asf-1", "asf-1")];
        let mut inp = input(&issues, &[], &claims, &t, 1, &[]);
        inp.active_workers = 0;
        let p = plan(&inp, ".worktrees").unwrap();
        let slots: Vec<_> = p.slots.iter().map(|s| s.issue.to_string()).collect();
        assert_eq!(slots, ["asf-2"], "the validating issue holds a claim, not a slot");
        assert_eq!(p.deferred.len(), 1);
        assert_eq!(p.deferred[0].issue, id("asf-3"));
        assert_eq!(p.deferred[0].reason, DeferReason::SlotsFull);
    }

    #[test]
    fn held_claim_still_blocks_a_conflicting_scope() {
        let issues = [
            mk("asf-1", Status::Validating, &["src/a.rs"]),
            mk("asf-2", Status::Ready, &["src/a.rs"]),
            mk("asf-3", Status::Ready, &["src/b.rs"]),
        ];
        let t = strs(&["src/a.rs", "src/b.rs"]);
        let claims = [claim("asf-1", "asf-1")];
        let mut inp = input(&issues, &[], &claims, &t, 3, &[]);
        inp.active_workers = 0;
        let p = plan(&inp, ".worktrees").unwrap();
        let slots: Vec<_> = p.slots.iter().map(|s| s.issue.to_string()).collect();
        assert_eq!(slots, ["asf-3"]);
        assert_eq!(p.deferred.len(), 1);
        assert_eq!(p.deferred[0].issue, id("asf-2"));
        assert_eq!(p.deferred[0].reason, DeferReason::ScopeConflict(id("asf-1")));
    }

    #[test]
    fn active_workers_use_up_the_capacity() {
        let issues = [
            mk("asf-1", Status::InProgress, &["src/a.rs"]),
            mk("asf-2", Status::InProgress, &["src/b.rs"]),
            mk("asf-3", Status::Ready, &["src/c.rs"]),
        ];
        let t = strs(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let claims = [claim("asf-1", "asf-1"), claim("asf-2", "asf-2")];
        let mut inp = input(&issues, &[], &claims, &t, 2, &[]);
        inp.active_workers = 2;
        let p = plan(&inp, ".worktrees").unwrap();
        assert!(p.slots.is_empty());
        assert_eq!(p.deferred[0].reason, DeferReason::SlotsFull);
    }

    #[test]
    fn inputs_hash_follows_the_active_worker_count() {
        let issues = [mk("asf-1", Status::Ready, &["a.rs"])];
        let t = strs(&["a.rs"]);
        let mut a = input(&issues, &[], &[], &t, 2, &[]);
        let h0 = plan(&a, ".worktrees").unwrap().inputs_hash;
        a.active_workers = 1;
        let h1 = plan(&a, ".worktrees").unwrap().inputs_hash;
        assert_ne!(h0, h1, "a stale plan must not be approved after a worker started");
    }

    #[test]
    fn count_active_runs_splits_workers_from_reviewers() {
        let mut review = mk("asf-9", Status::InProgress, &[]);
        review.kind = IssueType::Review;
        let issues = [
            mk("asf-1", Status::InProgress, &["a.rs"]),
            mk("asf-2", Status::Validating, &["b.rs"]),
            mk("asf-3", Status::InProgress, &["c.rs"]),
            review,
        ];
        let runs = [
            run_of("asf-1", RunState::Working),
            run_of("asf-1", RunState::Failed),
            run_of("asf-2", RunState::Succeeded),
            run_of("asf-3", RunState::Verifying),
            run_of("asf-3", RunState::Stalled),
            run_of("asf-9", RunState::Starting),
            run_of("asf-9", RunState::Aborted),
        ];
        let c = count_active_runs(&issues, &runs);
        assert_eq!(
            c,
            RunCounts {
                workers: 2,
                reviewers: 1
            }
        );
        for s in [
            RunState::Starting,
            RunState::Working,
            RunState::Blocked,
            RunState::Finishing,
            RunState::Verifying,
        ] {
            assert_eq!(
                count_active_runs(&issues, &[run_of("asf-1", s)]).workers,
                1,
                "{s}"
            );
        }
        for s in [
            RunState::Succeeded,
            RunState::Failed,
            RunState::Aborted,
            RunState::Stalled,
            RunState::OverBudget,
        ] {
            assert_eq!(
                count_active_runs(&issues, &[run_of("asf-1", s)]).workers,
                0,
                "{s}"
            );
        }
    }

    #[test]
    fn topo_order_follows_deps() {
        let issues = [
            mk("asf-1", Status::Ready, &[]),
            mk("asf-2", Status::Ready, &[]),
            mk("asf-3", Status::Ready, &[]),
        ];
        let deps = [dep("asf-1", "asf-3"), dep("asf-2", "asf-1")];
        let order = validate_dag(&issues, &deps).unwrap();
        let pos = |s: &str| order.iter().position(|x| x == &id(s)).unwrap();
        assert!(pos("asf-3") < pos("asf-1"));
        assert!(pos("asf-1") < pos("asf-2"));
    }

    #[test]
    fn cycle_detected() {
        let issues = [mk("asf-1", Status::Ready, &[]), mk("asf-2", Status::Ready, &[])];
        let deps = [dep("asf-1", "asf-2"), dep("asf-2", "asf-1")];
        let e = validate_dag(&issues, &deps).unwrap_err();
        assert!(
            matches!(e, SchedulerError::Cycle(ref ids) if ids.len() == 2),
            "{e:?}"
        );
    }

    #[test]
    fn unknown_dep_is_error() {
        let issues = [mk("asf-1", Status::Ready, &[])];
        let deps = [dep("asf-1", "asf-9")];
        let e = validate_dag(&issues, &deps).unwrap_err();
        assert_eq!(e, SchedulerError::UnknownIssue(id("asf-9")));
    }

    #[test]
    fn promotable_when_all_deps_done() {
        let issues = [
            mk("asf-1", Status::Done, &[]),
            mk("asf-2", Status::Backlog, &[]),
            mk("asf-3", Status::Backlog, &[]),
            mk("asf-4", Status::Ready, &[]),
            mk("asf-5", Status::Backlog, &[]),
        ];
        let deps = [dep("asf-2", "asf-1"), dep("asf-3", "asf-4")];
        let p = promotable(&issues, &deps);
        assert_eq!(p, vec![id("asf-2"), id("asf-5")]);
    }

    #[test]
    fn completable_epics_need_all_children_terminal_and_one_done() {
        let mut epic = mk("asf-1", Status::Backlog, &[]);
        epic.kind = IssueType::Epic;
        let mut empty_epic = mk("asf-2", Status::Backlog, &[]);
        empty_epic.kind = IssueType::Epic;
        let mut open_epic = mk("asf-3", Status::Backlog, &[]);
        open_epic.kind = IssueType::Epic;
        let mut cancelled_epic = mk("asf-4", Status::Backlog, &[]);
        cancelled_epic.kind = IssueType::Epic;
        let child = |i: &str, parent: &str, status: Status| {
            let mut c = mk(i, status, &[]);
            c.parent_id = Some(id(parent));
            c
        };
        let issues = [
            epic,
            empty_epic,
            open_epic,
            cancelled_epic,
            child("asf-5", "asf-1", Status::Done),
            child("asf-6", "asf-1", Status::Cancelled),
            child("asf-7", "asf-3", Status::Done),
            child("asf-8", "asf-3", Status::InProgress),
            child("asf-9", "asf-4", Status::Cancelled),
        ];
        assert_eq!(completable_epics(&issues), vec![id("asf-1")]);
    }

    #[test]
    fn completable_epics_skips_already_closed() {
        let mut epic = mk("asf-1", Status::Done, &[]);
        epic.kind = IssueType::Epic;
        let mut c = mk("asf-2", Status::Done, &[]);
        c.parent_id = Some(id("asf-1"));
        assert!(completable_epics(&[epic, c]).is_empty());
    }

    #[test]
    fn scope_overlap_literal() {
        let t = strs(&["src/a.rs"]);
        assert!(scopes_conflict(&strs(&["src/a.rs"]), &strs(&["src/a.rs"]), &t, &[]).unwrap());
        assert!(!scopes_conflict(&strs(&["src/a.rs"]), &strs(&["src/b.rs"]), &t, &[]).unwrap());
    }

    #[test]
    fn scope_literal_new_file_counts_even_if_untracked() {
        let t: Vec<String> = vec![];
        assert!(scopes_conflict(&strs(&["src/new.rs"]), &strs(&["src/new.rs"]), &t, &[]).unwrap());
    }

    #[test]
    fn scope_glob_vs_tracked() {
        let t = strs(&["src/x.rs", "src/sub/y.rs", "tests/t.rs"]);
        assert!(scopes_conflict(&strs(&["src/**"]), &strs(&["src/x.rs"]), &t, &[]).unwrap());
        assert!(scopes_conflict(&strs(&["src/**"]), &strs(&["src/sub/*.rs"]), &t, &[]).unwrap());
        assert!(!scopes_conflict(&strs(&["src/**"]), &strs(&["tests/t.rs"]), &t, &[]).unwrap());
        assert!(!scopes_conflict(&strs(&["src/**"]), &strs(&["tests/**"]), &t, &[]).unwrap());
    }

    #[test]
    fn hotspot_shared() {
        let t = strs(&["crates/a/Cargo.toml", "crates/b/Cargo.toml"]);
        let hot = strs(&["crates/*/Cargo.toml"]);
        assert!(scopes_conflict(
            &strs(&["crates/a/Cargo.toml"]),
            &strs(&["crates/b/Cargo.toml"]),
            &t,
            &hot
        )
        .unwrap());
        assert!(!scopes_conflict(
            &strs(&["crates/a/Cargo.toml"]),
            &strs(&["crates/b/Cargo.toml"]),
            &t,
            &[]
        )
        .unwrap());
    }

    #[test]
    fn empty_scope_conflicts() {
        let t = strs(&["src/a.rs"]);
        assert!(scopes_conflict(&[], &strs(&["src/a.rs"]), &t, &[]).unwrap());
        assert!(scopes_conflict(&[], &[], &t, &[]).unwrap());
    }

    #[test]
    fn bad_glob_is_error() {
        let t = strs(&["src/a.rs"]);
        assert!(scopes_conflict(&strs(&["src/[a.rs"]), &strs(&["src/a.rs"]), &t, &[]).is_err());
        let issues = [
            mk("asf-1", Status::Ready, &["src/[a.rs"]),
            mk("asf-2", Status::Ready, &["src/a.rs"]),
        ];
        let e = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap_err();
        assert!(
            matches!(e, SchedulerError::BadGlob { ref issue, .. } if issue == &id("asf-1")),
            "{e:?}"
        );
    }

    #[test]
    fn plan_respects_max_parallel() {
        let issues = [
            mk("asf-1", Status::Ready, &["a.rs"]),
            mk("asf-2", Status::Ready, &["b.rs"]),
            mk("asf-3", Status::Ready, &["c.rs"]),
        ];
        let t = strs(&["a.rs", "b.rs", "c.rs"]);
        let p = plan(&input(&issues, &[], &[], &t, 2, &[]), ".worktrees").unwrap();
        assert_eq!(p.slots.len(), 2);
        assert_eq!(p.slots[0].issue, id("asf-1"));
        assert_eq!(p.slots[0].branch, "feat/asf-1");
        assert_eq!(p.slots[0].worktree, ".worktrees/asf-1");
        assert_eq!(p.deferred.len(), 1);
        assert_eq!(p.deferred[0].issue, id("asf-3"));
        assert_eq!(p.deferred[0].reason, DeferReason::SlotsFull);
    }

    #[test]
    fn plan_defers_conflicting_scope() {
        let issues = [
            mk("asf-1", Status::Ready, &["src/a.rs"]),
            mk("asf-2", Status::Ready, &["src/a.rs"]),
            mk("asf-3", Status::Ready, &["tests/**"]),
        ];
        let t = strs(&["src/a.rs", "tests/t.rs"]);
        let p = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        let slots: Vec<_> = p.slots.iter().map(|s| s.issue.to_string()).collect();
        assert_eq!(slots, ["asf-1", "asf-3"]);
        assert_eq!(p.deferred.len(), 1);
        assert_eq!(p.deferred[0].reason, DeferReason::ScopeConflict(id("asf-1")));
    }

    #[test]
    fn plan_counts_claims_against_capacity_and_scope() {
        let issues = [
            mk("asf-1", Status::InProgress, &["src/a.rs"]),
            mk("asf-2", Status::Ready, &["src/a.rs"]),
            mk("asf-3", Status::Ready, &["src/b.rs"]),
            mk("asf-4", Status::Ready, &["src/c.rs"]),
        ];
        let t = strs(&["src/a.rs", "src/b.rs", "src/c.rs"]);
        let claims = [claim("asf-1", "asf-1")];
        let p = plan(&input(&issues, &[], &claims, &t, 2, &[]), ".worktrees").unwrap();
        let slots: Vec<_> = p.slots.iter().map(|s| s.issue.to_string()).collect();
        assert_eq!(slots, ["asf-3"]);
        let reasons: Vec<_> = p
            .deferred
            .iter()
            .map(|d| (d.issue.to_string(), d.reason.clone()))
            .collect();
        assert_eq!(
            reasons,
            vec![
                ("asf-2".to_string(), DeferReason::ScopeConflict(id("asf-1"))),
                ("asf-4".to_string(), DeferReason::SlotsFull),
            ]
        );
    }

    #[test]
    fn plan_prefers_priority_then_longest_chain() {
        let mut a = mk("asf-1", Status::Ready, &["a.rs"]);
        a.priority = 3;
        let b = mk("asf-2", Status::Ready, &["b.rs"]); // priority 2, has downstream chain
        let c = mk("asf-3", Status::Ready, &["c.rs"]); // priority 2, no chain
        let d = mk("asf-4", Status::Backlog, &["d.rs"]);
        let issues = [a, b, c, d];
        let deps = [dep("asf-4", "asf-2")];
        let t = strs(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        let p = plan(&input(&issues, &deps, &[], &t, 1, &[]), ".worktrees").unwrap();
        assert_eq!(p.slots[0].issue, id("asf-2"));
    }

    #[test]
    fn plan_defers_ready_issue_with_open_deps() {
        let issues = [
            mk("asf-1", Status::Backlog, &["a.rs"]),
            mk("asf-2", Status::Ready, &["b.rs"]),
        ];
        let deps = [dep("asf-2", "asf-1")];
        let t = strs(&["a.rs", "b.rs"]);
        let p = plan(&input(&issues, &deps, &[], &t, 3, &[]), ".worktrees").unwrap();
        assert!(p.slots.is_empty());
        assert_eq!(p.deferred[0].reason, DeferReason::DepsOpen(vec![id("asf-1")]));
    }

    #[test]
    fn plan_skips_epics_and_non_ready() {
        let mut e = mk("asf-1", Status::Ready, &["a.rs"]);
        e.kind = IssueType::Epic;
        let issues = [
            e,
            mk("asf-2", Status::Blocked, &["b.rs"]),
            mk("asf-3", Status::Ready, &["c.rs"]),
        ];
        let t = strs(&["a.rs", "b.rs", "c.rs"]);
        let p = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        let slots: Vec<_> = p.slots.iter().map(|s| s.issue.to_string()).collect();
        assert_eq!(slots, ["asf-3"]);
        assert!(p.deferred.is_empty());
    }

    #[test]
    fn plan_no_scope_alone_is_allowed_but_excludes_others() {
        let issues = [
            mk("asf-1", Status::Ready, &[]),
            mk("asf-2", Status::Ready, &["b.rs"]),
        ];
        let t = strs(&["b.rs"]);
        let p = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        assert_eq!(p.slots.len(), 1);
        assert_eq!(p.slots[0].issue, id("asf-1"));
        assert_eq!(p.deferred[0].reason, DeferReason::ScopeConflict(id("asf-1")));
        let issues = [
            mk("asf-1", Status::Ready, &["b.rs"]),
            mk("asf-2", Status::Ready, &[]),
        ];
        let p = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        assert_eq!(p.deferred[0].reason, DeferReason::NoScope);
    }

    #[test]
    fn plan_is_deterministic() {
        let issues = [
            mk("asf-1", Status::Ready, &["a.rs"]),
            mk("asf-2", Status::Ready, &["b.rs"]),
        ];
        let t = strs(&["a.rs", "b.rs"]);
        let p1 = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        let p2 = plan(&input(&issues, &[], &[], &t, 3, &[]), ".worktrees").unwrap();
        assert_eq!(p1, p2);
        assert_eq!(p1.inputs_hash.len(), 16);
    }

    fn arb_scope() -> impl Strategy<Value = Vec<String>> {
        proptest::collection::vec(
            prop_oneof![
                Just("a.rs"),
                Just("b.rs"),
                Just("c.rs"),
                Just("d/**"),
                Just("d/x.rs"),
                Just("d/y.rs")
            ]
            .prop_map(String::from),
            0..3,
        )
    }

    proptest! {
        #[test]
        fn no_two_slots_overlap(scopes in proptest::collection::vec(arb_scope(), 1..7), max in 1usize..5) {
            let tracked = strs(&["a.rs", "b.rs", "c.rs", "d/x.rs", "d/y.rs"]);
            let issues: Vec<Issue> = scopes.iter().enumerate()
                .map(|(i, s)| { let mut it = mk(&format!("asf-{}", i + 1), Status::Ready, &[]); it.file_scope = s.clone(); it })
                .collect();
            let p = plan(&input(&issues, &[], &[], &tracked, max, &[]), ".worktrees").unwrap();
            prop_assert!(p.slots.len() <= max);
            for (i, a) in p.slots.iter().enumerate() {
                for b in &p.slots[i + 1..] {
                    prop_assert!(!scopes_conflict(&a.scope, &b.scope, &tracked, &[]).unwrap(), "{} vs {}", a.issue, b.issue);
                }
            }
            let p2 = plan(&input(&issues, &[], &[], &tracked, max, &[]), ".worktrees").unwrap();
            prop_assert_eq!(p, p2);
        }

        /// Second guard behind `dep add` demotion: whatever the stored status says, an issue with a
        /// dependency that is not `done` never gets a slot, and a ready one is deferred as `DepsOpen`.
        #[test]
        fn no_slot_for_an_issue_with_an_open_dependency(
            statuses in proptest::collection::vec(
                prop_oneof![
                    Just(Status::Backlog), Just(Status::Ready), Just(Status::InProgress), Just(Status::Blocked),
                    Just(Status::InReview), Just(Status::Validating), Just(Status::Done), Just(Status::Cancelled)
                ],
                2..8,
            ),
            edges in proptest::collection::vec((0usize..8, 0usize..8), 0..14),
            max in 1usize..5,
        ) {
            let n = statuses.len();
            let issues: Vec<Issue> = statuses.iter().enumerate()
                .map(|(i, s)| mk(&format!("asf-{}", i + 1), *s, &[&format!("f{i}.rs")]))
                .collect();
            let tracked: Vec<String> = (0..n).map(|i| format!("f{i}.rs")).collect();
            // an edge always points at a lower index, so the graph stays acyclic
            let deps: Vec<Dep> = edges.iter()
                .map(|&(a, b)| (a % n, b % n))
                .filter(|(a, b)| a > b)
                .map(|(a, b)| dep(&format!("asf-{}", a + 1), &format!("asf-{}", b + 1)))
                .collect();
            let p = plan(&input(&issues, &deps, &[], &tracked, max, &[]), ".worktrees").unwrap();
            let open = |who: &IssueId| deps.iter().any(|d| {
                &d.issue_id == who && issues.iter().any(|i| i.id == d.depends_on && i.status != Status::Done)
            });
            for s in &p.slots {
                prop_assert!(!open(&s.issue), "{} was planned with an open dependency", s.issue);
            }
            for i in issues.iter().filter(|i| i.status == Status::Ready && open(&i.id)) {
                prop_assert!(
                    p.deferred.iter().any(|d| d.issue == i.id && matches!(d.reason, DeferReason::DepsOpen(_))),
                    "{} should be deferred as DepsOpen", i.id
                );
            }
        }
    }
}
