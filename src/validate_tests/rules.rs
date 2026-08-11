use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::config::TestPolicyConfig;
use super::model::{
    AnalysisGap, AppliedExemption, Evidence, Finding, FindingCategory, HygieneKind, Marker,
    MarkerKind, OracleFact, OracleKind, Remediation, Section, SourceLocation, SourceSpan, TestCase,
    TestIdentity, REPORT_SCHEMA_VERSION,
};

pub const NAMING_SHOULD_PREFIX: &str = "naming.should-prefix";
pub const AAA_MISSING_ARRANGE: &str = "structure.aaa.missing-arrange";
pub const AAA_MISSING_ACT: &str = "structure.aaa.missing-act";
pub const AAA_MISSING_ASSERT: &str = "structure.aaa.missing-assert";
pub const AAA_OUT_OF_ORDER: &str = "structure.aaa.out-of-order";
pub const AAA_COMBINED: &str = "structure.aaa.combined";
pub const MULTIPLE_ACTS: &str = "behavior.multiple-acts";
pub const IGNORE_MISSING_REASON: &str = "ignore.missing-reason";
pub const ORACLE_MISSING: &str = "oracle.missing";
pub const ORACLE_VACUOUS: &str = "oracle.vacuous";
pub const ORACLE_DISCONNECTED: &str = "oracle.disconnected";
pub const PANIC_MISSING_EXPECTED: &str = "panic.missing-expected";
pub const FIXED_SLEEP: &str = "nondeterminism.fixed-sleep";
pub const WALL_CLOCK: &str = "nondeterminism.clock";
pub const RANDOM: &str = "nondeterminism.rng";
pub const ENVIRONMENT: &str = "nondeterminism.environment";
pub const SELF_DERIVED_INTENT_GAP: &str = "analysis.intent.self-derived-expected";
pub const ERROR_UNINSPECTED_INTENT_GAP: &str = "analysis.intent.error-uninspected";
pub const INTERACTION_ONLY_INTENT_GAP: &str = "analysis.intent.interaction-only";

pub struct Evaluation {
    pub findings: Vec<Finding>,
    pub exemptions: Vec<AppliedExemption>,
    pub analysis_gaps: Vec<AnalysisGap>,
}

pub fn evaluate(tests: &[TestCase], policy: &TestPolicyConfig) -> Evaluation {
    let mut findings = Vec::new();
    let mut exemptions = Vec::new();
    let mut analysis_gaps = Vec::new();
    for test in tests {
        analysis_gaps.extend(evaluate_intent_gaps(test));
        let mut test_findings = evaluate_test(test, policy);
        test_findings.sort_by(|left, right| left.rule_id.cmp(&right.rule_id));
        for finding in test_findings {
            if let Some(exemption) =
                policy.exemption(&finding.rule_id, &test.path, &test.qualified_name)
            {
                exemptions.push(AppliedExemption {
                    rule_id: finding.rule_id,
                    test: identity(test),
                    reason: exemption.reason.clone(),
                    fingerprint: finding.fingerprint,
                });
            } else {
                findings.push(finding);
            }
        }
    }
    findings.sort_by(|left, right| {
        left.primary
            .path
            .cmp(&right.primary.path)
            .then_with(|| left.test.qualified_name.cmp(&right.test.qualified_name))
            .then_with(|| left.rule_id.cmp(&right.rule_id))
            .then_with(|| {
                left.primary
                    .span
                    .start
                    .line
                    .cmp(&right.primary.span.start.line)
            })
    });
    exemptions.sort_by(|left, right| {
        left.test
            .qualified_name
            .cmp(&right.test.qualified_name)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
    });
    analysis_gaps.sort_by(|left, right| {
        left.location
            .path
            .cmp(&right.location.path)
            .then_with(|| left.test.qualified_name.cmp(&right.test.qualified_name))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| {
                (
                    left.location.span.start.line,
                    left.location.span.start.column,
                )
                    .cmp(&(
                        right.location.span.start.line,
                        right.location.span.start.column,
                    ))
            })
    });
    Evaluation {
        findings,
        exemptions,
        analysis_gaps,
    }
}

fn evaluate_test(test: &TestCase, policy: &TestPolicyConfig) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !test.name.starts_with("should_") {
        findings.push(finding(
            test,
            NAMING_SHOULD_PREFIX,
            FindingCategory::Structure,
            test.name_span,
            "test name does not start with `should_`",
            Vec::new(),
            "Behavior-oriented names make the promised outcome visible in failures and reports.",
            "Rename the test around one observable behavior.",
            vec!["Use `should_<behavior>_given_<context>_when_<condition>` where useful."],
            Some("fn should_return_not_found_given_missing_key() { ... }"),
        ));
    }

    if test.line_count > policy.aaa_min_lines {
        evaluate_aaa(test, &mut findings);
    }

    let act_markers = markers(test, MarkerKind::Act);
    if act_markers.len() > 1 {
        let mut act_evidence = marker_evidence(test, &act_markers, "act", "Act section");
        act_evidence.extend(
            test.calls
                .iter()
                .filter(|call| call.section == Section::Act)
                .map(|call| {
                    evidence(
                        test,
                        "call",
                        call.span,
                        &format!("Act-section call `{}`.", call.qualified_name),
                    )
                }),
        );
        findings.push(finding(
            test,
            MULTIPLE_ACTS,
            FindingCategory::Structure,
            act_markers[1].span,
            "test has more than one Act section",
            act_evidence,
            "Multiple independent Acts make it unclear which behavior a failure proves.",
            "Split the Acts into separate single-behavior tests.",
            vec![
                "Keep one production stimulus and its directly related observations in each test.",
            ],
            None,
        ));
    }

    if test
        .ignored
        .as_deref()
        .is_some_and(|reason| reason.trim().is_empty())
    {
        findings.push(finding(
            test,
            IGNORE_MISSING_REASON,
            FindingCategory::Hygiene,
            test.ignore_span.unwrap_or(test.name_span),
            "ignored test has no reason",
            Vec::new(),
            "An unexplained ignored test can silently remove intended coverage indefinitely.",
            "Add a concrete reason to the ignore attribute.",
            vec!["Use `#[ignore = \"requires the Sqrzl emulator\"]`."],
            Some("#[ignore = \"requires external fixture\"]"),
        ));
    }

    let meaningful_oracles = post_act_oracles(test);
    if meaningful_oracles.is_empty() && !test.should_panic && !has_unresolved_oracle_gap(test) {
        findings.push(finding(
            test,
            ORACLE_MISSING,
            FindingCategory::Semantics,
            test.function_span,
            "test has no observable oracle",
            Vec::new(),
            "The test can execute production code without proving any result or state transition.",
            "Assert the Act result or query and assert an observable post-Act state.",
            vec!["Add an explicit assertion, a resolved assertion helper, or a success-propagating `?` tied to the Act."],
            Some("assert_eq!(actual, expected);"),
        ));
    }

    let vacuous: Vec<_> = test
        .oracles
        .iter()
        .filter(|oracle| oracle.tautological)
        .collect();
    if let Some(first) = vacuous.first() {
        findings.push(finding(
            test,
            ORACLE_VACUOUS,
            FindingCategory::Semantics,
            first.span,
            "assertion is tautological",
            vacuous
                .iter()
                .map(|oracle| {
                    evidence(
                        test,
                        "oracle",
                        oracle.span,
                        "This assertion cannot distinguish behavior.",
                    )
                })
                .collect(),
            "An always-true or self-comparison oracle passes regardless of production behavior.",
            "Compare an Act-derived actual value with an independently chosen expectation.",
            vec!["Replace constants or self-comparisons with an observable derived from the Act."],
            Some("assert_eq!(actual, independently_defined_expected);"),
        ));
    }

    if should_report_disconnected(test) {
        let disconnected_oracles = disconnected_candidates(test);
        let primary = disconnected_oracles
            .first()
            .map_or(test.function_span, |oracle| oracle.span);
        let mut evidence_items = Vec::new();
        if let Some(marker) = act_markers.first() {
            evidence_items.push(evidence(test, "act", marker.span, "The Act begins here."));
        }
        for oracle in disconnected_oracles {
            evidence_items.push(evidence(
                test,
                "oracle",
                oracle.span,
                "This resolved post-Act oracle does not reference an Act output or effect carrier.",
            ));
        }
        findings.push(finding(
            test,
            ORACLE_DISCONNECTED,
            FindingCategory::Semantics,
            primary,
            "resolved post-Act assertions do not observe the Act",
            evidence_items,
            "The test can keep passing when the behavior under test returns the wrong value or fails to change state.",
            "Assert an Act output or read post-Act state through the same receiver or effect carrier.",
            vec!["Use the value bound in Act, or query state using an object/path passed to Act."],
            Some("assert_eq!(result, expected);"),
        ));
    }

    if test.should_panic
        && test
            .should_panic_expected
            .as_deref()
            .is_none_or(|expected| expected.trim().is_empty())
    {
        findings.push(finding(
            test,
            PANIC_MISSING_EXPECTED,
            FindingCategory::Semantics,
            test.function_span,
            "panic test accepts any panic",
            Vec::new(),
            "A setup panic or unrelated defect can satisfy a bare `#[should_panic]` test.",
            "Declare the stable expected panic text or prefer an explicit error assertion.",
            vec!["Use `#[should_panic(expected = \"specific invariant\")]`."],
            Some("#[should_panic(expected = \"capacity must be positive\")]"),
        ));
    }

    evaluate_hygiene(test, &mut findings);
    deduplicate(findings)
}

fn evaluate_intent_gaps(test: &TestCase) -> Vec<AnalysisGap> {
    let mut gaps = Vec::new();

    if let Some(oracle) = test
        .oracles
        .iter()
        .find(|oracle| oracle_is_self_derived(test, oracle))
    {
        gaps.push(intent_gap(
            test,
            SELF_DERIVED_INTENT_GAP,
            oracle.span,
            "expected value repeats the Act invocation; construct it independently unless this test intentionally proves determinism",
        ));
    }

    if let Some(oracle) = test
        .oracles
        .iter()
        .filter(|oracle| is_post_act_oracle(test, oracle))
        .filter(|oracle| !oracle.conditional && oracle.kind == OracleKind::BroadError)
        .find(|broad| !has_precise_error_oracle(test, broad))
    {
        gaps.push(intent_gap(
            test,
            ERROR_UNINSPECTED_INTENT_GAP,
            oracle.span,
            "only error presence is checked; inspect a stable variant, code, or diagnostic when the error type carries distinguishable failures",
        ));
    }

    let post_act_oracles = post_act_oracles(test);
    if !post_act_oracles.is_empty()
        && post_act_oracles
            .iter()
            .all(|oracle| oracle.kind == OracleKind::Interaction)
    {
        gaps.push(intent_gap(
            test,
            INTERACTION_ONLY_INTENT_GAP,
            post_act_oracles[0].span,
            "only configured mock interactions are verified; add a public outcome when the interaction itself is not the contract",
        ));
    }

    gaps
}

fn intent_gap(test: &TestCase, code: &str, span: SourceSpan, message: &str) -> AnalysisGap {
    AnalysisGap {
        code: code.to_string(),
        test: identity(test),
        message: message.to_string(),
        location: location(test, span, "intent requires review"),
    }
}

fn evaluate_aaa(test: &TestCase, findings: &mut Vec<Finding>) {
    for (kind, rule_id, label) in [
        (MarkerKind::Arrange, AAA_MISSING_ARRANGE, "Arrange"),
        (MarkerKind::Act, AAA_MISSING_ACT, "Act"),
        (MarkerKind::Assert, AAA_MISSING_ASSERT, "Assert"),
    ] {
        let matching = markers(test, kind);
        let section = marker_section(kind);
        let empty = matching.len() == 1
            && test
                .section_statements
                .get(&section)
                .copied()
                .unwrap_or_default()
                == 0;
        if matching.is_empty() || empty {
            findings.push(finding(
                test,
                rule_id,
                FindingCategory::Structure,
                matching.first().map_or(test.function_span, |marker| marker.span),
                if empty {
                    format!("`// {label}` section contains no code")
                } else {
                    format!("missing `// {label}` section")
                },
                Vec::new(),
                "Visible AAA boundaries keep setup, the production stimulus, and observations reviewable.",
                &format!("Add one nonempty `// {label}` section."),
                vec!["Keep the markers in Arrange, Act, Assert order."],
                None,
            ));
        }
    }

    let combined: Vec<_> = test
        .markers
        .iter()
        .filter(|marker| marker.combined)
        .collect();
    if let Some(first) = combined.first() {
        findings.push(finding(
            test,
            AAA_COMBINED,
            FindingCategory::Structure,
            first.span,
            "AAA marker combines sections or contains extra text",
            marker_evidence(test, &combined, "marker", "Combined AAA marker"),
            "Combined markers obscure where the production stimulus ends and observation begins.",
            "Use exact standalone `// Arrange`, `// Act`, and `// Assert` markers.",
            vec!["Move explanatory comments beneath the relevant exact marker."],
            None,
        ));
    }

    let arrange = markers(test, MarkerKind::Arrange);
    let act = markers(test, MarkerKind::Act);
    let assert = markers(test, MarkerKind::Assert);
    let wrong_count = arrange.len() > 1 || assert.len() > 1;
    let wrong_order = arrange
        .first()
        .zip(act.first())
        .zip(assert.first())
        .is_some_and(|((arrange, act), assert)| {
            !(arrange.span.start.line < act.span.start.line
                && act.span.start.line < assert.span.start.line)
        });
    if wrong_count || wrong_order {
        findings.push(finding(
            test,
            AAA_OUT_OF_ORDER,
            FindingCategory::Structure,
            test.function_span,
            "AAA sections must appear exactly once in Arrange, Act, Assert order",
            marker_evidence(
                test,
                &test.markers.iter().collect::<Vec<_>>(),
                "marker",
                "AAA marker",
            ),
            "Duplicated or reordered sections make causality ambiguous.",
            "Keep one ordered marker for each section.",
            vec!["Reorder the test without combining independent behaviors."],
            None,
        ));
    }
}

fn evaluate_hygiene(test: &TestCase, findings: &mut Vec<Finding>) {
    for (kind, rule_id, message, why, summary) in [
        (
            HygieneKind::FixedSleep,
            FIXED_SLEEP,
            "test uses a fixed sleep",
            "Fixed delays make tests slow and timing-dependent without proving readiness.",
            "Use virtual time, an injected clock, or wait for an observable readiness signal.",
        ),
        (
            HygieneKind::UncontrolledClock,
            WALL_CLOCK,
            "Act or assertion reads the uncontrolled wall clock",
            "Wall-clock scheduling and resolution can make the asserted outcome nondeterministic.",
            "Inject or pause the clock and advance it deterministically.",
        ),
        (
            HygieneKind::UncontrolledRandom,
            RANDOM,
            "test uses an uncontrolled random generator",
            "An unrecorded random input makes failures difficult to reproduce and can leave cases untested.",
            "Use a fixed seed or a property-test framework that reports and replays the seed.",
        ),
        (
            HygieneKind::ProcessEnvironment,
            ENVIRONMENT,
            "test mutates process-global environment directly",
            "A panic or parallel test can observe leaked process state.",
            "Use a configured panic-safe RAII environment guard.",
        ),
    ] {
        let matching: Vec<_> = test
            .hygiene
            .iter()
            .filter(|fact| fact.kind == kind)
            .filter(|fact| kind != HygieneKind::UncontrolledClock || clock_influences_test(test, fact))
            .collect();
        if let Some(first) = matching.first() {
            findings.push(finding(
                test,
                rule_id,
                FindingCategory::Hygiene,
                first.span,
                message,
                matching
                    .iter()
                    .map(|fact| evidence(test, "call", fact.span, &format!("Uncontrolled call `{}`.", fact.call)))
                    .collect(),
                why,
                summary,
                vec!["If this behavior is intentional and cannot be controlled, add one exact reasoned exemption."],
                None,
            ));
        }
    }
}

fn should_report_disconnected(test: &TestCase) -> bool {
    if markers(test, MarkerKind::Act).len() != 1 || has_causality_gap(test) {
        return false;
    }
    let considered = disconnected_candidates(test);
    !considered.is_empty()
        && considered.iter().all(|oracle| {
            if oracle.section == Section::Act {
                return false;
            }
            let identifiers = oracle_identifiers(oracle);
            identifiers.is_disjoint(&test.act_outputs)
                && identifiers.is_disjoint(&test.act_effects)
                && !has_indirect_causality_gap(test, oracle)
        })
}

fn disconnected_candidates(test: &TestCase) -> Vec<&OracleFact> {
    let outcomes = outcome_oracles(test);
    let asserted: Vec<_> = outcomes
        .iter()
        .copied()
        .filter(|oracle| oracle.section == Section::Assert)
        .collect();
    if asserted.is_empty() {
        outcomes
    } else {
        asserted
    }
}

fn oracle_is_self_derived(test: &TestCase, oracle: &OracleFact) -> bool {
    oracle.self_derived_candidate
        && !oracle.conditional
        && !oracle.tautological
        && is_post_act_oracle(test, oracle)
        && !has_causality_gap(test)
        && markers(test, MarkerKind::Act).len() == 1
        && test.act_root_calls.len() == 1
        && ((identifiers_observe_act(&oracle.actual_identifiers, test)
            && oracle
                .expected_root_call
                .as_ref()
                .is_some_and(|call| test.act_root_calls.contains(call)))
            || (identifiers_observe_act(&oracle.expected_identifiers, test)
                && oracle
                    .actual_root_call
                    .as_ref()
                    .is_some_and(|call| test.act_root_calls.contains(call))))
}

fn post_act_oracles(test: &TestCase) -> Vec<&OracleFact> {
    test.oracles
        .iter()
        .filter(|oracle| is_post_act_oracle(test, oracle) && !oracle.conditional)
        .collect()
}

fn outcome_oracles(test: &TestCase) -> Vec<&OracleFact> {
    post_act_oracles(test)
        .into_iter()
        .filter(|oracle| oracle.kind != OracleKind::Interaction)
        .collect()
}

fn is_post_act_oracle(test: &TestCase, oracle: &OracleFact) -> bool {
    matches!(oracle.section, Section::Act | Section::Assert)
        || (test.markers.is_empty() && oracle.section == Section::Body)
}

fn oracle_identifiers(oracle: &OracleFact) -> BTreeSet<String> {
    oracle
        .actual_identifiers
        .union(&oracle.expected_identifiers)
        .cloned()
        .collect()
}

fn identifiers_observe_act(identifiers: &BTreeSet<String>, test: &TestCase) -> bool {
    !identifiers.is_disjoint(&test.act_outputs) || !identifiers.is_disjoint(&test.act_effects)
}

fn has_unresolved_oracle_gap(test: &TestCase) -> bool {
    test.gaps.iter().any(|gap| {
        matches!(
            gap.code.as_str(),
            "analysis.conditional-oracle"
                | "analysis.partially-parsed-assertion"
                | "analysis.unresolved-assertion-helper"
                | "analysis.unresolved-scenario-delegate"
                | "analysis.unsupported-macro"
        ) && gap_is_post_act(test, gap)
    })
}

fn gap_is_post_act(test: &TestCase, gap: &AnalysisGap) -> bool {
    if test.markers.is_empty() {
        return true;
    }
    test.markers
        .iter()
        .filter(|marker| position_at_or_before(marker.span.start, gap.location.span.start))
        .max_by_key(|marker| (marker.span.start.line, marker.span.start.column))
        .is_some_and(|marker| matches!(marker.kind, MarkerKind::Act | MarkerKind::Assert))
}

fn has_causality_gap(test: &TestCase) -> bool {
    test.gaps.iter().any(|gap| {
        matches!(
            gap.code.as_str(),
            "analysis.shadowed-binding"
                | "analysis.pattern-causality"
                | "analysis.unresolved-act-computation"
        )
    })
}

fn has_indirect_causality_gap(test: &TestCase, oracle: &OracleFact) -> bool {
    test.gaps.iter().any(|gap| {
        (matches!(
            gap.code.as_str(),
            "analysis.indirect-causality"
                | "analysis.receiver-effect-causality"
                | "analysis.shared-reference-effect-causality"
        ) && gap.location.span == oracle.span)
            || (matches!(
                gap.code.as_str(),
                "analysis.partially-parsed-assertion"
                    | "analysis.unsupported-macro"
                    | "analysis.implicit-format-capture"
            ) && span_contains(oracle.span, gap.location.span))
    })
}

fn has_precise_error_oracle(test: &TestCase, broad: &OracleFact) -> bool {
    let mut broad_identifiers = oracle_identifiers(broad);
    broad_identifiers.extend(broad.produced_identifiers.iter().cloned());
    loop {
        let previous_len = broad_identifiers.len();
        let known_identifiers = broad_identifiers.clone();
        let extracted_identifiers: Vec<_> = test
            .oracles
            .iter()
            .filter(|oracle| {
                is_post_act_oracle(test, oracle)
                    && !oracle.conditional
                    && oracle.kind == OracleKind::BroadError
                    && !oracle_identifiers(oracle).is_disjoint(&known_identifiers)
            })
            .flat_map(|oracle| oracle.produced_identifiers.iter().cloned())
            .collect();
        for identifier in extracted_identifiers {
            broad_identifiers.insert(identifier);
        }
        if broad_identifiers.len() == previous_len {
            break;
        }
    }
    !broad_identifiers.is_empty()
        && test.oracles.iter().any(|oracle| {
            is_post_act_oracle(test, oracle)
                && !oracle.conditional
                && oracle.kind == OracleKind::Outcome
                && !oracle.tautological
                && !broad_identifiers.is_disjoint(&oracle_identifiers(oracle))
        })
}

fn clock_influences_test(test: &TestCase, fact: &super::model::HygieneFact) -> bool {
    if fact.in_oracle {
        return true;
    }
    if test.oracles.iter().any(|oracle| {
        !oracle.conditional
            && !oracle_identifiers(oracle).is_disjoint(&test.uncontrolled_clock_outputs)
    }) {
        return true;
    }
    fact.section == Section::Act
        && test.calls.iter().any(|call| {
            call.section == Section::Act
                && call.span != fact.span
                && span_contains(call.span, fact.span)
        })
}

fn span_contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    position_at_or_before(outer.start, inner.start) && position_at_or_before(inner.end, outer.end)
}

fn position_at_or_before(left: super::model::Position, right: super::model::Position) -> bool {
    (left.line, left.column) <= (right.line, right.column)
}

fn markers(test: &TestCase, kind: MarkerKind) -> Vec<&Marker> {
    test.markers
        .iter()
        .filter(|marker| marker.kind == kind)
        .collect()
}

const fn marker_section(kind: MarkerKind) -> Section {
    match kind {
        MarkerKind::Arrange => Section::Arrange,
        MarkerKind::Act => Section::Act,
        MarkerKind::Assert => Section::Assert,
    }
}

fn marker_evidence(
    test: &TestCase,
    markers: &[&Marker],
    kind: &str,
    message: &str,
) -> Vec<Evidence> {
    markers
        .iter()
        .map(|marker| evidence(test, kind, marker.span, message))
        .collect()
}

fn evidence(test: &TestCase, kind: &str, span: SourceSpan, message: &str) -> Evidence {
    Evidence {
        kind: kind.to_string(),
        message: message.to_string(),
        location: location(test, span, message),
    }
}

#[allow(clippy::too_many_arguments)]
fn finding(
    test: &TestCase,
    rule_id: &str,
    category: FindingCategory,
    span: SourceSpan,
    message: impl Into<String>,
    evidence: Vec<Evidence>,
    why_it_matters: &str,
    remediation_summary: &str,
    actions: Vec<&str>,
    example: Option<&str>,
) -> Finding {
    Finding {
        rule_id: rule_id.to_string(),
        category,
        severity: "error",
        confidence: "high",
        fingerprint: fingerprint(rule_id, &test.path, &test.qualified_name),
        test: identity(test),
        message: message.into(),
        primary: location(test, span, "primary finding"),
        evidence,
        why_it_matters: why_it_matters.to_string(),
        remediation: Remediation {
            summary: remediation_summary.to_string(),
            actions: actions.into_iter().map(str::to_string).collect(),
            example: example.map(str::to_string),
            rerun: format!("cntryl-tools validate-tests -f {}", shell_quote(&test.path)),
        },
    }
}

fn location(test: &TestCase, span: SourceSpan, label: &str) -> SourceLocation {
    SourceLocation {
        path: test.path.clone(),
        span,
        label: label.to_string(),
    }
}

fn identity(test: &TestCase) -> TestIdentity {
    TestIdentity {
        name: test.name.clone(),
        qualified_name: test.qualified_name.clone(),
    }
}

fn fingerprint(rule_id: &str, path: &str, qualified_test: &str) -> String {
    let canonical = format!(
        "{REPORT_SCHEMA_VERSION}\0{rule_id}\0{}\0{qualified_test}",
        path.replace('\\', "/")
    );
    format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn deduplicate(findings: Vec<Finding>) -> Vec<Finding> {
    let mut by_rule = BTreeMap::<String, Finding>::new();
    for finding in findings {
        by_rule
            .entry(finding.rule_id.clone())
            .and_modify(|existing| existing.evidence.extend(finding.evidence.clone()))
            .or_insert(finding);
    }
    by_rule.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate, ENVIRONMENT, ERROR_UNINSPECTED_INTENT_GAP, FIXED_SLEEP, IGNORE_MISSING_REASON,
        INTERACTION_ONLY_INTENT_GAP, NAMING_SHOULD_PREFIX, ORACLE_DISCONNECTED, ORACLE_MISSING,
        ORACLE_VACUOUS, PANIC_MISSING_EXPECTED, RANDOM, SELF_DERIVED_INTENT_GAP, WALL_CLOCK,
    };
    use crate::validate_tests::config::{TestExemption, TestPolicyConfig};
    use crate::validate_tests::rust::analyze_source;

    fn findings(source: &str) -> Vec<String> {
        let policy = TestPolicyConfig::default();
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        evaluate(&tests, &policy)
            .findings
            .into_iter()
            .map(|finding| finding.rule_id)
            .collect()
    }

    fn analysis_codes(source: &str) -> Vec<String> {
        let policy = TestPolicyConfig::default();
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let mut codes: Vec<_> = tests
            .iter()
            .flat_map(|test| test.gaps.iter().map(|gap| gap.code.clone()))
            .collect();
        codes.extend(
            evaluate(&tests, &policy)
                .analysis_gaps
                .into_iter()
                .map(|gap| gap.code),
        );
        codes
    }

    #[test]
    fn should_reject_disconnected_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_return_created() {
    // Arrange
    let expected = 201;
    // Act
    let response = create_record();
    // Assert
    assert_eq!(expected, 201);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_accept_oracle_connected_to_act_result() {
        // Arrange
        let source = r#"
#[test]
fn should_return_created() {
    // Arrange
    let expected = 201;
    // Act
    let response = create_record();
    // Assert
    assert_eq!(response.status(), expected);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_accept_oracle_connected_through_act_assignment() {
        // Arrange
        let source = r#"
#[test]
fn should_return_created() {
    // Arrange
    let mut response = 0;
    // Act
    response = create_record();
    // Assert
    assert_eq!(response, 201);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_treat_equality_operands_symmetrically_for_causality() {
        // Arrange
        let source = r#"
#[test]
fn should_return_created() {
    // Arrange
    let expected = 201;
    // Act
    let actual = create_record();
    // Assert
    assert_eq!(expected, actual);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_connect_named_snapshot_to_the_act_output() {
        // Arrange
        let source = r#"
#[test]
fn should_match_snapshot() {
    // Arrange
    let snapshot_name = "created";
    // Act
    let actual = create_record();
    // Assert
    assert_snapshot!(snapshot_name, actual);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_not_treat_arrange_precondition_as_post_act_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_mutate_state() {
    // Arrange
    let mut state = Vec::new();
    assert!(state.is_empty());
    // Act
    mutate(&mut state);
    // Assert
    log_completion();
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_MISSING));
    }

    #[test]
    fn should_not_treat_immutable_act_argument_as_effect_carrier() {
        // Arrange
        let source = r#"
#[test]
fn should_transform_input() {
    // Arrange
    let input = 42;
    // Act
    let actual = transform(input);
    // Assert
    assert_eq!(input, 42);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_not_treat_constant_bound_in_act_as_production_output() {
        // Arrange
        let source = r#"
#[test]
fn should_create_record() {
    // Arrange
    let expected = 201;
    // Act
    create_record();
    let unrelated = expected;
    // Assert
    assert_eq!(unrelated, 201);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_connect_mutable_act_argument_as_effect_carrier() {
        // Arrange
        let source = r#"
#[test]
fn should_append_value() {
    // Arrange
    let mut state = Vec::new();
    // Act
    append_value(&mut state);
    // Assert
    assert_eq!(state.len(), 1);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_leave_method_receiver_effect_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_append_value() {
    // Arrange
    let mut state = Vec::new();
    // Act
    state.push(42);
    // Assert
    assert_eq!(state.len(), 1);
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert!(tests[0]
            .gaps
            .iter()
            .any(|gap| gap.code == "analysis.receiver-effect-causality"));
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_not_let_unrelated_gap_hide_resolved_disconnected_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_return_created() {
    // Arrange
    let expected = 201;
    // Act
    let actual = create_record();
    // Assert
    diagnostic!();
    assert_eq!(expected, 201);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_scope_conditional_oracle_uncertainty_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_observe_optional_result() {
    // Arrange
    let enabled = false;
    // Act
    let actual = operation();
    // Assert
    if enabled {
        assert_eq!(actual, 42);
    }
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert!(tests[0]
            .gaps
            .iter()
            .any(|gap| gap.code == "analysis.conditional-oracle"));
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == ORACLE_MISSING));
    }

    #[test]
    fn should_reject_vacuous_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_validate_result() {
    assert!(true);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_VACUOUS));
    }

    #[test]
    fn should_reject_boolean_self_comparison_but_not_assert_ne() {
        // Arrange
        let self_comparison = r#"
#[test]
fn should_validate_result() {
    assert!(true == true);
}
"#;
        let assert_ne = r#"
#[test]
fn should_validate_distinct_results() {
    let first = operation();
    let second = another_operation();
    assert_ne!(first, second);
}
"#;

        // Act
        let self_comparison_rules = findings(self_comparison);
        let assert_ne_rules = findings(assert_ne);

        // Assert
        assert!(self_comparison_rules
            .iter()
            .any(|rule| rule == ORACLE_VACUOUS));
        assert!(!assert_ne_rules.iter().any(|rule| rule == ORACLE_VACUOUS));
    }

    #[test]
    fn should_leave_local_self_comparison_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_validate_result() {
    let actual = operation();
    assert!(actual == actual);
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert!(tests[0]
            .gaps
            .iter()
            .any(|gap| gap.code == "analysis.possible-self-comparison"));
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == ORACLE_VACUOUS));
    }

    #[test]
    fn should_not_call_repeated_effectful_expressions_vacuous() {
        // Arrange
        let source = r#"
#[test]
fn should_compare_two_queue_items() {
    assert_eq!(queue.pop(), queue.pop());
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_VACUOUS));
    }

    #[test]
    fn should_reject_uninspected_error() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    assert!(load_artifact().is_err());
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_reject_wildcard_error_match() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    let result = load_artifact();
    assert!(matches!(result, Err(_)));
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_reject_wildcard_error_match_statement() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    // Arrange
    let path = "missing";
    // Act
    let result = load_artifact(path);
    // Assert
    match result {
        Err(_) => {}
        Ok(_) => panic!("expected an error"),
    }
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_accept_error_payload_inspected_inside_match_arm() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    // Arrange
    let path = "missing";
    // Act
    let result = load_artifact(path);
    // Assert
    match result {
        Err(error) => assert_eq!(error.code(), "missing_schema"),
        Ok(_) => panic!("expected an error"),
    }
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(!codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_not_let_unrelated_outcome_hide_broad_error_check() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    // Arrange
    let unrelated = 1;
    // Act
    let result = load_artifact();
    // Assert
    assert!(result.is_err());
    assert_eq!(unrelated, 1);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_accept_precise_error_check_for_same_result() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_schema() {
    // Arrange
    let path = "missing";
    // Act
    let result = load_artifact(path);
    // Assert
    assert!(result.is_err());
    assert!(matches!(result, Err(Error::MissingSchema)));
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(!codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_reject_literal_fixed_sleep() {
        // Arrange
        let source = r#"
#[test]
fn should_wait_for_ready() {
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(service_is_ready());
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == FIXED_SLEEP));
    }

    #[test]
    fn should_not_flag_unresolved_domain_apis_as_nondeterministic() {
        // Arrange
        let source = r#"
#[test]
fn should_observe_domain_state() {
    animal.sleep(50);
    let observed_at = FakeInstant::now();
    EnvGuard::set_var("MODE", "test");
    assert!(observed_at.is_recorded());
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(!rules.iter().any(|rule| rule == WALL_CLOCK));
        assert!(!rules.iter().any(|rule| rule == ENVIRONMENT));
    }

    #[test]
    fn should_not_flag_watchdog_deadline_that_is_not_asserted() {
        // Arrange
        let source = r#"
#[test]
fn should_wait_until_ready() {
    // Arrange
    let mut ready = false;
    // Act
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if service_is_ready() {
            ready = true;
            break;
        }
    }
    // Assert
    assert!(ready);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == WALL_CLOCK));
    }

    #[test]
    fn should_reject_test_without_observable_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_write_record() {
    write_record();
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_MISSING));
    }

    #[test]
    fn should_mark_markerless_call_after_setup_check_as_partial() {
        // Arrange
        let source = r#"
#[test]
fn should_run_operation() {
    fixture().unwrap();
    operation();
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");

        // Assert
        assert!(tests[0]
            .gaps
            .iter()
            .any(|gap| gap.code == "analysis.ambiguous-markerless-phase"));
    }

    #[test]
    fn should_mark_call_after_act_execution_check_as_partial() {
        // Arrange
        let source = r#"
#[test]
fn should_run_operation() {
    // Arrange
    let fixture = fixture();
    // Act
    fixture.unwrap();
    operation();
    // Assert
    log_completion();
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == "analysis.ambiguous-act-causality"));
    }

    #[test]
    fn should_mark_later_unobserved_act_call_as_partial() {
        // Arrange
        let source = r#"
#[test]
fn should_run_operation() {
    // Arrange
    let expected = 42;
    // Act
    let actual = operation();
    unobserved_operation();
    // Assert
    assert_eq!(actual, expected);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == "analysis.unobserved-later-act-call"));
    }

    #[test]
    fn should_report_unknown_assertion_helper_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_validate_domain_value() {
    let actual = operation();
    assert_domain_value(actual);
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert_eq!(
            tests[0].gaps[0].code,
            "analysis.unresolved-assertion-helper"
        );
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == ORACLE_MISSING));
    }

    #[test]
    fn should_reject_unexplained_ignore_and_bare_should_panic() {
        // Arrange
        let source = r#"
#[test]
#[ignore]
#[should_panic]
fn should_panic_later() {
    panic!("specific invariant");
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == IGNORE_MISSING_REASON));
        assert!(rules.iter().any(|rule| rule == PANIC_MISSING_EXPECTED));
    }

    #[test]
    fn should_reject_expected_value_built_with_act_callable() {
        // Arrange
        let source = r#"
#[test]
fn should_normalize_value() {
    // Arrange
    let input = " Value ";
    let want = normalize(input);
    // Act
    let actual = normalize(input);
    // Assert
    assert_eq!(actual, want);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes.iter().any(|code| code == SELF_DERIVED_INTENT_GAP));
    }

    #[test]
    fn should_reject_self_derived_expected_with_reversed_equality_operands() {
        // Arrange
        let source = r#"
#[test]
fn should_normalize_value() {
    // Arrange
    let input = " Value ";
    let expected = normalize(input);
    // Act
    let actual = normalize(input);
    // Assert
    assert_eq!(expected, actual);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes.iter().any(|code| code == SELF_DERIVED_INTENT_GAP));
    }

    #[test]
    fn should_allow_same_callable_with_independent_arguments() {
        // Arrange
        let source = r#"
#[test]
fn should_normalize_equivalent_values() {
    // Arrange
    let expected = normalize("value");
    // Act
    let actual = normalize(" value ");
    // Assert
    assert_eq!(actual, expected);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(!codes.iter().any(|code| code == SELF_DERIVED_INTENT_GAP));
    }

    #[test]
    fn should_report_explicit_determinism_comparison_as_intent_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_be_deterministic() {
    // Arrange
    let input = "value";
    let first = normalize(input);
    // Act
    let second = normalize(input);
    // Assert
    assert_eq!(first, second);
}
"#;

        // Act
        let codes = analysis_codes(source);

        // Assert
        assert!(codes.iter().any(|code| code == SELF_DERIVED_INTENT_GAP));
    }

    #[test]
    fn should_connect_precise_matches_oracle_to_act_result() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_record() {
    // Arrange
    let key = "missing";
    // Act
    let result = load(key);
    // Assert
    assert!(matches!(result, Err(Error::MissingRecord)));
}
"#;

        // Act
        let rules = findings(source);
        let codes = analysis_codes(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
        assert!(!codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_accept_precise_match_with_unexpected_panic_branch() {
        // Arrange
        let source = r#"
#[test]
fn should_reject_missing_record() {
    // Arrange
    let key = "missing";
    // Act
    let result = load(key);
    // Assert
    match result {
        Err(Error::MissingRecord) => {}
        other => panic!("unexpected result: {other:?}"),
    }
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_MISSING));
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_leave_indirect_external_observable_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_publish_record() {
    // Arrange
    let publisher = start_publisher();
    let subscriber = connect_subscriber();
    // Act
    publisher.publish("record");
    // Assert
    assert_eq!(subscriber.receive(), "record");
}
"#;
        let policy = TestPolicyConfig::default();

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert!(tests[0]
            .gaps
            .iter()
            .any(|gap| gap.code == "analysis.indirect-causality"));
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_reject_direct_uncontrolled_inputs() {
        // Arrange
        let source = r#"
#[test]
fn should_build_dynamic_input() {
    // Arrange
    std::env::set_var("MODE", "test");
    // Act
    let value = rand::random::<u64>();
    let observed_at = std::time::Instant::now();
    // Assert
    assert!(value > 0 && observed_at <= std::time::Instant::now());
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ENVIRONMENT));
        assert!(rules.iter().any(|rule| rule == RANDOM));
        assert!(rules.iter().any(|rule| rule == WALL_CLOCK));
    }

    #[test]
    fn should_require_outcome_when_configured_mock_is_only_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_deliver_record() {
    // Arrange
    let mock = make_mock();
    // Act
    deliver(&mock);
    // Assert
    mock.verify();
}
"#;
        let mut policy = TestPolicyConfig::default();
        policy.mock_interaction_methods.push("verify".into());

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert!(evaluation
            .analysis_gaps
            .iter()
            .any(|gap| gap.code == INTERACTION_ONLY_INTENT_GAP));
    }

    #[test]
    fn should_not_let_arrange_helper_uncertainty_hide_missing_oracle() {
        // Arrange
        let source = r#"
#[test]
fn should_run_operation() {
    // Arrange
    assert_fixture_ready(true);
    // Act
    operation();
    // Assert
    log_completion();
}
"#;

        // Act
        let rules = findings(source);
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == "analysis.unresolved-assertion-helper"));
        assert!(rules.iter().any(|rule| rule == ORACLE_MISSING));
    }

    #[test]
    fn should_scope_shared_reference_interior_mutation_as_analysis_gap() {
        // Arrange
        let source = r#"
#[test]
fn should_insert_record() {
    // Arrange
    let database = Database::new();
    // Act
    insert(&database, 42);
    // Assert
    assert_eq!(database.len(), 1);
}
"#;

        // Act
        let rules = findings(source);
        let codes = analysis_codes(source);

        // Assert
        assert!(codes
            .iter()
            .any(|code| code == "analysis.shared-reference-effect-causality"));
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_scope_implicit_format_capture_without_losing_explicit_format_causality() {
        // Arrange
        let implicit = r#"
#[test]
fn should_format_record() {
    // Arrange
    let expected = "created";
    // Act
    let actual = create_record();
    // Assert
    assert_eq!(format!("{actual}"), expected);
}
"#;
        let explicit = r#"
#[test]
fn should_format_record() {
    // Arrange
    let expected = "created";
    // Act
    let actual = create_record();
    // Assert
    assert_eq!(format!("{}", actual), expected);
}
"#;

        // Act
        let implicit_rules = findings(implicit);
        let implicit_codes = analysis_codes(implicit);
        let explicit_rules = findings(explicit);
        let explicit_codes = analysis_codes(explicit);

        // Assert
        assert!(implicit_codes
            .iter()
            .any(|code| code == "analysis.implicit-format-capture"));
        assert!(!implicit_rules
            .iter()
            .any(|rule| rule == ORACLE_DISCONNECTED));
        assert!(!explicit_codes
            .iter()
            .any(|code| code == "analysis.implicit-format-capture"));
        assert!(!explicit_rules
            .iter()
            .any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_not_treat_assert_matches_pattern_binding_as_act_causality() {
        // Arrange
        let source = r#"
#[test]
fn should_match_created_record() {
    // Arrange
    let unrelated = Some(201);
    // Act
    let actual = create_record();
    // Assert
    assert_matches!(unrelated, Some(actual));
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_resolve_direct_and_aliased_imports_for_hygiene_rules() {
        // Arrange
        let source = r#"
use rand::random;
use std::env::set_var;
use std::thread::sleep as delay;
use std::time::{Duration, Instant};

#[test]
fn should_build_dynamic_input() {
    // Arrange
    set_var("MODE", "test");
    // Act
    let value = random::<u64>();
    let observed_at = Instant::now();
    delay(Duration::from_millis(1));
    // Assert
    assert!(value > 0 && observed_at <= Instant::now());
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ENVIRONMENT));
        assert!(rules.iter().any(|rule| rule == RANDOM));
        assert!(rules.iter().any(|rule| rule == WALL_CLOCK));
        assert!(rules.iter().any(|rule| rule == FIXED_SLEEP));
    }

    #[test]
    fn should_not_expand_shadowed_import_and_should_gap_glob_hygiene() {
        // Arrange
        let shadowed = r#"
use std::thread::sleep;
use std::time::Duration;

#[test]
fn should_use_local_delay() {
    // Arrange
    let sleep = |_: Duration| {};
    // Act
    let actual = operation();
    sleep(Duration::from_millis(1));
    // Assert
    assert_eq!(actual, 42);
}
"#;
        let glob = r#"
use std::thread::*;
use std::time::Duration;

#[test]
fn should_wait_for_ready() {
    // Arrange
    let expected = true;
    // Act
    sleep(Duration::from_millis(1));
    let actual = service_is_ready();
    // Assert
    assert_eq!(actual, expected);
}
"#;
        let rebound = r#"
use std::time::Duration;

#[test]
fn should_wait_for_ready() {
    // Arrange
    let sleep = std::thread::sleep;
    // Act
    sleep(Duration::from_millis(1));
    let actual = service_is_ready();
    // Assert
    assert!(actual);
}
"#;
        let block_import = r#"
use std::time::Duration;

#[test]
fn should_wait_for_ready() {
    // Arrange
    let expected = true;
    // Act
    {
        use std::thread::sleep;
        sleep(Duration::from_millis(1));
    }
    let actual = service_is_ready();
    // Assert
    assert_eq!(actual, expected);
}
"#;
        let parameter_shadow = r#"
use std::thread::sleep;
use std::time::Duration;

#[rstest]
fn should_use_injected_delay(sleep: fn(Duration)) {
    // Arrange
    let expected = 42;
    // Act
    sleep(Duration::from_millis(1));
    let actual = operation();
    // Assert
    assert_eq!(actual, expected);
}
"#;
        let nested_type_shadow = r#"
use std::time::Instant;

mod nested {
    struct Instant;

    impl Instant {
        fn now() -> u64 { 42 }
    }

    #[test]
    fn should_use_domain_instant() {
        // Arrange
        let expected = 42;
        // Act
        let actual = Instant::now();
        // Assert
        assert_eq!(actual, expected);
    }
}
"#;
        let type_alias = r#"
type BaseClock = std::time::Instant;
type Clock = BaseClock;

#[test]
fn should_observe_runtime_clock() {
    // Arrange
    let expected = std::time::Instant::now();
    // Act
    let actual = Clock::now();
    // Assert
    assert!(actual >= expected);
}
"#;

        // Act
        let shadowed_rules = findings(shadowed);
        let glob_rules = findings(glob);
        let glob_codes = analysis_codes(glob);
        let rebound_rules = findings(rebound);
        let block_import_rules = findings(block_import);
        let parameter_shadow_rules = findings(parameter_shadow);
        let nested_type_shadow_rules = findings(nested_type_shadow);
        let type_alias_rules = findings(type_alias);

        // Assert
        assert!(!shadowed_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(!glob_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(glob_codes
            .iter()
            .any(|code| code == "analysis.unresolved-glob-import"));
        assert!(rebound_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(block_import_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(!parameter_shadow_rules
            .iter()
            .any(|rule| rule == FIXED_SLEEP));
        assert!(!nested_type_shadow_rules
            .iter()
            .any(|rule| rule == WALL_CLOCK));
        assert!(type_alias_rules.iter().any(|rule| rule == WALL_CLOCK));
    }

    #[test]
    fn should_respect_pattern_and_block_item_shadowing_of_imported_hygiene_api() {
        // Arrange
        let source = r#"
use std::thread::sleep;
use std::time::Duration;

#[test]
fn should_use_injected_delays() {
    // Arrange
    let local_delay = |_: Duration| {};
    let mut delays = vec![local_delay as fn(Duration)].into_iter();
    // Act
    (|sleep: fn(Duration)| sleep(Duration::from_millis(1)))(local_delay);
    for sleep in [local_delay as fn(Duration)] {
        sleep(Duration::from_millis(1));
    }
    match Some(local_delay as fn(Duration)) {
        Some(sleep) => sleep(Duration::from_millis(1)),
        None => {}
    }
    if let Some(sleep) = Some(local_delay as fn(Duration)) {
        sleep(Duration::from_millis(1));
    }
    while let Some(sleep) = delays.next() {
        sleep(Duration::from_millis(1));
    }
    {
        fn sleep(_: Duration) {}
        sleep(Duration::from_millis(1));
    }
    let actual = operation();
    // Assert
    assert_eq!(actual, 42);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == FIXED_SLEEP));
    }

    #[test]
    fn should_accept_literal_sleep_after_tokio_virtual_time_is_paused() {
        // Arrange
        let dynamic_pause = r#"
#[tokio::test]
async fn should_advance_virtual_time() {
    // Arrange
    tokio::time::pause();
    // Act
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let actual = operation();
    // Assert
    assert_eq!(actual, 42);
}
"#;
        let paused_alias = r#"
use tokio::test as async_test;

#[async_test(start_paused = true)]
async fn should_advance_virtual_time() {
    // Arrange
    let expected = 42;
    // Act
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let observed_at = tokio::time::Instant::now();
    let actual = operation();
    // Assert
    assert!(observed_at <= tokio::time::Instant::now());
    assert_eq!(actual, expected);
}
"#;
        let unpaused = r#"
#[tokio::test]
async fn should_observe_runtime_time() {
    // Arrange
    let expected = 42;
    // Act
    let observed_at = tokio::time::Instant::now();
    let actual = operation();
    // Assert
    assert!(observed_at <= tokio::time::Instant::now());
    assert_eq!(actual, expected);
}
"#;

        // Act
        let dynamic_pause_rules = findings(dynamic_pause);
        let paused_alias_rules = findings(paused_alias);
        let unpaused_rules = findings(unpaused);

        // Assert
        assert!(!dynamic_pause_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(!paused_alias_rules.iter().any(|rule| rule == FIXED_SLEEP));
        assert!(!paused_alias_rules.iter().any(|rule| rule == WALL_CLOCK));
        assert!(unpaused_rules.iter().any(|rule| rule == WALL_CLOCK));
    }

    #[test]
    fn should_find_nested_broad_error_check_and_follow_extracted_payload() {
        // Arrange
        let broad = r#"
#[test]
fn should_reject_missing_record() {
    // Arrange
    let attempts = 1;
    // Act
    let result = load_record();
    // Assert
    assert!(result.is_err() && attempts == 1);
}
"#;
        let inspected = r#"
#[test]
fn should_reject_missing_record() {
    // Arrange
    let expected_code = "missing";
    // Act
    let result = load_record();
    // Assert
    assert!(result.is_err());
    let error = result.unwrap_err();
    assert_eq!(error.code(), expected_code);
}
"#;

        // Act
        let broad_codes = analysis_codes(broad);
        let inspected_codes = analysis_codes(inspected);

        // Assert
        assert!(broad_codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
        assert!(!inspected_codes
            .iter()
            .any(|code| code == ERROR_UNINSPECTED_INTENT_GAP));
    }

    #[test]
    fn should_reject_constant_true_disjunction() {
        // Arrange
        let source = r#"
#[test]
fn should_create_record() {
    // Arrange
    let expected = 201;
    // Act
    let actual = create_record();
    // Assert
    assert!(true || actual == expected);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(rules.iter().any(|rule| rule == ORACLE_VACUOUS));
    }

    #[test]
    fn should_treat_non_unit_test_return_as_harness_oracle() {
        // Arrange
        let source = r#"
use std::process::ExitCode;

#[test]
fn should_report_command_success() -> ExitCode {
    run_command()
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_MISSING));
    }

    #[test]
    fn should_follow_mutable_reference_alias_as_act_effect() {
        // Arrange
        let source = r#"
#[test]
fn should_append_value() {
    // Arrange
    let mut state = Vec::new();
    let target = &mut state;
    // Act
    append_value(target);
    // Assert
    assert_eq!(state.len(), 1);
}
"#;

        // Act
        let rules = findings(source);

        // Assert
        assert!(!rules.iter().any(|rule| rule == ORACLE_DISCONNECTED));
    }

    #[test]
    fn should_gap_unresolved_operator_and_markerless_causality() {
        // Arrange
        let operator = r#"
#[test]
fn should_add_counters() {
    // Arrange
    let left = Counter(1);
    let right = Counter(2);
    // Act
    let actual = left + right;
    // Assert
    assert_eq!(actual.0, 3);
}
"#;
        let markerless = r#"
#[test]
fn should_create_record() {
    let _actual = create_record();
    assert_eq!(2 + 2, 4);
}
"#;

        // Act
        let operator_rules = findings(operator);
        let operator_codes = analysis_codes(operator);
        let markerless_codes = analysis_codes(markerless);

        // Assert
        assert!(operator_codes
            .iter()
            .any(|code| code == "analysis.unresolved-act-computation"));
        assert!(!operator_rules
            .iter()
            .any(|rule| rule == ORACLE_DISCONNECTED));
        assert!(markerless_codes
            .iter()
            .any(|code| code == "analysis.ambiguous-markerless-causality"));
    }

    #[test]
    fn should_apply_only_exact_reasoned_exemption() {
        // Arrange
        let source = r#"
#[test]
fn reports_anything() {
    assert!(true);
}
"#;
        let mut policy = TestPolicyConfig::default();
        policy.exemptions.push(TestExemption {
            rule_id: NAMING_SHOULD_PREFIX.into(),
            path: "src/lib.rs".into(),
            test: "reports_anything".into(),
            reason: "legacy externally referenced test name".into(),
        });

        // Act
        let tests = analyze_source("src/lib.rs", source, &policy).expect("analyze Rust test");
        let evaluation = evaluate(&tests, &policy);

        // Assert
        assert_eq!(evaluation.exemptions.len(), 1);
        assert!(!evaluation
            .findings
            .iter()
            .any(|finding| finding.rule_id == NAMING_SHOULD_PREFIX));
    }
}
